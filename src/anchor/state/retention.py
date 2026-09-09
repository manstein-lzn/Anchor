"""Rolling retention: purge finished runs and reclaim their orphaned artifacts.

This is the only destructive path in the state layer. It is deliberately
separate from execution: it can only ever touch terminal runs, it refuses to
touch a run that is in flight or needs an operator, and every call is recorded
in ``retention_audit``.
"""

from __future__ import annotations

from uuid import UUID, uuid4

import sqlalchemy as sa

from . import schema as s
from .base import utc_now
from .errors import ConcurrencyConflict

TERMINAL_STATUSES = ("completed", "failed", "cancelled")
PROTECTED_NODE_STATUSES = ("waiting_approval", "waiting_event")

# Children before parents. tool/verification rows reference node_leases, and
# run_admissions references run_outbox.message_id, so those parents go last.
PURGE_ORDER = (
    "tool_operations", "verification_records", "progress_evidence",
    "diagnostic_requests", "node_leases", "context_snapshots",
    "edge_decisions", "node_runs", "run_admissions", "execution_inbox",
    "run_outbox",
)


class RetentionStoreMixin:
    def protected_run_ids(self) -> set[UUID]:
        """Runs a sweep must never touch, because work or a human is pending."""
        protected: set[UUID] = set()
        with self.engine.connect() as connection:
            protected |= {UUID(row[0]) for row in connection.execute(
                sa.select(s.runs.c.id).where(s.runs.c.status.not_in(TERMINAL_STATUSES))).all()}
            protected |= {UUID(row[0]) for row in connection.execute(
                sa.select(s.node_leases.c.run_id).where(
                    s.node_leases.c.released_at.is_(None))).all()}
            protected |= {UUID(row[0]) for row in connection.execute(
                sa.select(s.node_runs.c.run_id).where(
                    s.node_runs.c.status.in_(PROTECTED_NODE_STATUSES))).all()}
            protected |= {UUID(row[0]) for row in connection.execute(
                sa.select(s.tool_operations.c.run_id).where(
                    s.tool_operations.c.status == "outcome_unknown")).all()}
        return protected

    def purge_run(self, run_id: UUID) -> dict:
        """Delete one terminal run and its children. Irreversible."""
        with self._transaction() as connection:
            row = connection.execute(sa.select(s.runs).where(
                s.runs.c.id == str(run_id))).mappings().first()
            if row is None:
                return {"run_id": str(run_id), "deleted": False}
            if row["status"] not in TERMINAL_STATUSES:
                raise ConcurrencyConflict("only a terminal run can be purged")
            task_id = row["task_id"]
            for name in PURGE_ORDER:
                table = s.metadata.tables[name]
                connection.execute(sa.delete(table).where(table.c.run_id == str(run_id)))
            connection.execute(sa.delete(s.events).where(
                s.events.c.stream_id == str(run_id)))
            connection.execute(sa.delete(s.runs).where(s.runs.c.id == str(run_id)))
            remaining = connection.execute(sa.select(sa.func.count()).select_from(
                s.runs).where(s.runs.c.task_id == task_id)).scalar_one()
            if not remaining:
                connection.execute(sa.delete(s.tasks).where(s.tasks.c.id == task_id))
        return {"run_id": str(run_id), "deleted": True}

    def vacuum(self) -> None:
        """Reclaim file space after deletes. SQLite keeps freed pages otherwise."""
        if self.engine.url.get_backend_name() == "sqlite":
            with self.engine.connect() as connection:
                connection.exec_driver_sql("VACUUM")
        else:
            with self.engine.connect() as connection:
                connection.execution_options(isolation_level="AUTOCOMMIT").exec_driver_sql("VACUUM")

    def record_retention_audit(self, *, trigger: str, evicted_runs: int,
                               freed_bytes: int, detail: dict) -> dict:
        record = {"audit_id": str(uuid4()), "created_at": utc_now(), "trigger": trigger,
                  "evicted_runs": evicted_runs, "freed_bytes": freed_bytes, "detail": detail}
        with self._transaction() as connection:
            connection.execute(sa.insert(s.retention_audit).values(**record))
        return record

    def list_retention_audit(self, limit: int = 50) -> list[dict]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.retention_audit).order_by(
                s.retention_audit.c.created_at.desc()).limit(limit)).mappings()
            return [dict(row) for row in rows]
