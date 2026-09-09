"""Tool operation ledger: register, start, finish, reconcile, replay."""

from __future__ import annotations

from typing import Callable
from uuid import UUID

import sqlalchemy as sa

from anchor.domain.models import NodeRun, utc_now
from anchor.domain.operations import OperationStatus, ToolOperation
from . import schema as s
from .base import _StoreHost, decode
from .errors import OperationConflict


class OperationStoreMixin(_StoreHost):
    """Ledger-first external side-effect records."""
    def register_tool_operation(self, operation: ToolOperation) -> ToolOperation:
        operation = ToolOperation.model_validate(operation.model_dump(mode="json"))
        if operation.status is not OperationStatus.REGISTERED:
            raise ValueError("new operation must be registered")
        with self._transaction() as connection:
            self._lock(connection, f"operation:{operation.operation_id}")
            prior = connection.execute(sa.select(s.tool_operations).where(
                s.tool_operations.c.operation_id == str(operation.operation_id))).mappings().first()
            if prior:
                stored = decode(ToolOperation, prior)
                if stored != operation:
                    raise OperationConflict("operation identity already has different content")
                return stored
            lease = connection.execute(sa.select(s.node_leases).where(
                s.node_leases.c.claim_id == str(operation.claim_id)).with_for_update()).mappings().first()
            if lease is None:
                raise KeyError(operation.claim_id)
            if lease["released_at"] is not None:
                raise OperationConflict("operation lease is released")
            if (lease["node_run_id"], lease["run_id"]) != (str(operation.node_run_id), str(operation.run_id)):
                raise OperationConflict("operation does not match its lease")
            self._insert(connection, s.tool_operations, operation)
            sequence = self._append_event(connection, stream_id=operation.run_id,
                event_type="operation.registered",
                payload={"operation_id": str(operation.operation_id), "claim_id": str(operation.claim_id),
                         "node_run_id": str(operation.node_run_id), "tool_ref": operation.tool_ref,
                         "request_hash": operation.request_hash},
                idempotency_key=f"operation:{operation.operation_id}:registered")
            self._advance_run_event_pointer(connection, operation.run_id, sequence)
            return operation
    def start_tool_operation(self, operation_id: UUID, claim_id: UUID) -> ToolOperation:
        with self._transaction() as connection:
            self._lock(connection, f"operation:{operation_id}")
            row = connection.execute(sa.select(s.tool_operations).where(
                s.tool_operations.c.operation_id == str(operation_id)).with_for_update()).mappings().first()
            if row is None:
                raise KeyError(operation_id)
            operation = decode(ToolOperation, row)
            if operation.claim_id != claim_id:
                raise OperationConflict("operation belongs to another lease")
            if operation.status is OperationStatus.RUNNING:
                return operation
            if operation.status is not OperationStatus.REGISTERED:
                raise OperationConflict("terminal operation cannot be started again")
            now = utc_now()
            connection.execute(sa.update(s.tool_operations).where(
                s.tool_operations.c.operation_id == str(operation_id)).values(
                status=OperationStatus.RUNNING.value, updated_at=now))
            sequence = self._append_event(connection, stream_id=operation.run_id,
                event_type="operation.running",
                payload={"operation_id": str(operation_id), "claim_id": str(claim_id)},
                idempotency_key=f"operation:{operation_id}:running")
            self._advance_run_event_pointer(connection, operation.run_id, sequence)
            return decode(ToolOperation, connection.execute(sa.select(s.tool_operations).where(
                s.tool_operations.c.operation_id == str(operation_id))).mappings().one())
    def finish_tool_operation(self, operation_id: UUID, claim_id: UUID, *, status: OperationStatus,
                              result_ref: str | None = None, error_code: str | None = None) -> ToolOperation:
        if status not in {OperationStatus.SUCCEEDED, OperationStatus.FAILED, OperationStatus.OUTCOME_UNKNOWN}:
            raise ValueError("finish status must be a terminal operation status")
        with self._transaction() as connection:
            self._lock(connection, f"operation:{operation_id}")
            row = connection.execute(sa.select(s.tool_operations).where(
                s.tool_operations.c.operation_id == str(operation_id)).with_for_update()).mappings().first()
            if row is None:
                raise KeyError(operation_id)
            operation = decode(ToolOperation, row)
            if operation.claim_id != claim_id:
                raise OperationConflict("operation belongs to another lease")
            candidate = operation.model_copy(update={"status": status, "result_ref": result_ref,
                                                       "error_code": error_code, "updated_at": utc_now()})
            candidate = ToolOperation.model_validate(candidate.model_dump(mode="json"))
            if operation.status is status:
                if operation.result_ref != result_ref or operation.error_code != error_code:
                    raise OperationConflict("terminal operation outcome conflicts with stored result")
                return operation
            if operation.status is not OperationStatus.RUNNING:
                raise OperationConflict("only a running operation can record an outcome")
            connection.execute(sa.update(s.tool_operations).where(
                s.tool_operations.c.operation_id == str(operation_id)).values(
                status=status.value, result_ref=result_ref, error_code=error_code,
                updated_at=candidate.updated_at))
            sequence = self._append_event(connection, stream_id=operation.run_id,
                event_type=f"operation.{status.value}",
                payload={"operation_id": str(operation_id), "claim_id": str(claim_id),
                         "result_ref": result_ref, "error_code": error_code},
                idempotency_key=f"operation:{operation_id}:{status.value}")
            self._advance_run_event_pointer(connection, operation.run_id, sequence)
            return candidate
    def reconcile_tool_operation(self, operation_id: UUID, *, status: OperationStatus,
                                 reconciliation_ref: str, result_ref: str | None = None,
                                 error_code: str | None = None) -> ToolOperation:
        if status not in {OperationStatus.SUCCEEDED, OperationStatus.FAILED}:
            raise ValueError("reconciliation must resolve to succeeded or failed")
        if not reconciliation_ref:
            raise ValueError("reconciliation_ref is required")
        with self._transaction() as connection:
            self._lock(connection, f"operation:{operation_id}")
            row = connection.execute(sa.select(s.tool_operations).where(
                s.tool_operations.c.operation_id == str(operation_id)).with_for_update()).mappings().first()
            if row is None:
                raise KeyError(operation_id)
            operation = decode(ToolOperation, row)
            if operation.reconciliation_ref is not None:
                if (operation.status, operation.reconciliation_ref, operation.result_ref, operation.error_code) != (
                    status, reconciliation_ref, result_ref, error_code):
                    raise OperationConflict("reconciliation conflicts with stored evidence")
                return operation
            if operation.status is not OperationStatus.OUTCOME_UNKNOWN:
                raise OperationConflict("only an unknown outcome can be reconciled")
            candidate = operation.model_copy(update={"status": status, "result_ref": result_ref,
                "error_code": error_code, "reconciliation_ref": reconciliation_ref, "updated_at": utc_now()})
            candidate = ToolOperation.model_validate(candidate.model_dump(mode="json"))
            connection.execute(sa.update(s.tool_operations).where(
                s.tool_operations.c.operation_id == str(operation_id)).values(status=status.value,
                result_ref=result_ref, error_code=error_code, reconciliation_ref=reconciliation_ref,
                updated_at=candidate.updated_at))
            sequence = self._append_event(connection, stream_id=operation.run_id,
                event_type="operation.reconciled",
                payload={"operation_id": str(operation_id), "from_status": OperationStatus.OUTCOME_UNKNOWN.value,
                         "to_status": status.value, "reconciliation_ref": reconciliation_ref,
                         "result_ref": result_ref, "error_code": error_code},
                idempotency_key=f"operation:{operation_id}:reconciled")
            self._advance_run_event_pointer(connection, operation.run_id, sequence)
            return candidate
    def get_tool_operation(self, operation_id: UUID) -> ToolOperation | None:
        return self._read(s.tool_operations, ToolOperation, operation_id)

    def resolve_reconciled_operation(self, operation_id: UUID, *, actor: str,
                                     reason: str,
                                     read_artifact: Callable[[str], str] | None = None) -> "NodeRun":
        """Apply a reconciled operation outcome to its owning node.

        The operator first reconciles the operation with external evidence; this
        step is the deterministic consequence: a reconciled success completes the
        node with the operation's result artifact, a reconciled failure fails it.
        Only an unreleased lease on a running node is eligible, so an active
        worker can never race the operator.
        """
        from anchor.domain.conditions import build_condition_context
        if not actor or len(actor) > 200:
            raise ValueError("reconcile actor is required and must be at most 200 characters")
        if not reason or len(reason) > 2000:
            raise ValueError("reconcile reason is required and must be at most 2000 characters")
        operation = self.get_tool_operation(operation_id)
        if operation is None:
            raise KeyError(operation_id)
        if operation.status not in (OperationStatus.SUCCEEDED, OperationStatus.FAILED):
            raise OperationConflict("only a reconciled operation can be applied to its node")
        if not operation.reconciliation_ref:
            raise OperationConflict("operation has no reconciliation evidence")
        lease = next((item for item in self.list_active_leases()
                      if item.claim_id == operation.claim_id), None)
        if lease is None:
            raise OperationConflict("operation lease is no longer active")
        if operation.status is OperationStatus.FAILED:
            return self.fail_node_and_propagate(
                lease.claim_id, lease.worker_id,
                error_code=operation.error_code or "reconciled_failure", phase="reconciliation")
        text = self._read_operation_result(operation, read_artifact)
        snapshot = self._latest_snapshot(lease.node_run_id)
        self.complete_node_and_propagate(
            lease.claim_id, lease.worker_id, output_ref=operation.result_ref,
            input_snapshot=snapshot,
            condition_context=build_condition_context(text, snapshot))
        return next(item for item in self.list_node_runs(operation.run_id)
                    if item.id == operation.node_run_id)

    def _read_operation_result(self, operation: ToolOperation,
                               read_artifact: Callable[[str], str] | None) -> str:
        """Read the reconciled result through an injected reader.

        The state layer must not know about artifact storage or settings; the
        composition root (API/worker) supplies the reader.
        """
        if read_artifact is None:
            raise ValueError("resolving a reconciled success requires an artifact reader")
        if not operation.result_ref:
            raise ValueError("reconciled operation has no result reference")
        return read_artifact(operation.result_ref)

    def _latest_snapshot(self, node_run_id: UUID) -> dict:
        snapshot = self.get_context_snapshot(node_run_id)
        return dict(snapshot.snapshot) if snapshot is not None else {}

    def list_tool_operations(self, run_id: UUID) -> list[ToolOperation]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.tool_operations).where(
                s.tool_operations.c.run_id == str(run_id)).order_by(
                s.tool_operations.c.created_at, s.tool_operations.c.operation_id)).mappings()
            return [decode(ToolOperation, row) for row in rows]
