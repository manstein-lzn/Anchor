"""Node runs, leases, claims, verification records and edge decisions."""

from __future__ import annotations

from uuid import UUID

import sqlalchemy as sa

from anchor.domain.graph import (CONTROL_NODE_TYPES, RECOVERABLE_CONTROL_TYPES,
                                 GraphVersion, NodeType)
from anchor.domain.models import (ContextSnapshot, EdgeDecision, NodeLease, NodeRun,
                                  Run, RunStatus, TaskStatus, VerificationRecord,
                                  VerificationVerdict, utc_now)
from . import schema as s
from .base import _StoreHost, decode
from .errors import ConcurrencyConflict


class ExecutionStoreMixin(_StoreHost):
    """Run/node state machine queries, leases and verification persistence."""
    def stop_run(self, run_id: UUID, *, reason: str, budget_exceeded: bool = False) -> Run:
        """Fence further execution without deleting artifacts or operation history."""
        with self._transaction() as connection:
            row = connection.execute(sa.select(s.runs).where(s.runs.c.id == str(run_id)).with_for_update()).mappings().first()
            if row is None:
                raise KeyError(run_id)
            if row["status"] in ("completed", "failed", "cancelled"):
                return decode(Run, row)
            now = utc_now()
            status = "failed" if budget_exceeded else "cancelled"
            phase = "execution_budget_exceeded" if budget_exceeded else "operator_stop"
            seq = self._append_event(connection, stream_id=run_id, event_type=f"run.{status}",
                payload={"reason": reason, "phase": phase}, idempotency_key=f"stop:{run_id}")
            connection.execute(sa.update(s.runs).where(s.runs.c.id == str(run_id)).values(
                status=status, current_phase=phase, revision=row["revision"] + 1,
                last_event_sequence=seq, updated_at=now))
            connection.execute(sa.update(s.node_runs).where(s.node_runs.c.run_id == str(run_id),
                s.node_runs.c.status.not_in(["completed", "failed", "cancelled", "skipped"])).values(
                status="cancelled", error_code=phase, revision=s.node_runs.c.revision + 1, updated_at=now))
            # Retain lease history so in-flight tool results remain auditable.
            connection.execute(sa.update(s.node_leases).where(s.node_leases.c.run_id == str(run_id),
                s.node_leases.c.released_at.is_(None)).values(released_at=now))
            self._append_event(connection, stream_id=UUID(row["task_id"]), event_type=f"task.{status}",
                payload={"run_id": str(run_id), "reason": reason}, idempotency_key=f"stop:{run_id}")
            connection.execute(sa.update(s.tasks).where(s.tasks.c.id == row["task_id"]).values(
                status=status, revision=s.tasks.c.revision + 1, updated_at=now))
            return decode(Run, connection.execute(sa.select(s.runs).where(s.runs.c.id == str(run_id))).mappings().one())
    def pause_run(self, run_id: UUID, *, reason: str, actor: str) -> Run:
        """Stop accepting new claims without touching in-flight work or artifacts.

        A paused run cannot be claimed (`claim_*` only accepts queued/running),
        so already-running nodes finish and their downstream stays ready until
        the run is resumed. Idempotent per run.
        """
        if not reason or len(reason) > 2000:
            raise ValueError("pause reason is required and must be at most 2000 characters")
        if not actor or len(actor) > 200:
            raise ValueError("pause actor is required and must be at most 200 characters")
        with self._transaction() as connection:
            row = connection.execute(sa.select(s.runs).where(
                s.runs.c.id == str(run_id)).with_for_update()).mappings().first()
            if row is None:
                raise KeyError(run_id)
            if row["status"] == RunStatus.PAUSED.value:
                return decode(Run, row)
            if row["status"] not in (RunStatus.QUEUED.value, RunStatus.RUNNING.value):
                raise ConcurrencyConflict("only a queued or running run can be paused")
            now = utc_now()
            seq = self._append_event(connection, stream_id=run_id, event_type="run.paused",
                payload={"reason": reason, "actor": actor},
                idempotency_key=f"run:{run_id}:paused")
            connection.execute(sa.update(s.runs).where(s.runs.c.id == str(run_id)).values(
                status=RunStatus.PAUSED.value, current_phase="paused",
                revision=row["revision"] + 1, last_event_sequence=seq, updated_at=now))
            return decode(Run, connection.execute(sa.select(s.runs).where(
                s.runs.c.id == str(run_id))).mappings().one())

    def resume_run(self, run_id: UUID, *, reason: str, actor: str) -> Run:
        """Return a paused run to running so workers can claim ready nodes again."""
        if not reason or len(reason) > 2000:
            raise ValueError("resume reason is required and must be at most 2000 characters")
        if not actor or len(actor) > 200:
            raise ValueError("resume actor is required and must be at most 200 characters")
        with self._transaction() as connection:
            row = connection.execute(sa.select(s.runs).where(
                s.runs.c.id == str(run_id)).with_for_update()).mappings().first()
            if row is None:
                raise KeyError(run_id)
            if row["status"] != RunStatus.PAUSED.value:
                raise ConcurrencyConflict("only a paused run can be resumed")
            now = utc_now()
            seq = self._append_event(connection, stream_id=run_id, event_type="run.resumed",
                payload={"reason": reason, "actor": actor},
                idempotency_key=f"run:{run_id}:resumed")
            connection.execute(sa.update(s.runs).where(s.runs.c.id == str(run_id)).values(
                status=RunStatus.RUNNING.value, current_phase="node.execute",
                revision=row["revision"] + 1, last_event_sequence=seq, updated_at=now))
            return decode(Run, connection.execute(sa.select(s.runs).where(
                s.runs.c.id == str(run_id))).mappings().one())

    def set_run_archived(self, run_id: UUID, *, archived: bool) -> Run:
        """Hide or restore a terminal run in operator listings.

        Evidence is never destroyed: the run, its nodes, events, decisions,
        verifications, operations and memory all stay queryable by id. Only a
        terminal run can be archived so an active lease cannot be hidden from
        supervision. Idempotent in both directions.
        """
        with self._transaction() as connection:
            row = connection.execute(sa.select(s.runs).where(
                s.runs.c.id == str(run_id)).with_for_update()).mappings().first()
            if row is None:
                raise KeyError(run_id)
            if archived and row["status"] not in ("completed", "failed", "cancelled"):
                raise ConcurrencyConflict("only a terminal run can be archived")
            if (row["archived_at"] is not None) == archived:
                return decode(Run, row)
            now = utc_now()
            revision = row["revision"] + 1
            event_type = "run.archived" if archived else "run.unarchived"
            seq = self._append_event(connection, stream_id=run_id, event_type=event_type,
                payload={"archived": archived},
                idempotency_key=f"run:{run_id}:{event_type}:{revision}")
            connection.execute(sa.update(s.runs).where(s.runs.c.id == str(run_id)).values(
                archived_at=now if archived else None, revision=revision,
                last_event_sequence=seq, updated_at=now))
            return decode(Run, connection.execute(sa.select(s.runs).where(
                s.runs.c.id == str(run_id))).mappings().one())

    def expire_run_budgets(self) -> int:
        with self.engine.connect() as connection:
            rows = list(connection.execute(sa.select(s.runs).where(
                s.runs.c.status.not_in(["completed", "failed", "cancelled"]))).mappings())
        expired = 0
        for row in rows:
            run = decode(Run, row)
            graph = self.get_graph_version(run.graph_version_id)
            budget = graph.definition.metadata.get("run_timeout_seconds") if graph else None
            if budget and (utc_now() - run.created_at).total_seconds() >= float(budget):
                self.stop_run(run.id, reason="Run time budget exhausted; partial artifacts retained", budget_exceeded=True)
                expired += 1
        return expired
    def create_node_run(self, node_run: NodeRun) -> NodeRun:
        with self._transaction() as connection:
            definition = connection.execute(sa.select(s.graph_versions.c.definition).join(
                s.runs, s.runs.c.graph_version_id == s.graph_versions.c.graph_version_id
            ).where(s.runs.c.id == str(node_run.run_id))).scalar_one_or_none()
            if definition is None:
                raise KeyError(node_run.run_id)
            if node_run.node_id not in {node["id"] for node in definition["nodes"]}:
                raise ValueError("node does not belong to the run's graph version")
            return self._insert(connection, s.node_runs, node_run)
    def list_node_runs(self, run_id: UUID) -> list[NodeRun]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.node_runs).where(
                s.node_runs.c.run_id == str(run_id)).order_by(s.node_runs.c.node_id, s.node_runs.c.attempt)).mappings()
            return [decode(NodeRun, row) for row in rows]
    def list_edge_decisions(self, run_id: UUID) -> list[EdgeDecision]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.edge_decisions).where(
                s.edge_decisions.c.run_id == str(run_id)).order_by(
                s.edge_decisions.c.edge_index)).mappings()
            return [decode(EdgeDecision, row) for row in rows]
    def _persist_edge_decision(self, connection, decision: EdgeDecision) -> int:
        prior = connection.execute(sa.select(s.edge_decisions).where(
            s.edge_decisions.c.run_id == str(decision.run_id),
            s.edge_decisions.c.edge_index == decision.edge_index,
            s.edge_decisions.c.source_attempt == decision.source_attempt,
        )).mappings().first()
        if prior is not None:
            stored = decode(EdgeDecision, prior)
            if stored.model_dump(mode="json", exclude={"decided_at"}) != decision.model_dump(
                mode="json", exclude={"decided_at"}
            ):
                raise ConcurrencyConflict("edge already has a different routing decision")
        else:
            self._insert(connection, s.edge_decisions, decision)
        return self._append_event(
            connection,
            stream_id=decision.run_id,
            event_type="edge.decided",
            payload={
                "edge_index": decision.edge_index,
                "source_attempt": decision.source_attempt,
                "source_node_id": decision.source_node_id,
                "target_node_id": decision.target_node_id,
                "selected": decision.selected,
                "reason": decision.reason.value,
                "condition": decision.condition,
                "evaluator": decision.evaluator,
                "evaluator_version": decision.evaluator_version,
                "evaluation_context_hash": decision.evaluation_context_hash,
                "evidence_ref": decision.evidence_ref,
            },
            idempotency_key=(f"edge:{decision.edge_index}:decided"
                if decision.source_attempt == 0
                else f"edge:{decision.edge_index}:{decision.source_attempt}:decided"),
        )
    def get_context_snapshot(self, node_run_id: UUID) -> ContextSnapshot | None:
        with self.engine.connect() as connection:
            row = connection.execute(sa.select(s.context_snapshots).where(
                s.context_snapshots.c.node_run_id == str(node_run_id)).order_by(
                s.context_snapshots.c.generation.desc(), s.context_snapshots.c.id.desc()).limit(1)).mappings().first()
            return decode(ContextSnapshot, row)
    def list_context_snapshots(self, run_id: UUID) -> list[ContextSnapshot]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.context_snapshots).where(
                s.context_snapshots.c.run_id == str(run_id)).order_by(
                s.context_snapshots.c.generation, s.context_snapshots.c.id)).mappings()
            return [decode(ContextSnapshot, row) for row in rows]
    def list_verifications(self, run_id: UUID) -> list[VerificationRecord]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.verification_records).where(
                s.verification_records.c.run_id == str(run_id)).order_by(
                s.verification_records.c.decided_at,
                s.verification_records.c.verification_id)).mappings()
            return [decode(VerificationRecord, row) for row in rows]
    def _persist_verification(self, connection, verification: VerificationRecord) -> int:
        self._insert(connection, s.verification_records, verification)
        return self._append_event(
            connection,
            stream_id=verification.run_id,
            event_type="verification.decided",
            payload={
                "verification_id": str(verification.verification_id),
                "node_id": verification.node_id,
                "verifier_ref": verification.verifier_ref,
                "verifier_version": verification.verifier_version,
                "adapter": verification.adapter,
                "adapter_version": verification.adapter_version,
                "verdict": verification.verdict.value,
                "reason": verification.reason,
                "evidence_ref": verification.evidence_ref,
                "verified_artifact_hashes": verification.verified_artifact_hashes,
                "verified_context_hash": verification.verified_context_hash,
            },
            idempotency_key=f"verification:{verification.verification_id}:decided",
        )
    @staticmethod
    def _validate_verification(
        verification: VerificationRecord | None,
        *,
        lease,
        graph_node,
        expected_verdicts: frozenset[VerificationVerdict],
        context_hash: str | None,
    ) -> None:
        if graph_node.type is NodeType.VERIFIER and verification is None:
            raise ConcurrencyConflict("verifier transition requires a persisted verification verdict")
        if graph_node.type is not NodeType.VERIFIER and verification is not None:
            raise ValueError("verification evidence can only be attached to a verifier node")
        if verification is None:
            return
        identity = (
            verification.claim_id == UUID(lease["claim_id"])
            and verification.run_id == UUID(lease["run_id"])
            and verification.node_run_id == UUID(lease["node_run_id"])
            and verification.node_id == lease["node_id"]
            and verification.verifier_ref == graph_node.verifier_ref
        )
        if not identity:
            raise ConcurrencyConflict("verification evidence does not match its claimed verifier node")
        if verification.verdict not in expected_verdicts:
            raise ConcurrencyConflict("verification verdict is not valid for this transition")
        if context_hash is None or verification.verified_context_hash != context_hash:
            raise ConcurrencyConflict("verification context hash does not match the persisted snapshot")
    def transition_run(self, *, run_id, expected_revision, status, phase, payload, idempotency_key) -> Run:
        with self._transaction() as connection:
            self._lock(connection, f"stream:{run_id}")
            row = connection.execute(sa.select(s.runs).where(s.runs.c.id == str(run_id))).mappings().first()
            if row is None:
                raise KeyError(run_id)
            if row["revision"] != expected_revision:
                raise ConcurrencyConflict(run_id)
            sequence = self._append_event(connection, stream_id=run_id, event_type=f"run.{status.value}",
                                          payload={"phase": phase, **payload}, idempotency_key=idempotency_key)
            connection.execute(sa.update(s.runs).where(s.runs.c.id == str(run_id)).values(
                status=status.value, current_phase=phase, revision=expected_revision + 1,
                last_event_sequence=sequence, updated_at=utc_now()))
            return decode(Run, connection.execute(sa.select(s.runs).where(s.runs.c.id == str(run_id))).mappings().one())
    def list_active_leases(self) -> list[NodeLease]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.node_leases).where(
                s.node_leases.c.released_at.is_(None)).order_by(s.node_leases.c.heartbeat_at)).mappings().all()
            return [decode(NodeLease, row) for row in rows]
    def claim_ready_node(self, worker_id: str, claim_id: UUID) -> NodeLease | None:
        """Generic claim across every executable node type (tests, admin paths)."""
        return self._claim_ready_typed_node(worker_id, claim_id, frozenset(NodeType))

    def _claim_ready_typed_node(self, worker_id: str, claim_id: UUID,
                                allowed_types: frozenset[NodeType]) -> NodeLease | None:
        """Claim only a ready node owned by one executor class.

        Unsupported ready nodes stay pending for their future executor. A
        candidate is locked only after its pinned graph is inspected, so a
        PostgreSQL worker does not accidentally claim a verifier/tool row.
        """
        if not worker_id or len(worker_id) > 128:
            raise ValueError("worker_id must contain 1 to 128 characters")
        with self._transaction() as connection:
            self._lock(connection, f"claim:{claim_id}")
            prior = connection.execute(sa.select(s.node_leases).where(
                s.node_leases.c.claim_id == str(claim_id))).mappings().first()
            if prior:
                if prior["worker_id"] != worker_id:
                    raise ConcurrencyConflict("claim identity belongs to another worker")
                return decode(NodeLease, prior)
            candidates = connection.execute(sa.select(s.node_runs).where(
                s.node_runs.c.status == "ready",
                sa.or_(s.node_runs.c.next_attempt_at.is_(None),
                       s.node_runs.c.next_attempt_at <= utc_now()),
            ).order_by(
                    s.node_runs.c.created_at, s.node_runs.c.id)).mappings().all()
            node = None
            graph = None
            for candidate in candidates:
                candidate_run = connection.execute(sa.select(s.runs).where(
                    s.runs.c.id == candidate["run_id"])).mappings().first()
                if candidate_run is None or candidate_run["status"] not in {RunStatus.QUEUED.value, RunStatus.RUNNING.value}:
                    continue
                candidate_graph = connection.execute(sa.select(s.graph_versions).where(
                    s.graph_versions.c.graph_version_id == candidate_run["graph_version_id"])).mappings().first()
                if candidate_graph is None:
                    continue
                decoded_graph = decode(GraphVersion, candidate_graph)
                graph_node = next((item for item in decoded_graph.definition.nodes if item.id == candidate["node_id"]), None)
                if graph_node is not None and graph_node.type in allowed_types:
                    lock_query = sa.select(s.node_runs).where(
                        s.node_runs.c.id == candidate["id"], s.node_runs.c.status == "ready").with_for_update()
                    if self.engine.dialect.name == "postgresql":
                        lock_query = lock_query.with_for_update(skip_locked=True)
                    locked = connection.execute(lock_query).mappings().first()
                    if locked is not None:
                        node, graph = locked, decoded_graph
                        break
            if node is None or graph is None:
                return None
            run = connection.execute(sa.select(s.runs).where(
                s.runs.c.id == node["run_id"]).with_for_update()).mappings().one()
            if run["status"] not in {RunStatus.QUEUED.value, RunStatus.RUNNING.value}:
                return None
            now = utc_now()
            lease = NodeLease(claim_id=claim_id, node_run_id=node["id"], run_id=run["id"],
                              node_id=node["node_id"], worker_id=worker_id,
                              acquired_at=now, heartbeat_at=now)
            self._insert(connection, s.node_leases, lease)
            sequence = self._append_event(connection, stream_id=run["id"], event_type="node.claimed",
                payload={"node_id": node["node_id"], "attempt": node["attempt"],
                         "claim_id": str(claim_id), "worker_id": worker_id},
                idempotency_key=f"claim:{claim_id}")
            connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id == node["id"]).values(
                status="running", revision=node["revision"] + 1, updated_at=now))
            if run["status"] == RunStatus.QUEUED.value:
                sequence = self._append_event(connection, stream_id=run["id"], event_type="run.running",
                    payload={"phase": "node.execute", "claim_id": str(claim_id), "node_id": node["node_id"]},
                    idempotency_key=f"claim:{claim_id}:run-running")
                connection.execute(sa.update(s.runs).where(s.runs.c.id == run["id"]).values(
                    status=RunStatus.RUNNING.value, current_phase="node.execute",
                    revision=run["revision"] + 1, last_event_sequence=sequence, updated_at=now))
                task = connection.execute(sa.select(s.tasks).where(
                    s.tasks.c.id == run["task_id"]).with_for_update()).mappings().one()
                if task["status"] == TaskStatus.READY.value:
                    self._append_event(connection, stream_id=task["id"], event_type="task.running",
                        payload={"run_id": run["id"], "claim_id": str(claim_id)},
                        idempotency_key=f"claim:{claim_id}:task-running")
                    connection.execute(sa.update(s.tasks).where(s.tasks.c.id == task["id"]).values(
                        status=TaskStatus.RUNNING.value, revision=task["revision"] + 1, updated_at=now))
            else:
                connection.execute(sa.update(s.runs).where(s.runs.c.id == run["id"]).values(
                    last_event_sequence=sequence, updated_at=now))
            return lease
    def claim_ready_agent_node(self, worker_id: str, claim_id: UUID) -> NodeLease | None:
        return self._claim_ready_typed_node(worker_id, claim_id, frozenset({NodeType.AGENT}))
    def claim_ready_control_node(self, worker_id: str, claim_id: UUID) -> NodeLease | None:
        return self._claim_ready_typed_node(worker_id, claim_id, CONTROL_NODE_TYPES)
    def claim_ready_verifier_node(self, worker_id: str, claim_id: UUID) -> NodeLease | None:
        return self._claim_ready_typed_node(worker_id, claim_id, frozenset({NodeType.VERIFIER}))
    def heartbeat_node_lease(self, claim_id: UUID, worker_id: str) -> NodeLease:
        with self._transaction() as connection:
            row = connection.execute(sa.select(s.node_leases).where(
                s.node_leases.c.claim_id == str(claim_id)).with_for_update()).mappings().first()
            if row is None:
                raise KeyError(claim_id)
            if row["worker_id"] != worker_id:
                raise ConcurrencyConflict("lease belongs to another worker")
            if row["released_at"] is not None:
                raise ConcurrencyConflict("lease is already released")
            connection.execute(sa.update(s.node_leases).where(s.node_leases.c.claim_id == str(claim_id))
                               .values(heartbeat_at=utc_now()))
            return decode(NodeLease, connection.execute(sa.select(s.node_leases).where(
                s.node_leases.c.claim_id == str(claim_id))).mappings().one())
    def _recover_typed_lease(self, claim_id: UUID, *, reason: str,
                             allowed_types: frozenset[NodeType], label: str) -> NodeRun:
        """Operator-confirmed recovery; never steals a live lease."""
        if not reason:
            raise ValueError("reason is required")
        with self._transaction() as connection:
            lease = connection.execute(sa.select(s.node_leases).where(
                s.node_leases.c.claim_id == str(claim_id)).with_for_update()).mappings().first()
            if lease is None:
                raise KeyError(claim_id)
            node = connection.execute(sa.select(s.node_runs).where(
                s.node_runs.c.id == lease["node_run_id"]).with_for_update()).mappings().one()
            graph = decode(GraphVersion, connection.execute(sa.select(s.graph_versions).join(
                s.runs, s.runs.c.graph_version_id == s.graph_versions.c.graph_version_id
            ).where(s.runs.c.id == lease["run_id"])).mappings().one())
            graph_node = next((item for item in graph.definition.nodes
                               if item.id == node["node_id"]), None)
            if graph_node is None or graph_node.type not in allowed_types:
                raise ConcurrencyConflict(f"only a {label} node lease can be recovered")
            if node["status"] == "ready":
                return decode(NodeRun, node)
            if lease["released_at"] is not None:
                raise ConcurrencyConflict("lease is already released")
            if node["status"] != "running":
                raise ConcurrencyConflict("only a running node lease can recover")
            now = utc_now()
            connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id == node["id"]).values(
                status="ready", error_code=reason, revision=node["revision"] + 1, updated_at=now))
            connection.execute(sa.update(s.node_leases).where(
                s.node_leases.c.claim_id == str(claim_id)).values(released_at=now))
            seq = self._append_event(connection, stream_id=UUID(lease["run_id"]),
                event_type="node.recovered",
                payload={"node_id": lease["node_id"], "claim_id": str(claim_id), "reason": reason},
                idempotency_key=f"claim:{claim_id}:recovered")
            connection.execute(sa.update(s.runs).where(s.runs.c.id == lease["run_id"]).values(
                revision=s.runs.c.revision + 1, last_event_sequence=seq, updated_at=now))
            return decode(NodeRun, connection.execute(sa.select(s.node_runs).where(
                s.node_runs.c.id == node["id"])).mappings().one())

    def recover_model_lease(self, claim_id: UUID, *, reason: str) -> NodeRun:
        return self._recover_typed_lease(
            claim_id, reason=reason, allowed_types=frozenset({NodeType.AGENT}), label="Agent",
        )
    def recover_node_lease(self, claim_id: UUID, *, reason: str) -> NodeRun:
        return self._recover_typed_lease(
            claim_id,
            reason=reason,
            allowed_types=frozenset({NodeType.AGENT, NodeType.VERIFIER,
                                     *RECOVERABLE_CONTROL_TYPES}),
            label="safe Agent/control/verifier",
        )