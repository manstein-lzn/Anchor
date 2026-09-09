"""Workspace tools exposed to an agent's tool loop.

These are *native* tools: they run in the kernel rather than through the sandbox
gateway, because they mutate the workspace through the workspace manager's
ledger. Every write is therefore committed to the workspace branch and audited
exactly like any other workspace mutation.

The workspace is bound by node metadata (`workspace_id`); a node that does not
declare one cannot use these tools.
"""

from __future__ import annotations

import json
from collections.abc import Sequence

from anchor.domain.content import workspace_ref
from anchor.runtime.workspace import (
    WorkspaceError,
    WorkspaceResolver,
    execute_in_workspace,
)
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

    def execute(self, *, lease, tool_ref: str, arguments: dict) -> str:
        if not self.handles(tool_ref):
            raise WorkspaceError(f"not a workspace tool: {tool_ref}")
        workspace_id = self.workspace_id_for(lease)
        if tool_ref == "workspace.read":
            return self._read(workspace_id, arguments)
        if tool_ref == "workspace.write":
            return self._write(workspace_id, arguments)
        if tool_ref == "workspace.list":
            return self._list(workspace_id, arguments)
        return self._exec(workspace_id, arguments)

    # -- operations --------------------------------------------------------
    def _read(self, workspace_id: str, arguments: dict) -> str:
        path = self._path(arguments)
        workspace = self._workspace(workspace_id)
        revision = workspace.current_revision or workspace.base_revision
        return self.resolver.read_text(workspace_ref(workspace_id, revision, path))

    def _write(self, workspace_id: str, arguments: dict) -> str:
        path = self._path(arguments)
        content = arguments.get("content")
        if not isinstance(content, str):
            raise WorkspaceError("workspace.write requires a string 'content'")
        operation = self.workspaces.write_text(workspace_id, path, content,
                                               actor=f"agent:{workspace_id}")
        return json.dumps({"path": path, "revision": operation.after_revision},
                          ensure_ascii=False)

    def _list(self, workspace_id: str, arguments: dict) -> str:
        from anchor.runtime.workspace import GitWorkspaceBackend, resolve_source
        workspace = self._workspace(workspace_id)
        revision = workspace.current_revision or workspace.base_revision
        root, backend_name = resolve_source(self.store, workspace_id)
        if backend_name != "git":
            raise WorkspaceError(f"unsupported backend: {backend_name}")
        prefix = arguments.get("prefix")
        paths = GitWorkspaceBackend(root).list_paths(revision, prefix if isinstance(prefix, str) else None)
        return json.dumps({"revision": revision, "paths": paths}, ensure_ascii=False)

    def _exec(self, workspace_id: str, arguments: dict) -> str:
        if self.sandbox is None:
            raise WorkspaceError("workspace.exec requires a configured sandbox")
        command = arguments.get("command")
        if not isinstance(command, Sequence) or isinstance(command, str) or not command:
            raise WorkspaceError("workspace.exec requires a non-empty 'command' list")
        workspace = self._workspace(workspace_id)
        revision = workspace.current_revision or workspace.base_revision
        result = execute_in_workspace(
            self.store, workspace_ref(workspace_id, revision),
            [str(item) for item in command], sandbox=self.sandbox)
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
