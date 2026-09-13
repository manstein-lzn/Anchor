"""Workspace tools exposed to an agent's tool loop.

A node works in its own tree (ADR-054). Reads, writes and commands all land
there, so the node sees what it just wrote and can revise it, and nothing is
committed along the way — the freeze when the node finishes is the one revision.
That is deliberately not one revision per write: a node at work is not a sequence
of auditable transactions, and recording each keystroke as one would confuse the
ledger with a text editor's undo history.

The workspace is bound by node metadata (`workspace_id`); a node that does not
declare one cannot use these tools.
"""

from __future__ import annotations

import json
from collections.abc import Sequence
from pathlib import Path

from anchor.domain.content import parse
from anchor.runtime.sandbox import SandboxSpec
from anchor.runtime.workspace import WorkspaceError, WorkspaceResolver
from anchor.runtime.workspaces import WorkspaceManager, node_workspace_id

WORKSPACE_TOOLS = frozenset({
    "workspace.read", "workspace.write", "workspace.list", "workspace.exec",
})


class WorkspaceToolset:
    """Resolve and mutate the workspace bound to a node."""

    def __init__(self, store, workspaces: WorkspaceManager, *, sandbox=None) -> None:
        self.store = store
        self.workspaces = workspaces
        self.sandbox = sandbox
        self.resolver = WorkspaceResolver(store)

    def handles(self, tool_ref: str) -> bool:
        return tool_ref in WORKSPACE_TOOLS

    def workspace_id_for(self, lease) -> str:
        workspace_id = node_workspace_id(self.store, lease)
        if not workspace_id:
            raise WorkspaceError(
                f"node {lease.node_id!r} does not declare metadata.workspace_id")
        return workspace_id

    def execute(self, *, lease, tool_ref: str, arguments: dict, input_snapshot=None) -> str:
        if not self.handles(tool_ref):
            raise WorkspaceError(f"not a workspace tool: {tool_ref}")
        workspace_id = self.workspace_id_for(lease)
        if tool_ref == "workspace.read":
            return self._read(lease, workspace_id, arguments, input_snapshot)
        if tool_ref == "workspace.write":
            return self._write(lease, workspace_id, arguments, input_snapshot)
        if tool_ref == "workspace.list":
            return self._list(lease, workspace_id, arguments, input_snapshot)
        return self._exec(lease, workspace_id, arguments, input_snapshot)

    def declared_revision(self, lease, workspace_id: str, input_snapshot=None) -> str | None:
        """The revision the node declared in its input snapshot (I2/B1)."""
        candidates = [input_snapshot]
        persisted = self.store.get_context_snapshot(lease.node_run_id)
        candidates.append(persisted.snapshot if persisted is not None else None)
        for candidate in candidates:
            recorded = (candidate or {}).get("workspace") if isinstance(candidate, dict) else None
            if isinstance(recorded, str):
                ref = parse(recorded)
                if ref.workspace_id == workspace_id and ref.revision:
                    return ref.revision
        return None

    def pinned_revision(self, lease, workspace_id: str, input_snapshot=None) -> str:
        """What this node may observe.

        Its own writes are visible, because it owns the write claim and its
        lineage starts at the declared revision. Another node's in-flight writes
        are not: without the claim, the declared revision is authoritative.
        """
        workspace = self._workspace(workspace_id)
        if workspace.writer_node_run_id == str(lease.node_run_id) and workspace.current_revision:
            return workspace.current_revision
        declared = self.declared_revision(lease, workspace_id, input_snapshot)
        return declared or workspace.current_revision or workspace.base_revision

    # -- operations --------------------------------------------------------
    def _read(self, lease, workspace_id: str, arguments: dict, input_snapshot=None) -> str:
        """Read from the node's own tree, so it sees what it has already written."""
        path = self._path(arguments)
        target = self.workspaces.target(workspace_id, path, claimant=lease.node_run_id)
        if not target.is_file():
            raise WorkspaceError(f"no such file in the workspace: {path}")
        return target.read_text(encoding="utf-8")

    def _write(self, lease, workspace_id: str, arguments: dict, input_snapshot=None) -> str:
        """Write into the node's tree. Nothing is committed; the freeze is the revision."""
        path = self._path(arguments)
        content = arguments.get("content")
        if not isinstance(content, str):
            raise WorkspaceError("workspace.write requires a string 'content'")
        target = self.workspaces.target(workspace_id, path, claimant=lease.node_run_id)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        return json.dumps({"path": path, "bytes": len(content.encode("utf-8"))},
                          ensure_ascii=False)

    def _list(self, lease, workspace_id: str, arguments: dict, input_snapshot=None) -> str:
        """List the node's own tree."""
        workspace = self.workspaces.claim(workspace_id, lease.node_run_id)
        root = Path(workspace.path)
        raw_prefix = arguments.get("prefix")
        prefix: str = raw_prefix if isinstance(raw_prefix, str) else ""
        paths = []
        for candidate in sorted(root.rglob("*")):
            relative = candidate.relative_to(root)
            # `.git` is the workspace's own bookkeeping, not something a node wrote.
            if relative.parts and relative.parts[0] == ".git":
                continue
            if candidate.is_file() and str(relative).startswith(prefix):
                paths.append(str(relative))
        return json.dumps({"paths": paths}, ensure_ascii=False)

    def _exec(self, lease, workspace_id: str, arguments: dict, input_snapshot=None) -> str:
        """Run an allowlisted command in the node's tree, which the sandbox binds read-write.

        Bound to the live tree and not to a copy of a revision, because a place to work whose writes
        vanish is not a place to work. What the command may not do is unchanged: no network, and
        nothing outside this tree is writable.
        """
        if self.sandbox is None:
            raise WorkspaceError("workspace.exec requires a configured sandbox")
        command = arguments.get("command")
        if not isinstance(command, Sequence) or isinstance(command, str) or not command:
            raise WorkspaceError("workspace.exec requires a non-empty 'command' list")
        workspace = self.workspaces.claim(workspace_id, lease.node_run_id)
        result = self.sandbox.run(SandboxSpec(workspace=Path(workspace.path),
                                             command=tuple(str(item) for item in command)))
        return json.dumps({"returncode": result.returncode, "stdout": result.stdout,
                           "stderr": result.stderr, "timed_out": result.timed_out},
                          ensure_ascii=False)

    # -- helpers -----------------------------------------------------------
    def _workspace(self, workspace_id: str):
        workspace = self.store.get_workspace(workspace_id)
        if workspace is None:
            raise WorkspaceError(f"unknown workspace: {workspace_id}")
        return workspace

    @staticmethod
    def _path(arguments: dict) -> str:
        path = arguments.get("path")
        if not isinstance(path, str) or not path:
            raise WorkspaceError("a non-empty 'path' argument is required")
        return path
