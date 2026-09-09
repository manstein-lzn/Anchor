"""Public scholarly retrieval with pinned DNS and durable tool evidence.

Network access is confined to this read-only adapter, never granted to the
subprocess sandbox. Every redirect is validated and every connection uses the
validated IP address while retaining the original TLS hostname.
"""

from __future__ import annotations

from datetime import datetime, timezone
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
RESEARCH_TOOLS = frozenset({"scholarly.search", "scholarly.read", "scholarly.citations"})
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
        final_url, _, body = fetch_public(url, timeout_seconds=timeout_seconds)
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


def read(request: ResearchRequest, *, timeout_seconds: float) -> dict:
    if not request.url:
        raise ValueError("scholarly.read requires url")
    # An arXiv /abs/ page is the abstract, which the search result already
    # contains. Reading it spends the read budget without adding evidence, so
    # the adapter refuses it and points at the full text instead.
    if re.match(r"https?://(?:www\.)?arxiv\.org/abs/", request.url):
        raise ResearchToolError(
            "abstract_page",
            "arXiv /abs/ is the abstract page, not the paper; read "
            "https://arxiv.org/pdf/<id> for the full text")
    url, content_type, body = fetch_public(request.url, timeout_seconds=timeout_seconds)
    page_count = None
    if "application/pdf" in content_type or body.startswith(b"%PDF-"):
        from pypdf import PdfReader

        pdf = PdfReader(BytesIO(body))
        page_count = len(pdf.pages)
        if request.page_start >= page_count:
            raise ValueError("page_start is beyond the document")
        text = "\n\n".join(page.extract_text() or "" for page in pdf.pages[request.page_start:request.page_start + 40])
        title = (pdf.metadata.title if pdf.metadata else None) or ""
        truncated = page_count > request.page_start + 40
    elif "html" in content_type or body.lstrip().startswith((b"<!", b"<html")):
        import trafilatura

        text = trafilatura.extract(body, url=url, include_tables=True, include_links=True) or ""
        metadata = trafilatura.extract_metadata(body, default_url=url)
        title = metadata.title if metadata else ""
        truncated = False
    elif content_type.startswith("text/plain"):
        text, title, truncated = body.decode("utf-8", "replace"), "", False
    else:
        raise ValueError("source is not a supported HTML, PDF, or text document")
    if len(text.strip()) < 100:
        raise ValueError("source contains no extractable article text; it may require access or OCR")
    if request.offset >= len(text):
        raise ValueError("offset is beyond the extracted document text")
    excerpt = text[request.offset:request.offset + 24000]
    next_offset = request.offset + len(excerpt) if request.offset + len(excerpt) < len(text) else None
    return {"url": url, "requested_url": request.url, "title": title,
            "content_type": content_type, "text": excerpt,
            "truncated": truncated or next_offset is not None or request.offset > 0 or request.page_start > 0,
            "pages": page_count, "page_start": request.page_start, "offset": request.offset,
            "next_offset": next_offset,
            "next_page_start": request.page_start + 40 if truncated and page_count else None,
            "evidence_level": "retrieved_text", "extracted_characters": len(text),
            "caution": "Retrieved text may be a landing page, abstract, or partial full text. Inspect it before making claims."}


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
    location = item.get("best_oa_location") or {}
    urls = [url for url in (location.get("pdf_url"), location.get("landing_page_url")) if url]
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
    elif tool_ref == "scholarly.citations":
        result = citations(request, timeout_seconds=timeout_seconds)
    else:
        result = read(request, timeout_seconds=timeout_seconds)
    result["retrieved_at"] = datetime.now(timezone.utc).isoformat()
    result["untrusted_source_content"] = True
    return json.dumps(result, ensure_ascii=False)
