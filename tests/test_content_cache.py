"""A cross-run cache that cannot quietly stop telling the truth.

Re-running a literature review re-fetches the same papers, so a cache is worth having. But a
cache is also where a system starts lying to itself: it can serve a stale copy, serve one task's
content to another, or hide a damaged entry until the damage reaches an artifact. Each of those
is a case below, and each is expected to be a *reported outcome* rather than a silent success.

The acceptance the plan asks for is cold, warm, corrupt, expired, and two kinds of key mismatch.
All six are here, plus the property that matters most: none of it touches canonical state.
"""

from __future__ import annotations

import json

import pytest

from anchor.runtime.content_cache import CACHE_FORMAT, CacheKey, ContentCache

URL = "https://arxiv.org/abs/2401.00001"
VERSION = "93e90cd1-88fb-4cbc-b1a6-3e8e12752659"


def key(*, url: str = URL, version: str = VERSION, scope: str = "cost models") -> CacheKey:
    return CacheKey(url=url, graph_version_id=version, scope=scope)


def seeded(tmp_path, **kwargs):
    cache = ContentCache(tmp_path / "cache", **kwargs)
    entry = cache.put(key(), content_type="text/html", final_url=URL,
                      body=b"<html>paper</html>")
    return cache, entry


def test_a_cold_cache_misses(tmp_path):
    cache = ContentCache(tmp_path / "cache")
    lookup = cache.get(key())
    assert lookup.outcome == "miss"
    assert lookup.entry is None
    assert cache.lookups["miss"] == 1


def test_a_warm_cache_hits_and_returns_the_bytes(tmp_path):
    cache, entry = seeded(tmp_path)
    lookup = cache.get(key())
    assert lookup.outcome == "hit"
    assert lookup.entry is not None
    assert lookup.entry.body == b"<html>paper</html>"
    assert lookup.entry.sha256 == entry.sha256
    assert lookup.entry.content_type == "text/html"


def test_a_corrupted_entry_is_detected_not_returned(tmp_path):
    """The check that stops a damaged entry becoming evidence.

    The hash is recomputed on every read rather than trusted from the metadata, so an edited or
    truncated body is a reported outcome. Reporting it as a miss would hide a disk problem for
    as long as the file remained readable.
    """
    cache, _entry = seeded(tmp_path)
    body_file = next(cache.root.glob("*.bin"))
    body_file.write_bytes(b"<html>tampered</html>")

    lookup = cache.get(key())
    assert lookup.outcome == "corrupt"
    assert lookup.entry is None
    assert "does not match" in lookup.detail
    assert cache.lookups["corrupt"] == 1


def test_metadata_that_does_not_match_the_key_is_corrupt(tmp_path):
    """A file that appeared under the wrong name may not be served as the right answer."""
    cache, _entry = seeded(tmp_path)
    meta_file = next(cache.root.glob("*.json"))
    meta = json.loads(meta_file.read_text(encoding="utf-8"))
    meta["key"] = {"url": "https://elsewhere.example/x", "graph_version_id": VERSION,
                   "scope": "cost models"}
    meta_file.write_text(json.dumps(meta), encoding="utf-8")
    assert cache.get(key()).outcome == "corrupt"


def test_an_unknown_format_is_corrupt(tmp_path):
    """A file this build does not understand is not a file it may interpret."""
    cache, _entry = seeded(tmp_path)
    meta_file = next(cache.root.glob("*.json"))
    meta = json.loads(meta_file.read_text(encoding="utf-8"))
    meta["format"] = CACHE_FORMAT + 1
    meta_file.write_text(json.dumps(meta), encoding="utf-8")
    assert cache.get(key()).outcome == "corrupt"


def test_an_entry_past_its_ttl_is_expired_and_a_fresh_one_is_not(tmp_path):
    cache = ContentCache(tmp_path / "cache", ttl_seconds=100)
    cache.put(key(), content_type="text/html", final_url=URL, body=b"x", now=1_000.0)
    assert cache.get(key(), now=1_050.0).outcome == "hit"
    lookup = cache.get(key(), now=1_200.0)
    assert lookup.outcome == "expired"
    assert lookup.entry is None, "an expired entry may not be served"


def test_no_ttl_means_no_expiry(tmp_path):
    """Right for immutable published papers, which is the common case here."""
    cache = ContentCache(tmp_path / "cache")
    cache.put(key(), content_type="text/html", final_url=URL, body=b"x", now=1_000.0)
    assert cache.get(key(), now=10_000_000.0).outcome == "hit"


def test_a_different_graph_version_does_not_reuse_another_versions_fetch(tmp_path):
    """The bytes at a URL do not depend on the version; the policy that fetched them does.

    Timeouts, allowed hosts and extraction belong to a graph version, so serving one version's
    fetch to another would let an older policy bypass a newer one.
    """
    cache, _entry = seeded(tmp_path)
    assert cache.get(key(version="some-other-version")).outcome == "miss"


def test_a_different_task_scope_does_not_reuse_another_tasks_fetch(tmp_path):
    """Serving one scope to another moves content between tasks with no edge declaring it."""
    cache, _entry = seeded(tmp_path)
    assert cache.get(key(scope="something else")).outcome == "miss"


def test_a_different_url_with_the_same_scope_misses(tmp_path):
    cache, _entry = seeded(tmp_path)
    assert cache.get(key(url="https://arxiv.org/abs/2401.00002")).outcome == "miss"


def test_putting_over_an_existing_entry_is_refused_unless_the_caller_says_why(tmp_path):
    """Silently overwriting would erase the evidence of whatever damaged the entry."""
    cache, _entry = seeded(tmp_path)
    with pytest.raises(FileExistsError):
        cache.put(key(), content_type="text/html", final_url=URL, body=b"new")
    replaced = cache.put(key(), content_type="text/html", final_url=URL, body=b"new",
                         replacement=True)
    assert replaced.body == b"new"
    assert cache.get(key()).entry.body == b"new"


def test_a_corrupt_entry_can_be_replaced_and_then_hits(tmp_path):
    """Recovery, which is what makes refusing to overwrite safe."""
    cache, _entry = seeded(tmp_path)
    next(cache.root.glob("*.bin")).write_bytes(b"broken")
    assert cache.get(key()).outcome == "corrupt"
    cache.put(key(), content_type="text/html", final_url=URL, body=b"<html>refetched</html>",
              replacement=True)
    lookup = cache.get(key())
    assert lookup.outcome == "hit"
    assert lookup.entry.body == b"<html>refetched</html>"


def test_discarding_removes_both_files(tmp_path):
    cache, _entry = seeded(tmp_path)
    assert cache.discard(key()) is True
    assert list(cache.root.iterdir()) == []
    assert cache.get(key()).outcome == "miss"


def test_a_remote_cache_root_is_refused_like_a_remote_artifact_root(tmp_path):
    """A cache configured at a URL is not a cache this build has, and pretending would put the
    entries somewhere nobody looks."""
    from anchor.runtime.artifacts import ArtifactBackendUnsupported

    with pytest.raises(ArtifactBackendUnsupported):
        ContentCache("s3://bucket/cache")


def test_a_non_positive_ttl_is_refused_rather_than_disabling_expiry(tmp_path):
    """A ttl of zero read as 'never expires' would be the opposite of what was asked."""
    with pytest.raises(ValueError):
        ContentCache(tmp_path / "cache", ttl_seconds=0)


def test_the_cache_is_a_directory_outside_the_database_and_the_artifacts(tmp_path):
    """It must be deletable, because a projection that cannot be dropped becomes state.

    Nothing about a run's completion may depend on it: the bytes it holds are also reachable
    through the run's own artifacts, and losing every entry can only cost a fetch.
    """
    cache, _entry = seeded(tmp_path)
    assert cache.root.is_dir()
    assert not (tmp_path / "cache" / "..").resolve().name.endswith("artifacts")
    summary = cache.summary()
    assert summary["root"] == str(cache.root)
    # Dropping everything is supported and leaves a working cache.
    for path in cache.root.iterdir():
        path.unlink()
    assert cache.get(key()).outcome == "miss"
    cache.put(key(), content_type="text/html", final_url=URL, body=b"again")
    assert cache.get(key()).outcome == "hit"


def test_a_fetch_uses_the_cache_only_inside_a_scope(tmp_path):
    """`fetch_public` is per-URL and knows nothing about the run, so the context is set around
    the call. Outside one, nothing is cached — which is what keeps a key from being incomplete.
    """
    from anchor.runtime.research_tools import _cached_fetch, content_cache_scope

    cache = ContentCache(tmp_path / "cache")
    fetches: list[int] = []

    def fetch():
        fetches.append(1)
        return "text/html", URL, b"<html>body</html>"

    with content_cache_scope(cache, graph_version_id=VERSION, scope="cost models"):
        _cached_fetch(URL, fetch)
        assert _cached_fetch(URL, fetch)[2] == b"<html>body</html>"
    assert len(fetches) == 1, "the second call inside the scope must be served from the cache"

    # Outside the scope there is no cache and no accidental key meant for somebody else.
    _cached_fetch(URL, fetch)
    assert len(fetches) == 2
    assert cache.lookups == {"hit": 1, "miss": 1, "expired": 0, "corrupt": 0}


def test_a_corrupt_entry_is_replaced_after_a_refetch(tmp_path):
    """Through the real fetch path, because that is where a corrupt entry would otherwise sit."""
    from anchor.runtime.research_tools import _cached_fetch, content_cache_scope

    cache = ContentCache(tmp_path / "cache")
    with content_cache_scope(cache, graph_version_id=VERSION, scope="s"):
        _cached_fetch(URL, lambda: ("text/html", URL, b"<html>first</html>"))
        next(cache.root.glob("*.bin")).write_bytes(b"damaged")
        _, _, body = _cached_fetch(URL, lambda: ("text/html", URL, b"<html>second</html>"))
    assert body == b"<html>second</html>"
    assert cache.lookups["corrupt"] == 1
    assert cache.get(key(scope="s")).entry.body == b"<html>second</html>"
