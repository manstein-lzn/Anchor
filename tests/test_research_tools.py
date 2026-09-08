import json
from unittest.mock import Mock

import pytest

pytest.importorskip("trafilatura")

from anchor.runtime import research_tools as research


@pytest.mark.parametrize("address", ["127.0.0.1", "10.0.0.2", "169.254.169.254", "::1", "192.168.1.1", "224.0.0.1"])
def test_private_dns_is_denied(monkeypatch, address):
    monkeypatch.setattr(research.socket, "getaddrinfo", lambda *a, **k: [(0, 0, 0, "", (address, 443))])
    with pytest.raises(ValueError, match="public Internet"):
        research.public_address("paper.example", 443)


@pytest.mark.parametrize("url", ["http://example.com/paper", "file:///etc/passwd", "https://user:password@example.com/", "https://example.com:8080/"])
def test_unsafe_urls_are_denied_before_network(url):
    with pytest.raises(ValueError, match="public HTTPS"):
        research.fetch_public(url)


def test_dns_is_pinned_with_original_tls_name_and_redirects_revalidated(monkeypatch):
    import urllib3

    addresses = []
    def resolve(host, port):
        addresses.append(host)
        if host == "internal.example":
            raise ValueError("private destination")
        return "93.184.216.34"
    monkeypatch.setattr(research, "public_address", resolve)
    response = Mock(status=302, headers={"Location": "https://internal.example/metadata"})
    pool = Mock()
    pool.request.return_value = response
    constructor = Mock(return_value=pool)
    monkeypatch.setattr(urllib3, "HTTPSConnectionPool", constructor)
    with pytest.raises(ValueError, match="private destination"):
        research.fetch_public("https://paper.example/article")
    assert constructor.call_args.args[0] == "93.184.216.34"
    assert constructor.call_args.kwargs["server_hostname"] == "paper.example"
    assert constructor.call_args.kwargs["assert_hostname"] == "paper.example"
    assert pool.request.call_args.kwargs["headers"]["Host"] == "paper.example"
    assert addresses == ["paper.example", "internal.example"]
    response.close.assert_called_once()


def test_download_size_is_bounded(monkeypatch):
    import urllib3

    monkeypatch.setattr(research, "public_address", lambda *a: "93.184.216.34")
    monkeypatch.setattr(research, "MAX_RESPONSE_BYTES", 10)
    response = Mock(status=200, headers={"Content-Type": "text/plain"})
    response.read.return_value = b"x" * 11
    pool = Mock()
    pool.request.return_value = response
    monkeypatch.setattr(urllib3, "HTTPSConnectionPool", Mock(return_value=pool))
    with pytest.raises(ValueError, match="download size"):
        research.fetch_public("https://paper.example/article")
    response.read.assert_called_once_with(11)


def test_arxiv_requests_are_spaced_without_delaying_other_hosts(monkeypatch):
    monkeypatch.setattr(research, "_ARXIV_LAST_REQUEST", 100.0)
    monkeypatch.setattr(research.time, "monotonic", lambda: 101.0)
    sleeps = []
    monkeypatch.setattr(research.time, "sleep", sleeps.append)
    research.pace_arxiv("api.crossref.org")
    assert not sleeps
    research.pace_arxiv("export.arxiv.org")
    assert sleeps == [2.0]


def test_crossref_requests_are_spaced(monkeypatch):
    monkeypatch.setattr(research, "_CROSSREF_LAST_REQUEST", 100.0)
    moments = iter([100.25, 101.0])
    monkeypatch.setattr(research.time, "monotonic", lambda: next(moments))
    sleeps = []
    monkeypatch.setattr(research.time, "sleep", sleeps.append)
    research.pace_crossref("api.crossref.org")
    assert sleeps == [0.75]


def test_rate_limit_retries_once_then_opens_host_circuit(monkeypatch):
    import urllib3

    monkeypatch.setattr(research, "public_address", lambda *a: "93.184.216.34")
    monkeypatch.setattr(research, "pace_crossref", lambda *a: None)
    monkeypatch.setattr(research, "pace_arxiv", lambda *a: None)
    monkeypatch.setattr(research, "_HOST_BLOCKED_UNTIL", {})
    monkeypatch.setattr(research.time, "sleep", lambda delay: None)
    responses = [Mock(status=429, headers={"Retry-After": "5"}),
                 Mock(status=429, headers={"Retry-After": "5"})]
    pool = Mock()
    pool.request.side_effect = responses
    monkeypatch.setattr(urllib3, "HTTPSConnectionPool", Mock(return_value=pool))
    with pytest.raises(research.ResearchToolError) as failure:
        research.fetch_public("https://api.crossref.org/works?q=test")
    assert failure.value.code == "source_rate_limited"
    assert failure.value.retryable is True
    assert pool.request.call_count == 2
    with pytest.raises(research.ResearchToolError, match="cooldown"):
        research.fetch_public("https://api.crossref.org/works?q=another")
    assert pool.request.call_count == 2


def test_crossref_normalizes_insecure_staging_fulltext(monkeypatch):
    data = {"message": {"items": [{"DOI": "10.1234/test", "title": ["A paper"],
             "URL": "http://xplorestaging.ieee.org/document/1",
             "link": [{"URL": "http://xplorestaging.ieee.org/paper.pdf"}]}]}}
    monkeypatch.setattr(research, "fetch_public", lambda url, **kw: (url, "application/json", json.dumps(data).encode()))
    paper = research.search(research.ResearchRequest(query="test"), timeout_seconds=5)["papers"][0]
    assert paper["url"] == "https://ieeexplore.ieee.org/document/1"
    assert paper["fulltext_urls"] == ["https://ieeexplore.ieee.org/paper.pdf"]


def test_crossref_preserves_metadata_and_declares_evidence_level(monkeypatch):
    data = {"message": {"items": [{"DOI": "10.1234/test", "title": ["A paper"],
             "author": [{"given": "A", "family": "Researcher"}], "published": {"date-parts": [[2024]]},
             "abstract": "<jats:p>Reported findings.</jats:p>", "type": "journal-article"}]}}
    monkeypatch.setattr(research, "fetch_public", lambda url, **kw: (url, "application/json", json.dumps(data).encode()))
    result = json.loads(research.execute_research("scholarly.search", research.ResearchRequest(query="test"), timeout_seconds=5))
    paper = result["papers"][0]
    assert paper["id"] == "doi:10.1234/test"
    assert paper["authors"] == ["A Researcher"]
    assert paper["abstract"] == "Reported findings."
    assert paper["evidence_level"] == "abstract"
    assert result["retrieved_at"] and result["untrusted_source_content"]


def test_read_reports_partial_text_and_access_failures(monkeypatch):
    body = ("A relevant academic paragraph. " * 1200).encode()
    monkeypatch.setattr(research, "fetch_public", lambda url, **kw: (url, "text/plain", body))
    result = research.read(research.ResearchRequest(url="https://paper.example/full"), timeout_seconds=5)
    assert result["truncated"]
    assert len(result["text"]) == 24000
    assert result["next_offset"] == 24000
    tail = research.read(research.ResearchRequest(url="https://paper.example/full", offset=24000), timeout_seconds=5)
    assert tail["text"] == body.decode()[24000:48000]
    assert tail["next_offset"] is None
    monkeypatch.setattr(research, "fetch_public", lambda url, **kw: (url, "text/html", b"<html>Access denied</html>"))
    with pytest.raises(ValueError, match="no extractable"):
        research.read(research.ResearchRequest(url="https://paper.example/full"), timeout_seconds=5)
