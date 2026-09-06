"""Content-addressed artifact storage boundary."""

from __future__ import annotations

import hashlib
from pathlib import Path
from typing import Protocol


class ArtifactStore(Protocol):
    def put_text(self, text: str, *, media_type: str = "text/plain") -> str: ...
    def get_text(self, ref: str) -> str: ...


class LocalArtifactStore:
    """Small development store; production can substitute S3/MinIO."""

    def __init__(self, root: str | Path) -> None:
        self.root = Path(root).expanduser()
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)

    def put_text(self, text: str, *, media_type: str = "text/plain") -> str:
        if not isinstance(text, str):
            raise TypeError("artifact text must be a string")
        digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
        path = self.root / digest
        if not path.exists():
            path.write_text(text, encoding="utf-8")
            path.chmod(0o600)
        return f"artifact://sha256/{digest}"

    def get_text(self, ref: str) -> str:
        prefix = "artifact://sha256/"
        if not ref.startswith(prefix):
            raise ValueError("unsupported artifact reference")
        digest = ref[len(prefix):]
        if len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
            raise ValueError("invalid artifact reference")
        path = self.root / digest
        text = path.read_text(encoding="utf-8")
        if hashlib.sha256(text.encode("utf-8")).hexdigest() != digest:
            raise ValueError("artifact integrity check failed")
        return text

