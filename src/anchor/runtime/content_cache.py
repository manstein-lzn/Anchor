"""Cross-run cache for fetched research content.

Re-running a literature review re-fetches the same papers. That is waste, but a cache is also a
place where a system can quietly stop telling the truth — it can serve a stale copy, serve one
task's content to another, or hide a corrupted entry until the corruption reaches an artifact.
So the rules are explicit and the failure modes are outcomes rather than exceptions nobody sees:

- **It is a projection, not canonical state.** It lives in its own directory, outside the
  database and outside the artifact store, so deleting it is a supported operation and its loss
  can never be a recovery failure. Nothing about a run's completion depends on it.
- **An entry carries its own identity**: the URL, the graph version and task scope it was
  fetched under, when, the content type and final URL after redirects, and the SHA-256 of the
  bytes. A reader recomputes the hash rather than trusting it, so a truncated or edited file is
  *detected* and reported as corrupt instead of being returned.
- **Every read says which of the four things happened**: `hit`, `miss`, `expired`, or `corrupt`.
  A caller that only cares about "did I avoid a fetch" can treat all but `hit` the same way, but
  an operator can see which one it was — and a cache that reported a corrupt entry as a miss
  would hide a disk problem indefinitely.
- **A key includes the graph version and the scope.** The bytes at a URL do not depend on either,
  but the *policy* that decided to fetch them does: timeouts, allowed hosts and extraction belong
  to a graph version, and a task's scope decides what it was looking for. Serving one version's
  fetch to another would let an older policy bypass a newer one, and serving one scope to another
  would move content between tasks without an edge that declared it — which is the same thing the
  declared-input rule exists to prevent.
"""

from __future__ import annotations

import base64
import hashlib
import json
import logging
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal

logger = logging.getLogger("anchor.content_cache")

#: What a lookup did. Anything other than ``hit`` means the caller must fetch.
Outcome = Literal["hit", "miss", "expired", "corrupt"]

CACHE_FORMAT = 1


@dataclass(frozen=True)
class CacheKey:
    """What identifies a cached fetch.

    The URL alone would identify the *content*; the version and scope identify the *decision* to
    fetch it, which is what a projection of a run may not silently transfer.
    """

    url: str
    graph_version_id: str
    scope: str

    def digest(self) -> str:
        payload = json.dumps({"url": self.url, "graph_version_id": self.graph_version_id,
                              "scope": self.scope}, sort_keys=True, ensure_ascii=False)
        return hashlib.sha256(payload.encode("utf-8")).hexdigest()


@dataclass(frozen=True)
class CacheEntry:
    """The bytes, and everything needed to decide whether to trust them."""

    key: CacheKey
    fetched_at: float
    content_type: str
    final_url: str
    body: bytes
    sha256: str

    def age_seconds(self, *, now: float | None = None) -> float:
        return (now if now is not None else time.time()) - self.fetched_at


@dataclass(frozen=True)
class CacheLookup:
    """The result of a lookup, including *why* it was not a hit."""

    outcome: Outcome
    entry: CacheEntry | None = None
    detail: str = ""

    @property
    def hit(self) -> bool:
        return self.outcome == "hit"


class ContentCache:
    """A directory of fetched responses, each with its own integrity metadata.

    Two files per entry: the metadata as JSON and the body as raw bytes. Writing the body
    separately keeps the metadata readable when the body is large, and lets a reader verify the
    body without parsing base64 out of JSON.
    """

    def __init__(self, root: str | Path, *, ttl_seconds: float | None = None) -> None:
        from anchor.runtime.artifacts import ArtifactBackendUnsupported, _REMOTE_ROOT

        selected = str(root)
        if _REMOTE_ROOT.match(selected):
            # Same refusal as the artifact root: a cache configured at a URL is not a cache this
            # build has, and pretending would put the entries somewhere nobody looks.
            raise ArtifactBackendUnsupported(
                f"ANCHOR_CONTENT_CACHE_ROOT={selected!r} names a remote backend, which this "
                f"build does not implement; only a local directory is supported")
        if ttl_seconds is not None and ttl_seconds <= 0:
            raise ValueError("ttl_seconds must be positive when set")
        self.root = Path(selected).expanduser()
        self.ttl_seconds = ttl_seconds
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        #: Counted so a report can say whether the cache is doing anything.
        self.lookups: dict[Outcome, int] = {"hit": 0, "miss": 0, "expired": 0, "corrupt": 0}

    def _paths(self, key: CacheKey) -> tuple[Path, Path]:
        digest = key.digest()
        return self.root / f"{digest}.json", self.root / f"{digest}.bin"

    def get(self, key: CacheKey, *, now: float | None = None) -> CacheLookup:
        meta_path, body_path = self._paths(key)
        if not meta_path.is_file() or not body_path.is_file():
            return self._count(CacheLookup("miss"))
        try:
            meta: dict[str, Any] = json.loads(meta_path.read_text(encoding="utf-8"))
            body = body_path.read_bytes()
        except (OSError, ValueError) as exc:
            # Unreadable is not absent. Reporting it as a miss would hide a disk problem for as
            # long as the deletion succeeded.
            return self._count(CacheLookup("corrupt", detail=f"unreadable: {exc}"))
        if meta.get("format") != CACHE_FORMAT:
            return self._count(CacheLookup("corrupt", detail=f"unknown format {meta.get('format')}"))
        if meta.get("key") != {"url": key.url, "graph_version_id": key.graph_version_id,
                               "scope": key.scope}:
            return self._count(CacheLookup("corrupt", detail="metadata does not match the key"))
        actual = hashlib.sha256(body).hexdigest()
        if actual != meta.get("sha256"):
            # Recomputed rather than trusted: this is the check that makes a corrupted entry
            # visible instead of letting it become evidence.
            return self._count(CacheLookup(
                "corrupt", detail=f"body hash {actual[:12]} does not match {str(meta.get('sha256'))[:12]}"))
        if self._is_expired(float(meta.get("fetched_at") or 0), now=now):
            return self._count(CacheLookup("expired"))
        entry = CacheEntry(key=key, fetched_at=float(meta["fetched_at"]),
                           content_type=str(meta.get("content_type") or ""),
                           final_url=str(meta.get("final_url") or key.url),
                           body=body, sha256=actual)
        return self._count(CacheLookup("hit", entry=entry))

    def _is_expired(self, fetched_at: float, *, now: float | None) -> bool:
        if self.ttl_seconds is None:
            return False
        current = now if now is not None else time.time()
        return current - fetched_at > self.ttl_seconds

    def put(self, key: CacheKey, *, content_type: str, final_url: str, body: bytes,
            now: float | None = None, replacement: bool = False) -> CacheEntry:
        """Write an entry.

        ``replacement`` says the caller looked and found the entry unusable, so an existing file
        may be overwritten. Without it a corrupt entry is left alone and the caller is told to
        fetch: silently overwriting would erase the evidence of whatever damaged it.
        """
        meta_path, body_path = self._paths(key)
        existing = meta_path.exists() or body_path.exists()
        if existing and not replacement:
            raise FileExistsError(f"a cache entry already exists for {key.url}; refusing to "
                                  f"overwrite while its integrity is unknown")
        digest = hashlib.sha256(body).hexdigest()
        fetched_at = now if now is not None else time.time()
        # Body first, then metadata: a reader treats a missing metadata file as a miss, so a
        # half-written entry is invisible rather than wrong.
        body_path.write_bytes(body)
        meta_path.write_text(json.dumps({
            "format": CACHE_FORMAT,
            "key": {"url": key.url, "graph_version_id": key.graph_version_id, "scope": key.scope},
            "fetched_at": fetched_at,
            "content_type": content_type,
            "final_url": final_url,
            "sha256": digest,
            "bytes": len(body),
        }, ensure_ascii=False, indent=1), encoding="utf-8")
        return CacheEntry(key=key, fetched_at=fetched_at, content_type=content_type,
                          final_url=final_url, body=body, sha256=digest)

    def discard(self, key: CacheKey) -> bool:
        """Remove one entry, for a caller that decided it is unusable."""
        meta_path, body_path = self._paths(key)
        removed = False
        for path in (meta_path, body_path):
            try:
                path.unlink()
                removed = True
            except FileNotFoundError:
                pass
        return removed

    def _count(self, lookup: CacheLookup) -> CacheLookup:
        self.lookups[lookup.outcome] += 1
        if lookup.outcome == "corrupt":
            logger.warning("content cache entry is corrupt and will be refetched: %s",
                           lookup.detail)
        return lookup

    def summary(self) -> dict[str, Any]:
        return {"root": str(self.root), "ttl_seconds": self.ttl_seconds,
                "lookups": dict(self.lookups)}


def encode_body(body: bytes) -> str:
    """Base64, for a caller that has to put bytes in JSON."""
    return base64.b64encode(body).decode("ascii")
