"""Read-only workspace backends and the workspace content resolver.

A project grants read access at immutable revisions. Reading never checks out a
working tree, never runs a hook and never writes to the repository, so a project
cannot be mutated by observation. Every failure raises; nothing falls back to a
live tree.
"""

from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path
from typing import Protocol

from anchor.domain.content import ContentKind, ContentRef
from anchor.runtime.content import ContentUnavailable

_HEX_REVISION_RE = re.compile(r"^[0-9a-f]{40}$|^[0-9a-f]{64}$")
DEFAULT_MAX_BYTES = 1_000_000


class WorkspaceError(RuntimeError):
    """A workspace backend could not satisfy a read."""


class WorkspaceBackend(Protocol):
    name: str

    def resolve_revision(self, revision: str) -> str: ...

    def list_paths(self, revision: str, prefix: str | None = None) -> list[str]: ...

    def read_text(self, revision: str, path: str) -> str: ...


def _run_git(root: str, *args: str, timeout: float = 10.0) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", root, *args], capture_output=True,
                          timeout=timeout, check=False, env={**os.environ, "GIT_TERMINAL_PROMPT": "0"})


def validate_project_root(root: str, backend: str = "git") -> None:
    """Fail registration when the root is not a usable read-only source."""
    if backend != "git":
        raise WorkspaceError(f"unsupported project backend: {backend}")
    path = Path(root).expanduser()
    if not path.is_dir():
        raise WorkspaceError(f"project root is not a directory: {root}")
    result = _run_git(str(path), "rev-parse", "--git-dir")
    if result.returncode != 0:
        raise WorkspaceError(f"project root is not a git repository: {root}")


class GitWorkspaceBackend:
    """Read blobs at an immutable commit without touching the working tree."""

    name = "git"

    def __init__(self, root: str, *, timeout: float = 10.0,
                 max_bytes: int = DEFAULT_MAX_BYTES) -> None:
        self.root = root
        self.timeout = timeout
        self.max_bytes = max_bytes

    def resolve_revision(self, revision: str) -> str:
        """Return the commit id, or raise if it is not an immutable commit.

        Only full hexadecimal ids are accepted: a branch or tag would move, and a
        reference that moves cannot be replayed.
        """
        if not _HEX_REVISION_RE.match(revision):
            raise WorkspaceError(f"revision is not an immutable commit id: {revision!r}")
        kind = self._git("cat-file", "-t", revision)
        if kind != "commit":
            raise WorkspaceError(f"revision {revision!r} is a {kind}, not a commit")
        return revision

    def list_paths(self, revision: str, prefix: str | None = None) -> list[str]:
        self.resolve_revision(revision)
        output = self._git("ls-tree", "-r", "--name-only", "-z", revision)
        paths = [item for item in output.split("\x00") if item]
        if prefix:
            paths = [item for item in paths if item.startswith(prefix)]
        return paths

    def read_text(self, revision: str, path: str) -> str:
        self.resolve_revision(revision)
        object_type = self._git_object_type(revision, path)
        if object_type != "blob":
            raise WorkspaceError(f"path {path!r} is a {object_type}, not a file")
        size = self._git_size(revision, path)
        if size > self.max_bytes:
            raise WorkspaceError(f"file {path!r} is {size} bytes, over the {self.max_bytes} limit")
        raw = self._git_bytes("cat-file", "-p", f"{revision}:{path}")
        try:
            return raw.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise WorkspaceError(f"file {path!r} is not UTF-8 text") from exc

    def _git(self, *args: str) -> str:
        return self._git_bytes(*args).decode("utf-8", errors="strict").strip()

    def _git_bytes(self, *args: str) -> bytes:
        result = _run_git(self.root, *args, timeout=self.timeout)
        if result.returncode != 0:
            message = result.stderr.decode("utf-8", errors="replace").strip()
            raise WorkspaceError(f"git {' '.join(args)} failed: {message}")
        return result.stdout

    def _git_object_type(self, revision: str, path: str) -> str:
        try:
            return self._git("cat-file", "-t", f"{revision}:{path}")
        except WorkspaceError as exc:
            raise WorkspaceError(f"path {path!r} is not present at {revision}") from exc

    def _git_size(self, revision: str, path: str) -> int:
        try:
            return int(self._git("cat-file", "-s", f"{revision}:{path}"))
        except WorkspaceError as exc:
            raise WorkspaceError(f"path {path!r} is not a file at {revision}") from exc


class WorkspaceResolver:
    """Resolve ``workspace://`` references against registered projects."""

    kind = ContentKind.WORKSPACE

    def __init__(self, store, *, max_bytes: int = DEFAULT_MAX_BYTES) -> None:
        self.store = store
        self.max_bytes = max_bytes

    def _backend(self, ref: ContentRef) -> GitWorkspaceBackend:
        project = self.store.get_project(ref.workspace_id or "")
        if project is None:
            raise ContentUnavailable(ref, f"unknown workspace/project {ref.workspace_id!r}")
        if project.backend != "git":
            raise ContentUnavailable(ref, f"unsupported backend {project.backend!r}")
        return GitWorkspaceBackend(project.root, max_bytes=self.max_bytes)

    def exists(self, ref: ContentRef) -> bool:
        if ref.kind is not self.kind or not ref.path:
            return False
        try:
            self._backend(ref).read_text(ref.revision or "", ref.path)
        except (ContentUnavailable, WorkspaceError):
            return False
        return True

    def read_text(self, ref: ContentRef) -> str:
        if ref.kind is not self.kind:
            raise ContentUnavailable(ref, "workspace resolver cannot read this reference")
        if not ref.path:
            raise ContentUnavailable(ref, "reading a whole tree as text is not supported; name a path")
        try:
            return self._backend(ref).read_text(ref.revision or "", ref.path)
        except (ContentUnavailable, WorkspaceError) as exc:
            if isinstance(exc, ContentUnavailable):
                raise
            raise ContentUnavailable(ref, str(exc)) from exc
