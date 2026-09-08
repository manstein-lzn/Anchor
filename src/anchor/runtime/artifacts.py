"""Content-addressed artifact storage boundary."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import tempfile
from typing import Protocol
from uuid import UUID


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

    def export_markdown(self, run_id: UUID, node_id: str, text: str) -> Path:
        if not re.fullmatch(r"[A-Za-z][A-Za-z0-9_-]{0,63}", node_id):
            raise ValueError("invalid export node id")
        directory = self.root / "reports" / str(UUID(str(run_id)))
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        destination = directory / f"{node_id}.md"
        # Publish atomically without replacing an earlier, different artifact.
        with tempfile.NamedTemporaryFile(dir=directory, delete=False) as temporary:
            temporary.write(text.encode("utf-8"))
            temporary.flush()
            os.fsync(temporary.fileno())
        try:
            try:
                os.link(temporary.name, destination)
            except FileExistsError:
                if destination.read_text(encoding="utf-8") != text:
                    raise ValueError("Markdown export already exists with different content") from None
        finally:
            Path(temporary.name).unlink()
        return destination
