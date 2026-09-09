"""Prepare, verify, commit and reconcile content-producing nodes.

The content store and the control store cannot share a transaction, so the
commit is a protocol rather than one atomic operation: freeze the bytes, record
that they are prepared, run the verifier, commit the control event, then clear
the prepared marker. Every step is idempotent, and the reconciler converges any
window that a crash leaves behind.

A revision whose bytes have vanished is never guessed from a live workspace; it
is reported as inconsistent so the run fails closed (I2).
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from uuid import UUID

from anchor.domain.content_commit import PreparedRevision
from anchor.domain.workspace import WorkspaceState
from anchor.runtime.workspace import GitWorkspaceBackend, WorkspaceError
from anchor.runtime.workspaces import WorkspaceManager


class ContentCommitError(RuntimeError):
    """The prepared-revision protocol could not proceed."""


@dataclass(frozen=True)
class ReconcileOutcome:
    prepared: PreparedRevision
    action: str  # already_committed | verified | committed | inconsistent

    @property
    def consistent(self) -> bool:
        return self.action != "inconsistent"


class ContentCommitter:
    def __init__(self, store, workspaces: WorkspaceManager) -> None:
        self.store = store
        self.workspaces = workspaces

    # -- protocol steps ----------------------------------------------------
    def prepare(self, *, run_id: UUID, node_run_id: UUID, attempt: int,
                workspace_id: str) -> PreparedRevision:
        """Freeze the workspace and record the prepared revision (idempotent)."""
        existing = self.store.get_prepared_revision(node_run_id, attempt)
        if existing is not None:
            return existing
        workspace = self.store.get_workspace(workspace_id)
        if workspace is None:
            raise ContentCommitError(f"unknown workspace: {workspace_id}")
        if workspace.state is WorkspaceState.ARCHIVED:
            raise ContentCommitError("an archived workspace cannot be prepared")
        if workspace.state is WorkspaceState.ACTIVE:
            workspace = self.workspaces.freeze(workspace_id, actor=f"node:{node_run_id}")
        revision = workspace.current_revision
        if not revision:
            raise ContentCommitError("workspace has no revision to prepare")
        project = self.store.get_project(workspace.project_id)
        if project is None:
            raise ContentCommitError(f"unknown project: {workspace.project_id}")
        digest = GitWorkspaceBackend(project.root).tree_digest(revision)
        return self.store.record_prepared_revision(PreparedRevision(
            node_run_id=node_run_id, attempt=attempt, run_id=run_id,
            workspace_id=workspace_id, revision=revision, manifest_digest=digest))

    def record_verification(self, prepared: PreparedRevision,
                            verifier_result: dict) -> PreparedRevision:
        return self.store.update_prepared_verification(
            prepared.node_run_id, prepared.attempt, verifier_result)

    def commit(self, prepared: PreparedRevision,
               *, commit_fn: Callable[[PreparedRevision], None]) -> None:
        """Run the control commit and clear the prepared marker.

        ``commit_fn`` must itself be idempotent, because a reconciler may invoke
        it after the event was already appended.
        """
        commit_fn(prepared)
        self.store.clear_prepared_revision(prepared.node_run_id, prepared.attempt)

    # -- reconciliation ----------------------------------------------------
    def reconcile(self, *, is_committed: Callable[[PreparedRevision], bool],
                  verify_fn: Callable[[PreparedRevision], dict],
                  commit_fn: Callable[[PreparedRevision], None],
                  limit: int = 200) -> list[ReconcileOutcome]:
        outcomes: list[ReconcileOutcome] = []
        for prepared in self.store.list_prepared_revisions(limit):
            if is_committed(prepared):
                self.store.clear_prepared_revision(prepared.node_run_id, prepared.attempt)
                outcomes.append(ReconcileOutcome(prepared, "already_committed"))
                continue
            if not self.content_readable(prepared):
                outcomes.append(ReconcileOutcome(prepared, "inconsistent"))
                continue
            if prepared.verifier_result is None:
                prepared = self.record_verification(prepared, verify_fn(prepared))
                outcomes.append(ReconcileOutcome(prepared, "verified"))
                continue
            self.commit(prepared, commit_fn=commit_fn)
            outcomes.append(ReconcileOutcome(prepared, "committed"))
        return outcomes

    def content_readable(self, prepared: PreparedRevision) -> bool:
        workspace = self.store.get_workspace(prepared.workspace_id)
        if workspace is None:
            return False
        project = self.store.get_project(workspace.project_id)
        if project is None:
            return False
        try:
            GitWorkspaceBackend(project.root).resolve_revision(prepared.revision)
        except WorkspaceError:
            return False
        return True
