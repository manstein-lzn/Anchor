"""Public scholarly retrieval with pinned DNS and durable tool evidence.

Network access is confined to this read-only adapter, never granted to the
subprocess sandbox. Every redirect is validated and every connection uses the
validated IP address while retaining the original TLS hostname.
"""

from __future__ import annotations

import logging
from contextlib import contextmanager
from typing import Any

from datetime import datetime, timezone
from concurrent.futures import ThreadPoolExecutor
from io import BytesIO
import ipaddress
import json
import re
import socket
import threading
import time
from email.utils import parsedate_to_datetime
from urllib.parse import urlencode, urljoin, urlsplit, urlunsplit

from pydantic import Field

from anchor.domain.models import DomainModel


MAX_RESPONSE_BYTES = 8_000_000
RESEARCH_TOOLS = frozenset({"scholarly.search", "scholarly.read", "scholarly.read_many",
                           "scholarly.citations"})
# Documents per batch read. Bounded so one call cannot monopolise a round.
BATCH_READ_LIMIT = 8
_ARXIV_LOCK = threading.Lock()
_ARXIV_LAST_REQUEST = 0.0
_CROSSREF_LOCK = threading.Lock()
_CROSSREF_LAST_REQUEST = 0.0
_HOST_STATE_LOCK = threading.Lock()
_HOST_REQUEST_LOCKS: dict[str, threading.Lock] = {}
_HOST_BLOCKED_UNTIL: dict[str, float] = {}


class ResearchToolError(ValueError):
    """A retrieval failure with a stable, user-visible error category."""

    def __init__(self, code: str, message: str, *, retryable: bool = False) -> None:
        self.code = code
        self.retryable = retryable
        super().__init__(message)


def pace_arxiv(hostname: str) -> None:
    global _ARXIV_LAST_REQUEST
    if hostname not in {"arxiv.org", "export.arxiv.org"}:
        return
    with _ARXIV_LOCK:
        delay = max(0, 3 - (time.monotonic() - _ARXIV_LAST_REQUEST))
        if delay:
            time.sleep(delay)
        _ARXIV_LAST_REQUEST = time.monotonic()


def pace_crossref(hostname: str) -> None:
    global _CROSSREF_LAST_REQUEST
    if hostname != "api.crossref.org":
        return
    with _CROSSREF_LOCK:
        delay = max(0, 1 - (time.monotonic() - _CROSSREF_LAST_REQUEST))
        if delay:
            time.sleep(delay)
        _CROSSREF_LAST_REQUEST = time.monotonic()


def _request_lock(hostname: str) -> threading.Lock:
    with _HOST_STATE_LOCK:
        return _HOST_REQUEST_LOCKS.setdefault(hostname, threading.Lock())


def _retry_after(value: str | None, default: float) -> float:
    if value:
        try:
            return min(30.0, max(0.0, float(value)))
        except ValueError:
            try:
                delay = (parsedate_to_datetime(value) - datetime.now(timezone.utc)).total_seconds()
                return min(30.0, max(0.0, delay))
            except (TypeError, ValueError, OverflowError):
                pass
    return default


def _normalize_source_url(value: str) -> str:
    parsed = urlsplit(value)
    hostname = (parsed.hostname or "").lower()
    if hostname == "xplorestaging.ieee.org":
        hostname = "ieeexplore.ieee.org"
    netloc = hostname
    if parsed.port and parsed.port != 443:
        netloc += f":{parsed.port}"
    return urlunsplit(("https" if parsed.scheme in {"http", "https"} else parsed.scheme,
                       netloc or parsed.netloc, parsed.path, parsed.query, parsed.fragment))


logger = logging.getLogger("anchor.research_tools")


class ResearchRequest(DomainModel):
    query: str | None = Field(default=None, min_length=1, max_length=1000)
    url: str | None = Field(default=None, min_length=1, max_length=3000)
    source: str = Field(default="crossref", pattern="^(crossref|arxiv)$")
    limit: int = Field(default=8, ge=1, le=20)
    offset: int = Field(default=0, ge=0)
    page_start: int = Field(default=0, ge=0)
    # Citation chasing: follow the graph a scholar follows, backwards (what a
    # paper cites) and forwards (what cites it).
    identifier: str | None = Field(default=None, min_length=1, max_length=300)
    direction: str = Field(default="cited_by", pattern="^(cites|cited_by)$")
    # Batch reads: fetching a group in one call keeps the model out of the I/O
    # loop, which is where a research campaign spends most of its wall clock.
    urls: list[str] | None = Field(default=None, max_length=BATCH_READ_LIMIT)


#: The cache a fetch may consult, and the context that scopes its keys. Set by the tool gateway
#: around a tool call, because `fetch_public` is per-URL and knows nothing about which run,
#: which graph version or which task asked — and a key that omitted those would serve one task's
#: content to another. A module-level holder rather than a threaded parameter because the fetch
#: call sits behind retry and redirect logic that has no business carrying a cache key.
_CACHE: Any = None
_CACHE_CONTEXT: tuple[str, str] | None = None


@contextmanager
def content_cache_scope(cache: Any, *, graph_version_id: str, scope: str):
    """Make ``cache`` available to fetches made inside this block, keyed by that context.

    The graph version and the task scope are part of the key because the *decision* to fetch
    belongs to a version's policy and a task's purpose, even though the bytes at a URL do not.
    """
    global _CACHE, _CACHE_CONTEXT
    previous = (_CACHE, _CACHE_CONTEXT)
    _CACHE, _CACHE_CONTEXT = cache, (graph_version_id, scope)
    try:
        yield cache
    finally:
        _CACHE, _CACHE_CONTEXT = previous


def _cached_fetch(url: str, fetch: Any) -> tuple[str, str, bytes]:
    """Consult the cache, fetch if it does not have it, and store what came back.

    Returns ``(content_type, final_url, body)`` so the caller cannot tell a hit from a fetch —
    the point is to avoid the request, not to change the result. Every non-hit outcome falls
    through to the real fetch, including ``corrupt``: a damaged entry is reported and then
    replaced, rather than being returned or left in place.
    """
    cache, context = _CACHE, _CACHE_CONTEXT
    if cache is None or context is None:
        return fetch()
    from anchor.runtime.content_cache import CacheKey

    key = CacheKey(url=url, graph_version_id=context[0], scope=context[1])
    lookup = cache.get(key)
    if lookup.hit and lookup.entry is not None:
        return lookup.entry.content_type, lookup.entry.final_url, lookup.entry.body
    content_type, final_url, body = fetch()
    try:
        cache.put(key, content_type=content_type, final_url=final_url, body=body,
                  replacement=lookup.outcome == "corrupt")
    except FileExistsError:
        # Another process wrote it between the lookup and now. Its bytes are as good as ours.
        pass
    except OSError as exc:
        # A cache that cannot be written must not fail a fetch that already succeeded.
        logger.warning("could not store a content cache entry for %s: %s", url, exc)
    return content_type, final_url, body


def public_address(hostname: str, port: int) -> str:
    addresses = {item[4][0] for item in socket.getaddrinfo(
        hostname, port, type=socket.SOCK_STREAM)}
    if not addresses or any(not ipaddress.ip_address(value).is_global
                            or ipaddress.ip_address(value).is_multicast for value in addresses):
        raise ValueError("research URLs must resolve exclusively to public Internet addresses")
    return sorted(addresses, key=lambda address: (":" in address, address))[0]


def fetch_public(url: str, *, timeout_seconds: float = 30) -> tuple[str, str, bytes]:
    import urllib3

    for _ in range(6):
        parsed = urlsplit(url)
        if (parsed.scheme != "https" or not parsed.hostname or parsed.username
                or parsed.password or parsed.port not in (None, 443)):
            raise ValueError("research URLs must be public HTTPS URLs without credentials or custom ports")
        address = public_address(parsed.hostname, 443)
        path = parsed.path or "/"
        if parsed.query:
            path += "?" + parsed.query
        with _request_lock(parsed.hostname):
            blocked = _HOST_BLOCKED_UNTIL.get(parsed.hostname, 0) - time.monotonic()
            if blocked > 0:
                raise ResearchToolError("source_rate_limited",
                    f"source cooldown is active for another {blocked:.1f} seconds", retryable=True)
            for request_attempt in range(2):
                pace_arxiv(parsed.hostname)
                pace_crossref(parsed.hostname)
                pool = urllib3.HTTPSConnectionPool(
                    address, port=443, server_hostname=parsed.hostname,
                    assert_hostname=parsed.hostname, cert_reqs="CERT_REQUIRED",
                    timeout=urllib3.Timeout(connect=min(10, timeout_seconds), read=timeout_seconds),
                )
                response = None
                try:
                    try:
                        response = pool.request("GET", path, headers={
                            "Host": parsed.hostname,
                            "User-Agent": "AnchorAcademicResearch/0.1 (read-only literature research)",
                            "Accept": "application/json, application/atom+xml, text/html, application/pdf, text/plain",
                            "Accept-Encoding": "identity",
                        }, redirect=False, retries=False, preload_content=False)
                    except Exception as exc:
                        code = "source_timeout" if "timed out" in str(exc).lower() else "source_unavailable"
                        raise ResearchToolError(code, str(exc), retryable=True) from exc
                    if response.status in (429, 500, 502, 503, 504):
                        delay = _retry_after(response.headers.get("Retry-After"),
                                             3.0 if response.status == 429 else 2.0)
                        if request_attempt == 0:
                            time.sleep(delay)
                            continue
                        cooldown = max(delay, 30.0 if response.status == 429 else 5.0)
                        _HOST_BLOCKED_UNTIL[parsed.hostname] = time.monotonic() + cooldown
                        code = "source_rate_limited" if response.status == 429 else "source_unavailable"
                        raise ResearchToolError(code, f"source returned HTTP {response.status}", retryable=True)
                    if response.status in (301, 302, 303, 307, 308):
                        location = response.headers.get("Location")
                        if not location:
                            raise ResearchToolError("invalid_redirect", "redirect has no destination")
                        url = urljoin(url, location)
                        break
                    if response.status in (401, 403):
                        raise ResearchToolError("source_access_denied",
                                                f"source returned HTTP {response.status}")
                    if response.status == 404:
                        raise ResearchToolError("source_not_found", "source returned HTTP 404")
                    if response.status != 200:
                        raise ResearchToolError("source_http_error",
                                                f"source returned HTTP {response.status}")
                    body = response.read(MAX_RESPONSE_BYTES + 1)
                    if len(body) > MAX_RESPONSE_BYTES:
                        raise ResearchToolError("source_too_large",
                                                "source exceeds the per-document download size limit")
                    return url, response.headers.get("Content-Type", "").lower(), body
                finally:
                    if response is not None:
                        response.close()
                    pool.close()
            else:  # pragma: no cover - loop always returns, raises or redirects
                continue
        # A validated redirect updates ``url`` and starts a fresh host-scoped request.
        continue
    raise ValueError("source exceeded the redirect limit")


def _plain_markup(value: str) -> str:
    from lxml import html

    return html.fromstring(value).text_content().strip() if value.strip() else ""


def search(request: ResearchRequest, *, timeout_seconds: float) -> dict:
    if not request.query:
        raise ValueError("scholarly.search requires query")
    if request.source == "crossref":
        url = "https://api.crossref.org/works?" + urlencode({
            "query.bibliographic": request.query, "rows": request.limit,
        })
        final_url, _, body = _cached_fetch(
            url, lambda: fetch_public(url, timeout_seconds=timeout_seconds))
        data = json.loads(body)["message"]
        papers = []
        for item in data.get("items", []):
            date = item.get("published", {}).get("date-parts", [[]])[0]
            papers.append({
                "id": "doi:" + item["DOI"], "doi": item["DOI"],
                "title": " ".join(item.get("title", [])),
                "authors": [" ".join(filter(None, (author.get("given"), author.get("family"))))
                            for author in item.get("author", [])],
                "year": date[0] if date else None,
                "venue": " ".join(item.get("container-title", [])),
                "publication_type": item.get("type"),
                "url": _normalize_source_url(item.get("URL", "https://doi.org/" + item["DOI"])),
                "abstract": _plain_markup(item.get("abstract", "")),
                "fulltext_urls": [_normalize_source_url(link["URL"])
                                  for link in item.get("link", []) if "URL" in link],
                "evidence_level": "abstract" if item.get("abstract") else "metadata_only",
            })
        return {"source": "crossref", "query": request.query, "request_url": final_url,
                "total_results": data.get("total-results"), "papers": papers}

    from defusedxml import ElementTree

    url = "https://export.arxiv.org/api/query?" + urlencode({
        "search_query": request.query, "start": 0, "max_results": request.limit,
        "sortBy": "relevance",
    })
    final_url, _, body = fetch_public(url, timeout_seconds=timeout_seconds)
    root = ElementTree.fromstring(body)
    ns = {"a": "http://www.w3.org/2005/Atom"}
    papers = []
    for entry in root.findall("a:entry", ns):
        paper_url = entry.findtext("a:id", "", ns).replace("http://", "https://", 1)
        papers.append({
            "id": paper_url, "url": paper_url,
            "title": " ".join(entry.findtext("a:title", "", ns).split()),
            "authors": [a.findtext("a:name", "", ns) for a in entry.findall("a:author", ns)],
            "year": entry.findtext("a:published", "", ns)[:4],
            "abstract": " ".join(entry.findtext("a:summary", "", ns).split()),
            "publication_type": "preprint", "evidence_level": "abstract",
            "fulltext_urls": [link.attrib["href"].replace("http://", "https://", 1)
                              for link in entry.findall("a:link", ns)
                              if link.attrib.get("title") == "pdf"],
        })
    return {"source": "arxiv", "query": request.query, "request_url": final_url, "papers": papers}


def _fetch_document(url: str, *, offset: int, page_start: int, timeout_seconds: float) -> dict:
    """Fetch one document and extract a bounded excerpt.

    Shared by the single and batch read paths so their provenance, HTML
    preference and extraction behave identically.
    """
    if re.match(r"https?://(?:www\.)?arxiv\.org/abs/", url):
        raise ResearchToolError(
            "abstract_page",
            "arXiv /abs/ is the abstract page, not the paper; read "
            "https://arxiv.org/pdf/<id> for the full text")
    # Ask for the arXiv HTML rendering first: it is smaller and cleaner than the
    # PDF, and going straight to it costs one request instead of two. Each source
    # request is paced, so a wasted probe doubles the wall clock of a batch.
    if re.match(r"https?://(?:www\.)?arxiv\.org/pdf/", url):
        html_url = re.sub(r"/pdf/", "/html/", url).removesuffix(".pdf")
        try:
            final, content_type, body = _cached_fetch(
                html_url, lambda: fetch_public(html_url, timeout_seconds=timeout_seconds))
        except ResearchToolError:
            final, content_type, body = _cached_fetch(
                url, lambda: fetch_public(url, timeout_seconds=timeout_seconds))
    else:
        final, content_type, body = fetch_public(url, timeout_seconds=timeout_seconds)
    page_count = None
    if "application/pdf" in content_type or body.startswith(b"%PDF-"):
        from pypdf import PdfReader

        pdf = PdfReader(BytesIO(body))
        page_count = len(pdf.pages)
        if page_start >= page_count:
            raise ValueError("page_start is beyond the document")
        text = "\n\n".join(page.extract_text() or ""
                           for page in pdf.pages[page_start:page_start + 40])
        title = (pdf.metadata.title if pdf.metadata else None) or ""
        truncated = page_count > page_start + 40
    elif "html" in content_type or body.lstrip().startswith((b"<!", b"<html")):
        import trafilatura

        text = trafilatura.extract(body, url=final, include_tables=True, include_links=True) or ""
        metadata = trafilatura.extract_metadata(body, default_url=final)
        title = metadata.title if metadata else ""
        truncated = False
    elif content_type.startswith("text/plain"):
        text, title, truncated = body.decode("utf-8", "replace"), "", False
    else:
        raise ValueError("source is not a supported HTML, PDF, or text document")
    if len(text.strip()) < 100:
        raise ValueError("source contains no extractable article text; it may require access or OCR")
    if offset >= len(text):
        raise ValueError("offset is beyond the extracted document text")
    excerpt = text[offset:offset + 24000]
    next_offset = offset + len(excerpt) if offset + len(excerpt) < len(text) else None
    return {"url": final, "requested_url": url, "title": title,
            "content_type": content_type, "text": excerpt,
            "truncated": (truncated or next_offset is not None or offset > 0 or page_start > 0),
            "pages": page_count, "page_start": page_start, "offset": offset,
            "next_offset": next_offset,
            **({"next_page_start": page_start + 40} if truncated else {})}


def read_many(request: ResearchRequest, *, timeout_seconds: float) -> dict:
    """Read several documents in one call, one bounded excerpt per document.

    The adapter still paces each source, so this does not evade a rate limit; it
    removes the model from the inner I/O loop. One model turn can now cover a
    group of papers instead of one paper per turn.
    """
    urls = list(request.urls or [])
    if not urls:
        raise ValueError("scholarly.read_many requires urls")
    documents: list[dict] = []
    with ThreadPoolExecutor(max_workers=min(len(urls), BATCH_READ_LIMIT)) as pool:
        futures = {
            pool.submit(_fetch_document, url, offset=0, page_start=0,
                        timeout_seconds=timeout_seconds): url
            for url in urls
        }
        for future in futures:
            url = futures[future]
            try:
                documents.append(future.result())
            except Exception as exc:  # noqa: BLE001 - one bad document must not fail the batch
                documents.append({"requested_url": url, "url": None, "title": "",
                                  "content_type": None, "text": "",
                                  "error": f"{type(exc).__name__}: {str(exc)[:200]}",
                                  "evidence_available": False})
    documents.sort(key=lambda item: urls.index(item["requested_url"]))
    return {"requested_urls": urls, "documents": documents,
            "retrieved": sum(1 for item in documents if item.get("text"))}


def read(request: ResearchRequest, *, timeout_seconds: float) -> dict:
    if not request.url:
        raise ValueError("scholarly.read requires url")
    return _fetch_document(request.url, offset=request.offset,
                           page_start=request.page_start,
                           timeout_seconds=timeout_seconds)


def _openalex_abstract(item: dict) -> str:
    """OpenAlex stores abstracts as an inverted index; rebuild the prose."""
    index = item.get("abstract_inverted_index") or {}
    if not isinstance(index, dict):
        return ""
    positions: dict[int, str] = {}
    for word, offsets in index.items():
        for offset in offsets or []:
            positions[offset] = word
    return " ".join(positions[key] for key in sorted(positions))


def _openalex_paper(item: dict) -> dict:
    doi = (item.get("doi") or "").removeprefix("https://doi.org/")
    abstract = _openalex_abstract(item)
    # Every location, not just the best open-access one: a paper is routinely
    # available as a publisher record and as an arXiv preprint, and a reader that
    # studies the preprint is reading the same work. Recording only one URL made
    # that read look like a provenance mismatch.
    urls: list[str] = []
    for location in [item.get("best_oa_location"), *(item.get("locations") or [])]:
        if not isinstance(location, dict):
            continue
        for url in (location.get("pdf_url"), location.get("landing_page_url")):
            if url and url not in urls:
                urls.append(url)
    return {
        "id": ("doi:" + doi) if doi else "openalex:" + str(item.get("id", "")).rsplit("/", 1)[-1],
        "doi": doi or None,
        "title": item.get("title") or "",
        "authors": [entry["author"]["display_name"] for entry in item.get("authorships", [])
                    if entry.get("author", {}).get("display_name")],
        "year": item.get("publication_year"),
        "venue": ((item.get("primary_location") or {}).get("source") or {}).get("display_name") or "",
        "publication_type": item.get("type"),
        "url": ("https://doi.org/" + doi) if doi else item.get("id"),
        "abstract": abstract,
        "fulltext_urls": [_normalize_source_url(url) for url in urls],
        "evidence_level": "abstract" if abstract else "metadata_only",
        "cited_by_count": item.get("cited_by_count"),
    }


def _openalex_work(identifier: str, *, timeout_seconds: float) -> dict:
    if identifier.startswith("openalex:"):
        url = "https://api.openalex.org/works/" + identifier.removeprefix("openalex:")
    elif identifier.startswith("doi:"):
        url = "https://api.openalex.org/works/doi:" + identifier.removeprefix("doi:")
    elif identifier.startswith("arxiv:"):
        url = "https://api.openalex.org/works/doi:10.48550/arXiv." + identifier.removeprefix("arxiv:")
    else:
        raise ResearchToolError("unsupported_identifier",
                                "identifier must be doi:<doi>, arxiv:<id> or openalex:<work id>")
    _, _, body = fetch_public(url, timeout_seconds=timeout_seconds)
    return json.loads(body)


def citations(request: ResearchRequest, *, timeout_seconds: float) -> dict:
    """Follow the citation graph from a seed paper, in either direction."""
    if not request.identifier:
        raise ValueError("scholarly.citations requires identifier")
    seed = _openalex_work(request.identifier, timeout_seconds=timeout_seconds)
    seed_id = str(seed.get("id", "")).rsplit("/", 1)[-1]
    if request.direction == "cited_by":
        url = "https://api.openalex.org/works?" + urlencode({
            "filter": f"cites:{seed_id}", "per-page": request.limit,
            "sort": "cited_by_count:desc"})
        _, _, body = fetch_public(url, timeout_seconds=timeout_seconds)
        data = json.loads(body)
        papers = [_openalex_paper(item) for item in data.get("results", [])]
        total = data.get("meta", {}).get("count")
    else:
        referenced = [str(item).rsplit("/", 1)[-1] for item in (seed.get("referenced_works") or [])]
        papers = []
        total = len(referenced)
        for start in range(0, min(len(referenced), 200), 50):
            chunk = referenced[start:start + 50]
            url = "https://api.openalex.org/works?" + urlencode({
                "filter": "openalex_id:" + "|".join(chunk), "per-page": 50})
            _, _, body = fetch_public(url, timeout_seconds=timeout_seconds)
            papers.extend(_openalex_paper(item) for item in json.loads(body).get("results", []))
        papers = papers[:request.limit]
    return {"source": "openalex", "identifier": request.identifier,
            "direction": request.direction, "total_results": total, "papers": papers}


def execute_research(tool_ref: str, request: ResearchRequest, *, timeout_seconds: float) -> str:
    if tool_ref == "scholarly.search":
        result = search(request, timeout_seconds=timeout_seconds)
    elif tool_ref == "scholarly.read_many":
        result = read_many(request, timeout_seconds=timeout_seconds)
    elif tool_ref == "scholarly.citations":
        result = citations(request, timeout_seconds=timeout_seconds)
    else:
        result = read(request, timeout_seconds=timeout_seconds)
    result["retrieved_at"] = datetime.now(timezone.utc).isoformat()
    result["untrusted_source_content"] = True
    return json.dumps(result, ensure_ascii=False)
