"""Core behavior: merge parallel branch revisions into a join workspace.

A `join` node is where parallel branches converge. When each branch produced its
own workspace revision, the join merges them into its own workspace under
``require_clean``: a conflict fails the node closed rather than silently keeping
one side. The behavior is registered as ``anchor.join_merge``.
"""

from __future__ import annotations

from uuid import uuid5

from anchor.domain.content import ContentRefError, parse, workspace_ref
from anchor.runtime.workspace import WorkspaceError
from anchor.runtime.workspaces import WorkspaceManager

JOIN_MERGE_REF = "anchor.join_merge"


class JoinMergeBehavior:
    """Merge the workspace revisions carried by the join node's input."""

    def __init__(self, store, workspaces: WorkspaceManager) -> None:
        self.store = store
        self.workspaces = workspaces

    def preflight(self, snapshot: dict, *, store, artifacts, run_id) -> dict | None:
        return None

    def validate_output(self, text: str) -> None:
        return None

    def execute_control(self, snapshot: dict, *, store, artifacts, run_id, node_id) -> dict:
        # The node's own declared input names the workspace it merges into, so
        # the behavior needs no graph lookup and cannot target a different tree.
        declared = snapshot.get("workspace")
        if not isinstance(declared, str):
            raise WorkspaceError(
                f"join node {node_id!r} has no declared workspace input to merge into")
        own = parse(declared)
        if not own.workspace_id or not own.revision:
            raise WorkspaceError(f"join node {node_id!r} declared an incomplete workspace input")
        target, expected = own.workspace_id, own.revision
        sources: list[str] = []
        for value in snapshot.values():
            if not isinstance(value, str):
                continue
            try:
                ref = parse(value)
            except ContentRefError:
                continue
            if ref.revision and ref.workspace_id != target and ref.revision not in sources:
                sources.append(ref.revision)
        if not sources:
            raise WorkspaceError(
                f"join node {node_id!r} received no branch workspace revisions to merge")
        # A stable per-(run, node) identity lets the merge hold the write claim.
        claimant = uuid5(run_id, f"join:{node_id}")
        merged: list[str] = []
        for revision in sources:
            operation = self.workspaces.merge(target, revision, actor=f"node:{node_id}",
                                              claimant=claimant, expected_revision=expected)
            if not operation.after_revision:
                raise WorkspaceError(f"merge of {revision[:12]} produced no revision")
            merged.append(operation.after_revision)
        workspace = self.store.get_workspace(target)
        if workspace is None or not workspace.current_revision:
            raise WorkspaceError(f"join workspace {target!r} has no revision after merge")
        return {"merged_sources": sources, "merged_revisions": merged,
                "workspace": str(workspace_ref(target, workspace.current_revision))}


def register_core_behaviors(behaviors, store, workspaces: WorkspaceManager) -> None:
    """Register behaviors that belong to the kernel rather than a domain."""
    behaviors.register(JOIN_MERGE_REF, JoinMergeBehavior(store, workspaces))
