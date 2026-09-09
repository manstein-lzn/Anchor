"""Workspace persistence: lifecycle, lineage and the operation ledger."""

from __future__ import annotations

import sqlalchemy as sa

from anchor.domain.workspace import Workspace, WorkspaceOperation, WorkspaceState

from . import schema as s
from .base import _StoreHost, decode, utc_now


class WorkspaceStoreMixin(_StoreHost):
    def create_workspace(self, workspace: Workspace) -> Workspace:
        with self._transaction() as connection:
            self._insert(connection, s.workspaces, workspace)
        return workspace

    def get_workspace(self, workspace_id: str) -> Workspace | None:
        with self.engine.connect() as connection:
            row = connection.execute(sa.select(s.workspaces).where(
                s.workspaces.c.workspace_id == workspace_id)).mappings().first()
        return decode(Workspace, row) if row is not None else None

    def list_workspaces(self, *, project_id: str | None = None,
                        state: WorkspaceState | None = None) -> list[Workspace]:
        with self.engine.connect() as connection:
            query = sa.select(s.workspaces)
            if project_id is not None:
                query = query.where(s.workspaces.c.project_id == project_id)
            if state is not None:
                query = query.where(s.workspaces.c.state == state.value)
            rows = connection.execute(query.order_by(s.workspaces.c.created_at)).mappings()
            return [decode(Workspace, row) for row in rows]

    def update_workspace_state(self, workspace_id: str, *, state: WorkspaceState,
                               current_revision: str | None = None) -> Workspace:
        with self._transaction() as connection:
            row = connection.execute(sa.select(s.workspaces).where(
                s.workspaces.c.workspace_id == workspace_id).with_for_update()).mappings().first()
            if row is None:
                raise KeyError(workspace_id)
            values = {"state": state.value, "updated_at": utc_now()}
            if current_revision is not None:
                values["current_revision"] = current_revision
            connection.execute(sa.update(s.workspaces).where(
                s.workspaces.c.workspace_id == workspace_id).values(**values))
        return self.get_workspace(workspace_id)  # type: ignore[return-value]

    def record_workspace_operation(self, operation: WorkspaceOperation) -> WorkspaceOperation:
        """Persist the ledger entry and its event atomically.

        The workspace stream is the audit trail for every mutation; a ledger row
        without its event, or an event without its row, would be a hole in it.
        """
        with self._transaction() as connection:
            self._insert(connection, s.workspace_operations, operation)
            self._append_event(
                connection, stream_id=operation.workspace_id,
                event_type=f"workspace.{operation.kind.value}",
                payload={"operation_id": str(operation.operation_id),
                         "path": operation.path,
                         "before_revision": operation.before_revision,
                         "after_revision": operation.after_revision,
                         "content_hash": operation.content_hash,
                         "actor": operation.actor},
                idempotency_key=f"workspace:{operation.operation_id}")
        return operation

    def list_workspace_operations(self, workspace_id: str) -> list[WorkspaceOperation]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.workspace_operations).where(
                s.workspace_operations.c.workspace_id == workspace_id).order_by(
                s.workspace_operations.c.created_at)).mappings()
            return [decode(WorkspaceOperation, row) for row in rows]
