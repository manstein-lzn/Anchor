"""Writable workspace lifecycle: create, write, freeze, archive.

Every mutation goes through the manager, is committed to the workspace branch
and is recorded in the operation ledger together with its event. A caller cannot
write a file without leaving a revision and an audit entry behind.

The workspace itself is not a source of truth. It becomes one only when a
control event references the revision it produced (I2).
"""

from __future__ import annotations

import hashlib
import os
import subprocess
from pathlib import Path
from uuid import uuid4

from anchor.domain.content import validate_workspace_path
from anchor.domain.workspace import (
    Workspace,
    WorkspaceOperation,
    WorkspaceOperationKind,
    WorkspaceState,
)

from anchor.runtime.workspace import WorkspaceError

DEFAULT_TIMEOUT_SECONDS = 60.0
DEFAULT_MAX_FILE_BYTES = 1_000_000


def _run_git(cwd: str, *args: str, timeout: float) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", cwd, *args], capture_output=True, timeout=timeout,
                          check=False, env={**os.environ, "GIT_TERMINAL_PROMPT": "0"})


class GitWorktree:
    """Git primitives for exactly one worktree; no policy lives here."""

    def __init__(self, repo_root: str, path: Path, branch: str,
                 *, timeout: float = DEFAULT_TIMEOUT_SECONDS) -> None:
        self.repo_root = repo_root
        self.path = path
        self.branch = branch
        self.timeout = timeout

    def create(self, base_revision: str) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        result = _run_git(self.repo_root, "worktree", "add", "-b", self.branch,
                          str(self.path), base_revision, timeout=self.timeout)
        if result.returncode != 0:
            raise WorkspaceError(result.stderr.decode("utf-8", errors="replace").strip())
        self.verify()

    def verify(self) -> None:
        """Fail unless this path is a worktree of exactly this repository.

        Without this check a relative or wrong path can resolve into a different
        repository, where `git commit` would silently succeed against unrelated
        history.
        """
        path = self.path.resolve()
        if not path.is_dir():
            raise WorkspaceError(f"workspace path does not exist: {path}")
        toplevel = Path(self._git("rev-parse", "--show-toplevel").strip()).resolve()
        if toplevel != path:
            raise WorkspaceError(
                f"workspace path {path} is inside a different repository: {toplevel}")
        actual = Path(self._git("rev-parse", "--git-common-dir").strip())
        if not actual.is_absolute():
            actual = (path / actual).resolve()
        expected = Path(_run_git(self.repo_root, "rev-parse", "--git-common-dir",
                                 timeout=self.timeout).stdout.decode().strip())
        if not expected.is_absolute():
            expected = (Path(self.repo_root).resolve() / expected).resolve()
        if actual.resolve() != expected.resolve():
            raise WorkspaceError(
                f"workspace path {path} belongs to {actual}, not {expected}")

    def head(self) -> str:
        return self._git("rev-parse", "HEAD").strip()

    def is_dirty(self) -> bool:
        return bool(self._git("status", "--porcelain").strip())

    def commit(self, message: str) -> str:
        """Commit the whole tree; returns the current head when nothing changed."""
        self.verify()
        self._git("add", "-A")
        if not self.is_dirty():
            return self.head()
        result = _run_git(str(self.path), "commit", "-q", "-m", message, timeout=self.timeout)
        if result.returncode != 0:
            raise WorkspaceError(result.stderr.decode("utf-8", errors="replace").strip())
        return self.head()

    def remove(self) -> None:
        result = _run_git(self.repo_root, "worktree", "remove", "--force", str(self.path),
                          timeout=self.timeout)
        if result.returncode != 0:
            raise WorkspaceError(result.stderr.decode("utf-8", errors="replace").strip())

    def _git(self, *args: str) -> str:
        result = _run_git(str(self.path), *args, timeout=self.timeout)
        if result.returncode != 0:
            raise WorkspaceError(result.stderr.decode("utf-8", errors="replace").strip())
        return result.stdout.decode("utf-8", errors="strict")


class WorkspaceManager:
    """Create and mutate run-scoped workspaces, recording every mutation."""

    def __init__(self, store, *, root: str | os.PathLike,
                 timeout: float = DEFAULT_TIMEOUT_SECONDS,
                 max_file_bytes: int = DEFAULT_MAX_FILE_BYTES) -> None:
        self.store = store
        # Absolute for the same reason as the project root: a relative path
        # resolves per process and can point at an unrelated repository.
        self.root = Path(root).expanduser().resolve()
        self.timeout = timeout
        self.max_file_bytes = max_file_bytes

    def create(self, *, project_id: str, base_revision: str,
               workspace_id: str | None = None, actor: str = "operator") -> Workspace:
        from anchor.runtime.workspace import GitWorkspaceBackend
        project = self.store.get_project(project_id)
        if project is None:
            raise WorkspaceError(f"unknown project: {project_id}")
        if project.backend != "git":
            raise WorkspaceError(f"unsupported project backend: {project.backend}")
        identifier = workspace_id or f"ws-{uuid4().hex[:12]}"
        backend = GitWorkspaceBackend(project.root, timeout=self.timeout)
        backend.resolve_revision(base_revision)  # fails closed on a non-commit
        branch = f"anchor/{identifier}"
        path = self.root / identifier
        worktree = GitWorktree(project.root, path, branch, timeout=self.timeout)
        worktree.create(base_revision)
        workspace = Workspace(workspace_id=identifier, project_id=project_id,
                              base_revision=base_revision, branch=branch, path=str(path),
                              state=WorkspaceState.ACTIVE, current_revision=base_revision)
        self.store.create_workspace(workspace)
        self.store.record_workspace_operation(WorkspaceOperation(
            workspace_id=identifier, kind=WorkspaceOperationKind.CREATE,
            after_revision=base_revision, actor=actor))
        return workspace

    def write_text(self, workspace_id: str, path: str, content: str, *,
                   actor: str = "operator") -> WorkspaceOperation:
        if not isinstance(content, str):
            raise WorkspaceError("workspace content must be a string")
        encoded = content.encode("utf-8")
        if len(encoded) > self.max_file_bytes:
            raise WorkspaceError(f"content is {len(encoded)} bytes, over the {self.max_file_bytes} limit")
        workspace = self._active(workspace_id)
        target = self._target(workspace, path)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        return self._commit(workspace, WorkspaceOperationKind.WRITE, path=path,
                            content_hash=hashlib.sha256(encoded).hexdigest(), actor=actor)

    def delete(self, workspace_id: str, path: str, *, actor: str = "operator") -> WorkspaceOperation:
        workspace = self._active(workspace_id)
        target = self._target(workspace, path)
        if not target.exists():
            raise WorkspaceError(f"path does not exist in workspace: {path}")
        target.unlink()
        return self._commit(workspace, WorkspaceOperationKind.DELETE, path=path, actor=actor)

    def freeze(self, workspace_id: str, *, actor: str = "operator") -> Workspace:
        workspace = self.store.get_workspace(workspace_id)
        if workspace is None:
            raise WorkspaceError(f"unknown workspace: {workspace_id}")
        if workspace.state is WorkspaceState.ARCHIVED:
            raise WorkspaceError("an archived workspace cannot be frozen")
        worktree = self._worktree(workspace)
        revision = worktree.commit(f"anchor: freeze {workspace_id}")
        self.store.record_workspace_operation(WorkspaceOperation(
            workspace_id=workspace_id, kind=WorkspaceOperationKind.FREEZE,
            before_revision=workspace.current_revision, after_revision=revision, actor=actor))
        return self.store.update_workspace_state(
            workspace_id, state=WorkspaceState.FROZEN, current_revision=revision)

    def archive(self, workspace_id: str, *, actor: str = "operator") -> Workspace:
        workspace = self.store.get_workspace(workspace_id)
        if workspace is None:
            raise WorkspaceError(f"unknown workspace: {workspace_id}")
        if workspace.state is not WorkspaceState.ARCHIVED:
            self._worktree(workspace).remove()
            self.store.record_workspace_operation(WorkspaceOperation(
                workspace_id=workspace_id, kind=WorkspaceOperationKind.ARCHIVE,
                before_revision=workspace.current_revision, actor=actor))
        return self.store.update_workspace_state(
            workspace_id, state=WorkspaceState.ARCHIVED,
            current_revision=workspace.current_revision)

    # -- internals ---------------------------------------------------------
    def _active(self, workspace_id: str) -> Workspace:
        workspace = self.store.get_workspace(workspace_id)
        if workspace is None:
            raise WorkspaceError(f"unknown workspace: {workspace_id}")
        if not workspace.is_writable:
            raise WorkspaceError(f"workspace {workspace_id} is {workspace.state.value}, not writable")
        return workspace

    def _worktree(self, workspace: Workspace) -> GitWorktree:
        project = self.store.get_project(workspace.project_id)
        if project is None:
            raise WorkspaceError(f"unknown project: {workspace.project_id}")
        return GitWorktree(project.root, Path(workspace.path), workspace.branch, timeout=self.timeout)

    def _target(self, workspace: Workspace, path: str) -> Path:
        validate_workspace_path(path)
        root = Path(workspace.path).resolve()
        target = (root / path).resolve()
        if target != root and root not in target.parents:
            raise WorkspaceError(f"path escapes the workspace: {path}")
        return target

    def _commit(self, workspace: Workspace, kind: WorkspaceOperationKind, *,
                path: str | None, content_hash: str | None = None,
                actor: str) -> WorkspaceOperation:
        worktree = self._worktree(workspace)
        revision = worktree.commit(f"anchor: {kind.value} {path or ''}".strip())
        operation = self.store.record_workspace_operation(WorkspaceOperation(
            workspace_id=workspace.workspace_id, kind=kind, path=path,
            before_revision=workspace.current_revision, after_revision=revision,
            content_hash=content_hash, actor=actor))
        self.store.update_workspace_state(workspace.workspace_id, state=workspace.state,
                                          current_revision=revision)
        return operation


def node_workspace_id(store, lease) -> str | None:
    """The workspace a node declares through ``metadata.workspace_id``."""
    run = store.get_run(lease.run_id)
    graph = store.get_version_for_run(run) if hasattr(store, "get_version_for_run") else None
    if graph is None:
        graph = store.get_graph_version(run.graph_version_id) if run else None
    node = next((item for item in graph.definition.nodes if item.id == lease.node_id),
                None) if graph else None
    return (node.metadata or {}).get("workspace_id") if node else None
