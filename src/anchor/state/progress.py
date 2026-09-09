"""Durable progress observations and diagnostic requests."""

from __future__ import annotations

from uuid import UUID

import sqlalchemy as sa

from anchor.domain.models import DiagnosticRequest, ProgressEvidence
from . import schema as s
from .base import decode


class ProgressStoreMixin:
    """Observation records; never a failure verdict."""
    def append_progress_evidence(self, evidence: ProgressEvidence) -> ProgressEvidence:
        with self._transaction() as connection:
            stored = self._insert(connection, s.progress_evidence, evidence)
            self._append_event(
                connection,
                stream_id=evidence.run_id,
                event_type="progress.evidence",
                payload=stored.model_dump(mode="json", exclude={"run_id", "node_run_id"}),
                idempotency_key=f"evidence:{evidence.evidence_id}",
            )
            return stored
    def list_progress_evidence(self, run_id: UUID) -> list[ProgressEvidence]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.progress_evidence).where(
                s.progress_evidence.c.run_id == str(run_id)).order_by(
                s.progress_evidence.c.state_revision, s.progress_evidence.c.evidence_id)).mappings()
            return [decode(ProgressEvidence, row) for row in rows]
    def add_diagnostic_request(self, request: DiagnosticRequest) -> DiagnosticRequest:
        with self._transaction() as connection:
            stored = self._insert(connection, s.diagnostic_requests, request)
            self._append_event(
                connection,
                stream_id=request.run_id,
                event_type="diagnostic.requested",
                payload=stored.model_dump(mode="json", exclude={"run_id", "node_run_id"}),
                idempotency_key=f"diagnostic:{stored.diagnostic_id}",
            )
            return stored
    def list_open_diagnostics(self, run_id: UUID) -> list[DiagnosticRequest]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.diagnostic_requests).where(
                s.diagnostic_requests.c.run_id == str(run_id),
                s.diagnostic_requests.c.status == "open").order_by(
                s.diagnostic_requests.c.created_at, s.diagnostic_requests.c.diagnostic_id)).mappings()
            return [decode(DiagnosticRequest, row) for row in rows]
    def supersede_diagnostic(self, diagnostic_id: str, *, superseded_by: str) -> DiagnosticRequest:
        with self._transaction() as connection:
            row = connection.execute(sa.select(s.diagnostic_requests).where(
                s.diagnostic_requests.c.diagnostic_id == diagnostic_id).with_for_update()).mappings().first()
            if row is None:
                raise KeyError(diagnostic_id)
            connection.execute(sa.update(s.diagnostic_requests).where(
                s.diagnostic_requests.c.diagnostic_id == diagnostic_id).values(
                status="superseded", superseded_by=superseded_by))
            self._append_event(
                connection,
                stream_id=row["run_id"],
                event_type="diagnostic.superseded",
                payload={"diagnostic_id": diagnostic_id, "superseded_by": superseded_by},
                idempotency_key=f"diagnostic:{diagnostic_id}:superseded",
            )
            return decode(DiagnosticRequest, connection.execute(sa.select(s.diagnostic_requests).where(
                s.diagnostic_requests.c.diagnostic_id == diagnostic_id)).mappings().one())
