"""Persistence for the prepared-revision window."""

from __future__ import annotations

from uuid import UUID

import sqlalchemy as sa

from anchor.domain.content_commit import PreparedRevision

from . import schema as s
from .base import _StoreHost, decode


class PreparedStoreMixin(_StoreHost):
    def record_prepared_revision(self, prepared: PreparedRevision) -> PreparedRevision:
        """Insert, or return the existing row for the same (node_run_id, attempt).

        Idempotent: preparing twice for one attempt must not overwrite the
        revision a reconciler may already be working from.
        """
        with self._transaction() as connection:
            existing = connection.execute(sa.select(s.prepared_revisions).where(
                s.prepared_revisions.c.node_run_id == str(prepared.node_run_id),
                s.prepared_revisions.c.attempt == prepared.attempt)).mappings().first()
            if existing is not None:
                return decode(PreparedRevision, existing)
            self._insert(connection, s.prepared_revisions, prepared)
        return prepared

    def get_prepared_revision(self, node_run_id: UUID, attempt: int) -> PreparedRevision | None:
        with self.engine.connect() as connection:
            row = connection.execute(sa.select(s.prepared_revisions).where(
                s.prepared_revisions.c.node_run_id == str(node_run_id),
                s.prepared_revisions.c.attempt == attempt)).mappings().first()
        return decode(PreparedRevision, row) if row is not None else None

    def list_prepared_revisions(self, limit: int = 200) -> list[PreparedRevision]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.prepared_revisions).order_by(
                s.prepared_revisions.c.created_at).limit(limit)).mappings()
            return [decode(PreparedRevision, row) for row in rows]

    def update_prepared_verification(self, node_run_id: UUID, attempt: int,
                                     verifier_result: dict) -> PreparedRevision:
        with self._transaction() as connection:
            connection.execute(sa.update(s.prepared_revisions).where(
                s.prepared_revisions.c.node_run_id == str(node_run_id),
                s.prepared_revisions.c.attempt == attempt).values(
                verifier_result=verifier_result))
        prepared = self.get_prepared_revision(node_run_id, attempt)
        if prepared is None:
            raise KeyError(node_run_id)
        return prepared

    def clear_prepared_revision(self, node_run_id: UUID, attempt: int) -> bool:
        with self._transaction() as connection:
            result = connection.execute(sa.delete(s.prepared_revisions).where(
                s.prepared_revisions.c.node_run_id == str(node_run_id),
                s.prepared_revisions.c.attempt == attempt))
        return bool(result.rowcount)

    def find_event(self, stream_id: UUID | str, idempotency_key: str) -> dict | None:
        with self.engine.connect() as connection:
            row = connection.execute(sa.select(s.events).where(
                s.events.c.stream_id == str(stream_id),
                s.events.c.idempotency_key == idempotency_key)).mappings().first()
        return dict(row) if row is not None else None
