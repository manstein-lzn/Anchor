"""Read-only workspace backends and the workspace content resolver.

A project grants read access at immutable revisions. Reading never checks out a
working tree, never runs a hook and never writes to the repository, so a project
cannot be mutated by observation. Every failure raises; nothing falls back to a
live tree.
"""

from __future__ import annotations

import hashlib
import io
import os
import re
import subprocess
import tarfile
import tempfile
from collections.abc import Sequence
from pathlib import Path
from typing import Protocol

from anchor.domain.content import ContentKind, ContentRef
from anchor.runtime.content import ContentUnavailable
from anchor.runtime.sandbox import (
    DEFAULT_MAX_OUTPUT_BYTES,
    DEFAULT_TIMEOUT_SECONDS,
    SandboxResult,
    SandboxSpec,
    WorkspaceSandbox,
)

_HEX_REVISION_RE = re.compile(r"^[0-9a-f]{40}$|^[0-9a-f]{64}$")
DEFAULT_MAX_BYTES = 1_000_000


class WorkspaceError(RuntimeError):
    """A workspace backend could not satisfy a read."""


class WorkspaceBackend(Protocol):
    name: str

    def resolve_revision(self, revision: str) -> str: ...

    def list_paths(self, revision: str, prefix: str | None = None) -> list[str]: ...

    def manifest(self, revision: str) -> list[tuple[str, str, str]]: ...

    def tree_digest(self, revision: str) -> str: ...

    def materialize(self, revision: str, destination: Path) -> None: ...

    def read_text(self, revision: str, path: str) -> str: ...


def _run_git(root: str, *args: str, timeout: float = 10.0) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", root, *args], capture_output=True,
                          timeout=timeout, check=False, env={**os.environ, "GIT_TERMINAL_PROMPT": "0"})


def validate_project_root(root: str, backend: str = "git") -> str:
    """Validate a read-only source and return its absolute path.

    Returning the resolved path is deliberate: a relative root would resolve
    differently in each process, and `git -C <relative>` can walk up into an
    unrelated repository.
    """
    if backend != "git":
        raise WorkspaceError(f"unsupported project backend: {backend}")
    path = Path(root).expanduser().resolve()
    if not path.is_dir():
        raise WorkspaceError(f"project root is not a directory: {root}")
    result = _run_git(str(path), "rev-parse", "--git-dir")
    if result.returncode != 0:
        raise WorkspaceError(f"project root is not a git repository: {root}")
    return str(path)


class GitWorkspaceBackend:
    """Read blobs at an immutable commit without touching the working tree."""

    name = "git"

    def __init__(self, root: str, *, timeout: float = 10.0,
                 max_bytes: int = DEFAULT_MAX_BYTES) -> None:
        # Always absolute: a relative root resolves differently per process and
        # `git -C <relative>` can walk up into an unrelated repository.
        self.root = str(Path(root).expanduser().resolve())
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

    def manifest(self, revision: str) -> list[tuple[str, str, str]]:
        """(mode, blob id, path) for every blob, sorted by raw path bytes."""
        self.resolve_revision(revision)
        raw = self._git_bytes("ls-tree", "-r", "-z", revision)
        entries: list[tuple[str, str, str]] = []
        for item in raw.split(b"\x00"):
            if not item:
                continue
            header, _, path = item.partition(b"\t")
            mode, object_type, object_id = header.split(b" ", 2)
            if object_type != b"blob":
                continue
            entries.append((mode.decode(), object_id.decode(), path.decode("utf-8")))
        entries.sort(key=lambda entry: entry[2].encode("utf-8"))
        return entries

    def tree_digest(self, revision: str) -> str:
        """Deterministic digest over the tree's canonical manifest.

        The digest covers paths, modes and blob ids, so two materializations of
        the same revision produce the same value. Hashing blob *contents* rather
        than git's object ids (to remove the object-format dependency) is a
        later refinement; the manifest records which object format was used.
        """
        hasher = hashlib.sha256()
        for mode, object_id, path in self.manifest(revision):
            hasher.update(f"{path}\x00{mode}\x00{object_id}\n".encode("utf-8"))
        return hasher.hexdigest()

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

    def materialize(self, revision: str, destination: Path) -> None:
        """Extract a clean tree at ``revision`` into ``destination``.

        ``git archive`` reads the object database and never touches the working
        tree or the index, so materializing a revision cannot mutate the source
        repository.
        """
        self.resolve_revision(revision)
        data = self._git_bytes("archive", "--format=tar", revision)
        destination.mkdir(parents=True, exist_ok=True)
        with tarfile.open(fileobj=io.BytesIO(data)) as archive:
            archive.extractall(destination, filter="data")

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


def resolve_source(store, workspace_id: str) -> tuple[str, str]:
    """Map a reference id to a repository root and backend.

    An id may name a run-scoped workspace (which belongs to a project) or a
    project directly, so a read-only project and a workspace share one reference
    form.
    """
    workspace = store.get_workspace(workspace_id)
    project_id = workspace.project_id if workspace is not None else workspace_id
    project = store.get_project(project_id)
    if project is None:
        raise WorkspaceError(f"unknown workspace/project {workspace_id!r}")
    return project.root, project.backend


class WorkspaceResolver:
    """Resolve ``workspace://`` references against workspaces or projects."""

    kind = ContentKind.WORKSPACE

    def __init__(self, store, *, max_bytes: int = DEFAULT_MAX_BYTES) -> None:
        self.store = store
        self.max_bytes = max_bytes

    def _backend(self, ref: ContentRef) -> GitWorkspaceBackend:
        try:
            root, backend = resolve_source(self.store, ref.workspace_id or "")
        except WorkspaceError as exc:
            raise ContentUnavailable(ref, str(exc)) from exc
        if backend != "git":
            raise ContentUnavailable(ref, f"unsupported backend {backend!r}")
        return GitWorkspaceBackend(root, max_bytes=self.max_bytes)

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


def execute_in_workspace(store, ref: ContentRef, command: Sequence[str], *,
                         sandbox: WorkspaceSandbox,
                         timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
                         max_output_bytes: int = DEFAULT_MAX_OUTPUT_BYTES) -> SandboxResult:
    """Materialize ``ref`` and run an allowlisted command against it, read-only.

    The materialized tree is temporary and removed afterwards, so executing a
    command leaves no trace in the source repository and cannot mutate a
    revision.
    """
    if ref.kind is not ContentKind.WORKSPACE:
        raise ContentUnavailable(ref, "execute_in_workspace requires a workspace reference")
    try:
        root, backend_name = resolve_source(store, ref.workspace_id or "")
    except WorkspaceError as exc:
        raise ContentUnavailable(ref, str(exc)) from exc
    if backend_name != "git":
        raise ContentUnavailable(ref, f"unsupported backend {backend_name!r}")
    backend = GitWorkspaceBackend(root)
    with tempfile.TemporaryDirectory(prefix="anchor-workspace-") as directory:
        backend.materialize(ref.revision or "", Path(directory))
        spec = SandboxSpec(workspace=Path(directory), command=tuple(command),
                           timeout_seconds=timeout_seconds, max_output_bytes=max_output_bytes)
        return sandbox.run(spec)
