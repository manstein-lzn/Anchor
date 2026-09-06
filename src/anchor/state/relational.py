"""SQLAlchemy store for PostgreSQL and isolated SQLite contract-test databases."""

import hashlib
import json
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from uuid import UUID, uuid4

import sqlalchemy as sa

from anchor.domain.admission import RunDispatch, RunReceipt, RunRequest
from anchor.domain.drafts import GraphDraft
from anchor.domain.graph import CONTROL_NODE_TYPES, RECOVERABLE_CONTROL_TYPES, GraphDefinition, GraphVersion, GraphVersionStatus, NodeType, Trigger
from anchor.domain.context import input_hash as context_input_hash
from anchor.domain.models import (ContextSnapshot, EdgeDecision, NodeLease, NodeRun, Run, RunStatus,
                                  Task, TaskStatus, VerificationRecord, VerificationVerdict, utc_now)
from anchor.domain.operations import OperationStatus, ToolOperation
from anchor.runtime.propagation import decide_outgoing_edges, plan_propagation
from . import schema as s
from .errors import AdmissionConflict, ConcurrencyConflict, DuplicateEvent, GraphVersionConflict, OperationConflict


WAITING_NODE_TYPES = frozenset({"approval", "human_task", "wait_for_event"})


def wait_status_for(node_type: str) -> str | None:
    """Ready-state override for nodes that wait instead of executing."""
    if node_type in ("approval", "human_task"):
        return "waiting_approval"
    if node_type == "wait_for_event":
        return "waiting_event"
    return None


def values(model):
    result = model.model_dump(mode="python")
    return {key: str(value) if isinstance(value, UUID) else value for key, value in result.items()}


def decode(model, row):
    if row is None:
        return None
    data = dict(row)
    # SQLite drops timezone metadata; all persisted application timestamps are UTC.
    for key, value in data.items():
        if isinstance(value, datetime) and value.tzinfo is None:
            data[key] = value.replace(tzinfo=timezone.utc)
    return model.model_validate(data)


class RelationalStateStore:
    def __init__(self, url: str):
        self.engine = sa.create_engine(url, pool_pre_ping=True)
        if self.engine.dialect.name not in {"sqlite", "postgresql"}:
            self.engine.dispose()
            raise ValueError("only SQLite and PostgreSQL are supported")
        if self.engine.dialect.name == "sqlite":
            @sa.event.listens_for(self.engine, "connect")
            def configure(connection, record):
                connection.execute("PRAGMA foreign_keys=ON")
                connection.execute("PRAGMA busy_timeout=10000")

    def close(self):
        self.engine.dispose()

    @contextmanager
    def _transaction(self):
        with self.engine.connect() as connection:
            if self.engine.dialect.name == "sqlite":
                connection.exec_driver_sql("BEGIN IMMEDIATE")
            else:
                connection.begin()
            try:
                yield connection
                connection.commit()
            except BaseException:
                connection.rollback()
                raise

    def _lock(self, connection, key):
        if self.engine.dialect.name == "postgresql":
            token = int.from_bytes(hashlib.sha256(key.encode()).digest()[:8], "big", signed=True)
            connection.execute(sa.select(sa.func.pg_advisory_xact_lock(token)))

    def _read(self, table, model, identity):
        with self.engine.connect() as connection:
            row = connection.execute(sa.select(table).where(list(table.primary_key)[0] == str(identity))).mappings().first()
            return decode(model, row)

    def _insert(self, connection, table, model):
        connection.execute(sa.insert(table).values(**values(model)))
        return model

    def create_task(self, task: Task) -> Task:
        with self._transaction() as connection:
            return self._insert(connection, s.tasks, task)

    def get_task(self, task_id: UUID) -> Task | None:
        return self._read(s.tasks, Task, task_id)

    def publish_graph(self, version: GraphVersion) -> GraphVersion:
        version = GraphVersion.model_validate(version.model_dump(mode="json"))
        checked = GraphVersion.publish(version.definition, version.version)
        if version.graph_id != checked.graph_id or version.content_hash != checked.content_hash:
            raise GraphVersionConflict("definition does not match its identity or hash")
        with self._transaction() as connection:
            self._lock(connection, f"graph-publication:{version.graph_id}")
            self._lock(connection, f"graph:{version.graph_id}:{version.version}")
            row = connection.execute(sa.select(s.graph_versions).where(
                s.graph_versions.c.graph_id == version.graph_id, s.graph_versions.c.version == version.version,
            )).mappings().first()
            if row:
                if row["content_hash"] != version.content_hash:
                    raise GraphVersionConflict("published version cannot be replaced")
                return decode(GraphVersion, row)
            return self._insert(connection, s.graph_versions, version)

    def get_graph_version(self, graph_version_id: UUID) -> GraphVersion | None:
        return self._read(s.graph_versions, GraphVersion, graph_version_id)

    def get_draft(self, graph_id: str) -> GraphDraft | None:
        return self._read(s.graph_drafts, GraphDraft, graph_id)

    def save_draft(self, graph_id: str, *, expected_revision: int, definition: dict, layout: dict) -> GraphDraft:
        candidate = GraphDraft(graph_id=graph_id, revision=expected_revision + 1,
                               definition=definition, layout=layout, updated_at=utc_now())
        json.dumps(candidate.definition, allow_nan=False)
        json.dumps(candidate.layout, allow_nan=False)
        with self._transaction() as connection:
            self._lock(connection, f"draft:{graph_id}")
            current = connection.execute(sa.select(s.graph_drafts).where(s.graph_drafts.c.graph_id == graph_id)).mappings().first()
            if (current["revision"] if current else 0) != expected_revision:
                raise ConcurrencyConflict("draft changed; reload before saving")
            if current:
                connection.execute(sa.update(s.graph_drafts).where(s.graph_drafts.c.graph_id == graph_id).values(**values(candidate)))
            else:
                self._insert(connection, s.graph_drafts, candidate)
            return candidate

    def _expand_draft_subgraphs(self, connection, definition: GraphDefinition) -> GraphDefinition:
        """Materialize SUBGRAPH pins; only published versions are referenceable."""
        from anchor.domain.graph import expand_subgraphs

        def resolve(version_id: str) -> GraphDefinition:
            row = connection.execute(sa.select(s.graph_versions).where(
                s.graph_versions.c.graph_version_id == version_id)).mappings().first()
            if row is None:
                raise KeyError(version_id)
            child = decode(GraphVersion, row)
            if child.status is not GraphVersionStatus.PUBLISHED:
                raise ValueError(f"subgraph version is not published: {version_id}")
            return child.definition
        expanded, _ = expand_subgraphs(definition, resolve)
        return expanded

    def publish_draft(self, graph_id: str, expected_revision: int) -> GraphVersion:
        with self._transaction() as connection:
            self._lock(connection, f"draft:{graph_id}")
            prior = connection.execute(sa.select(s.graph_versions).join(s.draft_publications,
                s.draft_publications.c.graph_version_id == s.graph_versions.c.graph_version_id).where(
                s.draft_publications.c.graph_id == graph_id,
                s.draft_publications.c.draft_revision == expected_revision)).mappings().first()
            if prior:
                return decode(GraphVersion, prior)
            draft = connection.execute(sa.select(s.graph_drafts).where(s.graph_drafts.c.graph_id == graph_id)).mappings().first()
            if draft is None:
                raise KeyError(graph_id)
            if draft["revision"] != expected_revision:
                raise ConcurrencyConflict("draft changed; reload before publishing")
            definition = GraphDefinition.model_validate(draft["definition"])
            if definition.graph_id != graph_id:
                raise ValueError("definition graph_id must match the draft")
            if any(node.type is NodeType.SUBGRAPH for node in definition.nodes):
                definition = self._expand_draft_subgraphs(connection, definition)
            self._lock(connection, f"graph-publication:{graph_id}")
            number = connection.scalar(sa.select(sa.func.coalesce(sa.func.max(s.graph_versions.c.version), 0) + 1)
                                        .where(s.graph_versions.c.graph_id == graph_id))
            version = GraphVersion.publish(definition, number)
            self._insert(connection, s.graph_versions, version)
            connection.execute(sa.insert(s.draft_publications).values(graph_id=graph_id,
                draft_revision=expected_revision, graph_version_id=str(version.graph_version_id)))
            return version

    def list_drafts(self, limit: int = 50, offset: int = 0) -> list[GraphDraft]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.graph_drafts).order_by(s.graph_drafts.c.graph_id)
                                      .limit(limit).offset(offset)).mappings()
            return [decode(GraphDraft, row) for row in rows]

    def list_graph_versions(self, graph_id: str, limit: int = 50, offset: int = 0) -> list[GraphVersion]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.graph_versions).where(s.graph_versions.c.graph_id == graph_id)
                .order_by(s.graph_versions.c.version.desc()).limit(limit).offset(offset)).mappings()
            return [decode(GraphVersion, row) for row in rows]

    def get_trigger(self, trigger_id: UUID) -> Trigger | None:
        return self._read(s.triggers, Trigger, trigger_id)

    def register_trigger(self, trigger: Trigger) -> Trigger:
        with self._transaction() as connection:
            self._lock(connection, f"trigger-registration:{trigger.id}")
            prior = decode(Trigger, connection.execute(sa.select(s.triggers)
                .where(s.triggers.c.id == str(trigger.id))).mappings().first())
            if prior:
                if prior != trigger:
                    raise AdmissionConflict("trigger identity already has a different configuration")
                return prior
            graph = connection.execute(sa.select(s.graph_versions.c.status)
                .where(s.graph_versions.c.graph_version_id == str(trigger.graph_version_id))).scalar_one_or_none()
            if graph is None:
                raise KeyError(trigger.graph_version_id)
            if graph != GraphVersionStatus.PUBLISHED.value:
                raise ValueError("trigger requires a published version")
            return self._insert(connection, s.triggers, trigger)

    def set_trigger_enabled(self, trigger_id: UUID, enabled: bool) -> Trigger:
        with self._transaction() as connection:
            updated = connection.execute(sa.update(s.triggers).where(s.triggers.c.id == str(trigger_id)).values(enabled=enabled))
            if not updated.rowcount:
                raise KeyError(trigger_id)
            return decode(Trigger, connection.execute(sa.select(s.triggers).where(s.triggers.c.id == str(trigger_id))).mappings().one())

    def list_runs(self, limit: int = 50, offset: int = 0) -> list[Run]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.runs).order_by(s.runs.c.created_at.desc(), s.runs.c.id)
                                      .limit(limit).offset(offset)).mappings()
            return [decode(Run, row) for row in rows]

    def event_page(self, run_id: UUID, after: int = 0, limit: int = 100) -> list[dict]:
        with self.engine.connect() as connection:
            return [dict(row) for row in connection.execute(sa.select(s.events).where(
                s.events.c.stream_id == str(run_id), s.events.c.sequence > after)
                .order_by(s.events.c.sequence).limit(limit)).mappings()]

    def check_schema(self) -> None:
        with self.engine.connect() as connection:
            if connection.execute(sa.text("SELECT version_num FROM alembic_version")).scalar_one() != "0011_decision_attempts":
                raise RuntimeError("database schema is not at the supported revision")

    def create_trigger(self, trigger: Trigger) -> Trigger:
        with self._transaction() as connection:
            return self._insert(connection, s.triggers, trigger)

    def list_triggers(self, graph_version_id: UUID) -> list[Trigger]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.triggers).where(
                s.triggers.c.graph_version_id == str(graph_version_id)).order_by(s.triggers.c.id)).mappings()
            return [decode(Trigger, row) for row in rows]

    def list_active_triggers(self) -> list[Trigger]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.triggers).where(s.triggers.c.enabled.is_(True)).order_by(s.triggers.c.id)).mappings()
            return [decode(Trigger, row) for row in rows]

    def create_run(self, run: Run) -> Run:
        with self._transaction() as connection:
            return self._insert(connection, s.runs, run)

    def get_run(self, run_id: UUID) -> Run | None:
        return self._read(s.runs, Run, run_id)

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

    def _append_event(self, connection, *, stream_id, event_type, payload, idempotency_key):
        self._lock(connection, f"stream:{stream_id}")
        prior = connection.execute(sa.select(s.events).where(
            s.events.c.stream_id == str(stream_id), s.events.c.idempotency_key == idempotency_key,
        )).mappings().first()
        if prior:
            if prior["event_type"] != event_type or prior["payload"] != payload:
                raise DuplicateEvent(idempotency_key)
            return prior["sequence"]
        sequence = connection.execute(sa.select(sa.func.coalesce(sa.func.max(s.events.c.sequence), 0) + 1)
                                      .where(s.events.c.stream_id == str(stream_id))).scalar_one()
        connection.execute(sa.insert(s.events).values(stream_id=str(stream_id), sequence=sequence,
            event_type=event_type, payload=payload, idempotency_key=idempotency_key, created_at=utc_now()))
        return sequence

    def append_event(self, **kwargs) -> int:
        with self._transaction() as connection:
            return self._append_event(connection, **kwargs)

    def list_events(self, stream_id: UUID) -> list[dict]:
        with self.engine.connect() as connection:
            return [dict(row) for row in connection.execute(sa.select(s.events).where(
                s.events.c.stream_id == str(stream_id)).order_by(s.events.c.sequence)).mappings()]

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

    def accept_dispatch(self, message: RunDispatch) -> Run:
        """Atomically accept one outbox message into the execution lifecycle.

        This is deliberately an admission boundary, not an Agent execution call.
        The idempotency key is the durable message identity, so a redelivery after
        a lost acknowledgement returns the already-running Run without another
        revision or event.
        """
        payload = {
            "message_id": str(message.message_id),
            "task_id": str(message.task_id),
            "graph_version_id": str(message.graph_version_id),
        }
        key = f"dispatch:{message.message_id}"
        with self._transaction() as connection:
            self._lock(connection, f"dispatch:{message.message_id}")
            row = connection.execute(sa.select(s.runs).where(s.runs.c.id == str(message.run_id))).mappings().first()
            if row is None:
                raise KeyError(message.run_id)
            if row["task_id"] != str(message.task_id) or row["graph_version_id"] != str(message.graph_version_id):
                raise ValueError("dispatch does not match the pinned run")
            prior = connection.execute(sa.select(s.execution_inbox).where(
                s.execution_inbox.c.message_id == str(message.message_id))).mappings().first()
            if prior:
                if prior["run_id"] != str(message.run_id) or prior["envelope"] != message.model_dump(mode="json"):
                    raise DuplicateEvent(key)
                return decode(Run, row)
            if row["status"] != RunStatus.CREATED.value:
                raise ConcurrencyConflict("run is no longer awaiting dispatch")
            connection.execute(sa.insert(s.execution_inbox).values(message_id=str(message.message_id),
                run_id=str(message.run_id), envelope=message.model_dump(mode="json"), accepted_at=utc_now()))
            sequence = self._append_event(connection, stream_id=message.run_id,
                                          event_type="run.dispatch_accepted", payload=payload, idempotency_key=key)
            graph = decode(GraphVersion, connection.execute(sa.select(s.graph_versions).where(
                s.graph_versions.c.graph_version_id == str(message.graph_version_id))).mappings().one())
            entry_node_id = graph.definition.resolved_entry_node_id()
            node = connection.execute(sa.select(s.node_runs).where(
                s.node_runs.c.run_id == str(message.run_id), s.node_runs.c.node_id == entry_node_id,
                s.node_runs.c.attempt == 0)).mappings().one()
            entry_type = next((item.type.value for item in graph.definition.nodes
                               if item.id == entry_node_id), "")
            entry_wait = wait_status_for(entry_type)
            entry_event = "node.waiting" if entry_wait else "node.ready"
            entry_status = entry_wait or "ready"
            entry_key_suffix = "waiting" if entry_wait else "ready"
            sequence = self._append_event(connection, stream_id=message.run_id,
                event_type=entry_event, payload={"node_id": entry_node_id, "attempt": 0},
                idempotency_key=f"{key}:node:{entry_node_id}:{entry_key_suffix}")
            connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id == node["id"]).values(
                status=entry_status, revision=node["revision"] + 1, updated_at=utc_now()))
            connection.execute(sa.update(s.runs).where(s.runs.c.id == str(message.run_id)).values(
                status=RunStatus.QUEUED.value, current_phase="queued",
                revision=row["revision"] + 1, last_event_sequence=sequence, updated_at=utc_now()))
            task_row = connection.execute(sa.select(s.tasks).where(s.tasks.c.id == str(message.task_id))).mappings().first()
            if task_row is None:
                raise KeyError(message.task_id)
            if task_row["status"] == TaskStatus.CREATED.value:
                task_key = f"dispatch:{message.message_id}"
                self._append_event(connection, stream_id=message.task_id, event_type="task.running",
                                    payload={"run_id": str(message.run_id), "message_id": str(message.message_id)},
                                    idempotency_key=task_key)
                connection.execute(sa.update(s.tasks).where(s.tasks.c.id == str(message.task_id)).values(
                    status=TaskStatus.READY.value, revision=task_row["revision"] + 1, updated_at=utc_now()))
            return decode(Run, connection.execute(sa.select(s.runs).where(s.runs.c.id == str(message.run_id))).mappings().one())

    def accepted_dispatches(self, limit: int = 100) -> list[RunDispatch]:
        if limit <= 0:
            raise ValueError("batch size must be positive")
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.execution_inbox.c.envelope)
                .order_by(s.execution_inbox.c.accepted_at, s.execution_inbox.c.message_id).limit(limit)).scalars()
            return [RunDispatch.model_validate(row) for row in rows]

    def record_runtime_heartbeat(self, component: str, instance_id: UUID) -> None:
        now = utc_now()
        with self._transaction() as connection:
            existing = connection.execute(sa.select(s.runtime_heartbeats.c.component).where(
                s.runtime_heartbeats.c.component == component)).scalar_one_or_none()
            values_ = {"instance_id": str(instance_id), "observed_at": now}
            if existing:
                connection.execute(sa.update(s.runtime_heartbeats).where(
                    s.runtime_heartbeats.c.component == component).values(**values_))
            else:
                connection.execute(sa.insert(s.runtime_heartbeats).values(component=component, **values_))

    def runtime_connected(self, component: str, *, within_seconds: int = 15) -> bool:
        if within_seconds <= 0:
            raise ValueError("heartbeat freshness must be positive")
        with self.engine.connect() as connection:
            observed = connection.execute(sa.select(s.runtime_heartbeats.c.observed_at).where(
                s.runtime_heartbeats.c.component == component)).scalar_one_or_none()
        if observed is None:
            return False
        if observed.tzinfo is None:
            observed = observed.replace(tzinfo=timezone.utc)
        return observed >= utc_now() - timedelta(seconds=within_seconds)

    def list_active_leases(self) -> list[NodeLease]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.node_leases).where(
                s.node_leases.c.released_at.is_(None)).order_by(s.node_leases.c.heartbeat_at)).mappings().all()
            return [decode(NodeLease, row) for row in rows]

    def claim_ready_node(self, worker_id: str, claim_id: UUID) -> NodeLease | None:
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
            query = sa.select(s.node_runs).where(s.node_runs.c.status == "ready").order_by(
                s.node_runs.c.created_at, s.node_runs.c.id).limit(1)
            if self.engine.dialect.name == "postgresql":
                query = query.with_for_update(skip_locked=True)
            node = connection.execute(query).mappings().first()
            if node is None:
                return None
            connection.execute(sa.delete(s.node_leases).where(s.node_leases.c.node_run_id == node["id"], s.node_leases.c.released_at.is_not(None)))
            run = connection.execute(sa.select(s.runs).where(
                s.runs.c.id == node["run_id"]).with_for_update()).mappings().one()
            if run["status"] not in {RunStatus.QUEUED.value, RunStatus.RUNNING.value}:
                raise ConcurrencyConflict("ready node belongs to a run that cannot execute")
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
                task = connection.execute(sa.select(s.tasks).where(s.tasks.c.id == run["task_id"])
                                          .with_for_update()).mappings().one()
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
                s.node_runs.c.status == "ready").order_by(
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
            connection.execute(sa.delete(s.node_leases).where(
                s.node_leases.c.node_run_id == node["id"], s.node_leases.c.released_at.is_not(None)))
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
        if not reason: raise ValueError("reason is required")
        with self._transaction() as connection:
            lease=connection.execute(sa.select(s.node_leases).where(s.node_leases.c.claim_id==str(claim_id)).with_for_update()).mappings().first()
            if lease is None: raise KeyError(claim_id)
            node=connection.execute(sa.select(s.node_runs).where(s.node_runs.c.id==lease["node_run_id"]).with_for_update()).mappings().one()
            graph=decode(GraphVersion, connection.execute(sa.select(s.graph_versions).join(s.runs, s.runs.c.graph_version_id==s.graph_versions.c.graph_version_id).where(s.runs.c.id==lease["run_id"])).mappings().one())
            graph_node=next((item for item in graph.definition.nodes if item.id == node["node_id"]), None)
            if graph_node is None or graph_node.type not in allowed_types:
                raise ConcurrencyConflict(f"only a {label} node lease can be recovered")
            if node["status"] == "ready": return decode(NodeRun,node)
            if node["status"] != "running": raise ConcurrencyConflict("only a running node lease can recover")
            now=utc_now()
            connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id==node["id"]).values(status="ready",error_code=reason,revision=node["revision"]+1,updated_at=now))
            connection.execute(sa.update(s.node_leases).where(s.node_leases.c.claim_id==str(claim_id)).values(released_at=now))
            seq=self._append_event(connection,stream_id=UUID(lease["run_id"]),event_type="node.recovered",payload={"node_id":lease["node_id"],"claim_id":str(claim_id),"reason":reason},idempotency_key=f"claim:{claim_id}:recovered")
            connection.execute(sa.update(s.runs).where(s.runs.c.id==lease["run_id"]).values(revision=s.runs.c.revision+1,last_event_sequence=seq,updated_at=now))
            return decode(NodeRun,connection.execute(sa.select(s.node_runs).where(s.node_runs.c.id==node["id"])).mappings().one())

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

    def checkpoint_node_result(self, claim_id: UUID, worker_id: str, *, output_ref: str) -> NodeRun:
        if not output_ref: raise ValueError("output_ref is required")
        with self._transaction() as connection:
            lease = connection.execute(sa.select(s.node_leases).where(s.node_leases.c.claim_id == str(claim_id)).with_for_update()).mappings().first()
            if lease is None: raise KeyError(claim_id)
            if lease["worker_id"] != worker_id or lease["released_at"] is not None: raise ConcurrencyConflict("lease is not owned by worker")
            node = connection.execute(sa.select(s.node_runs).where(s.node_runs.c.id == lease["node_run_id"]).with_for_update()).mappings().one()
            if node["status"] == "completed" and node["output_ref"] == output_ref: return decode(NodeRun, node)
            if node["status"] != "running": raise ConcurrencyConflict("node is not running")
            graph = decode(GraphVersion, connection.execute(sa.select(s.graph_versions).join(
                s.runs, s.runs.c.graph_version_id == s.graph_versions.c.graph_version_id
            ).where(s.runs.c.id == lease["run_id"])).mappings().one())
            graph_node = next(
                (item for item in graph.definition.nodes if item.id == lease["node_id"]), None,
            )
            if graph_node is not None and graph_node.type is NodeType.VERIFIER:
                raise ConcurrencyConflict("verifier completion requires the verification gate")
            now = utc_now()
            sequence = self._append_event(connection, stream_id=UUID(lease["run_id"]), event_type="node.completed",
                payload={"node_id": lease["node_id"], "claim_id": str(claim_id), "output_ref": output_ref}, idempotency_key=f"claim:{claim_id}:completed")
            connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id == lease["node_run_id"]).values(status="completed", output_ref=output_ref, revision=node["revision"]+1, updated_at=now))
            connection.execute(sa.update(s.node_leases).where(s.node_leases.c.claim_id == str(claim_id)).values(released_at=now))
            connection.execute(sa.update(s.runs).where(s.runs.c.id == lease["run_id"]).values(last_event_sequence=sequence, revision=s.runs.c.revision+1, updated_at=now))
            return decode(NodeRun, connection.execute(sa.select(s.node_runs).where(s.node_runs.c.id == lease["node_run_id"])).mappings().one())

    def _propagate_completion(self, connection, *, run, graph: GraphVersion, run_id: UUID,
                                now, seq: int, outgoing_decisions=(),
                                open_downstream: bool = True,
                                input_hashes: dict[str, str] | None = None,
                                generation: int | None = None) -> tuple[int, list[NodeRun]]:
        """Shared post-completion tail: decisions, plan, ready/skipped, terminal.

        Used by the lease-bound worker path and the human/event decision paths
        alike, so downstream semantics cannot diverge between executors. `seq`
        is the latest stream sequence on entry and the updated one on exit."""
        input_hashes = input_hashes or {}
        created: list[NodeRun] = []
        if open_downstream:
            for decision in outgoing_decisions:
                seq = self._persist_edge_decision(connection, decision)
            rows = connection.execute(sa.select(s.node_runs).where(
                s.node_runs.c.run_id == str(run_id))).mappings().all()
            existing: dict[str, object] = {}
            for row in rows:
                prior = existing.get(row["node_id"])
                if prior is None or row["attempt"] > prior["attempt"]:
                    existing[row["node_id"]] = row
            decision_rows = connection.execute(sa.select(s.edge_decisions).where(
                s.edge_decisions.c.run_id == str(run_id)).order_by(
                s.edge_decisions.c.edge_index)).mappings().all()
            plan = plan_propagation(
                graph,
                [decode(NodeRun, row) for row in rows],
                [decode(EdgeDecision, row) for row in decision_rows],
            )
            for node_id in plan.skipped_node_ids:
                target = existing[node_id]
                connection.execute(sa.update(s.node_runs).where(
                    s.node_runs.c.id == target["id"]).values(
                    status="skipped", revision=target["revision"] + 1, updated_at=now))
                seq = self._append_event(
                    connection,
                    stream_id=run_id,
                    event_type="node.skipped",
                    payload={"node_id": node_id, "reason": "no_selected_incoming_edge"},
                    idempotency_key=f"node:{target['id']}:skipped",
                )
            for decision in plan.inferred_decisions:
                seq = self._persist_edge_decision(connection, decision)
            node_types = {item.id: item.type.value for item in graph.definition.nodes}
            for node_id in plan.ready_node_ids:
                target = existing[node_id]
                waiting = wait_status_for(node_types.get(node_id, ""))
                target_status = waiting or "ready"
                target_event = "node.waiting" if waiting else "node.ready"
                key_suffix = "waiting" if waiting else "ready"
                connection.execute(sa.update(s.node_runs).where(
                    s.node_runs.c.id == target["id"]).values(
                    status=target_status, input_hash=input_hashes.get(node_id),
                    revision=target["revision"] + 1, updated_at=now))
                seq = self._append_event(
                    connection,
                    stream_id=run_id,
                    event_type=target_event,
                    payload={"node_id": node_id, "input_hash": input_hashes.get(node_id)},
                    idempotency_key=f"node:{target['id']}:{key_suffix}",
                )
                created.append(decode(NodeRun, connection.execute(sa.select(s.node_runs).where(
                    s.node_runs.c.id == target["id"])).mappings().one()))
            # Cycle re-entry: a selected edge whose target already completed
            # (only possible on validator-approved loop boundaries) starts a
            # new attempt instead of resurrecting the finished row. The re-armed
            # node is marked ready only when its inputs are decided, selected
            # and freshly completed; otherwise it stays pending and the next
            # plan evaluation fails closed loudly.
            decided_by_index = {}
            for row in connection.execute(sa.select(s.edge_decisions).where(
                    s.edge_decisions.c.run_id == str(run_id))).mappings():
                prior = decided_by_index.get(row["edge_index"])
                if prior is None or row["source_attempt"] >= prior["source_attempt"]:
                    decided_by_index[row["edge_index"]] = row
            rearmed: set[str] = set()
            # Only evaluations made in this call can re-arm: stale selections
            # were already consumed by the finished attempt.
            for decision in outgoing_decisions:
                if not decision.selected:
                    continue
                edge = graph.definition.edges[decision.edge_index]
                target = existing.get(edge.target)
                if target is None or target["status"] != "completed":
                    continue
                if edge.target in rearmed:
                    continue
                rearmed.add(edge.target)
                incoming = [i for i, item in enumerate(graph.definition.edges)
                            if item.target == edge.target]
                if any(i not in decided_by_index for i in incoming):
                    continue
                selected = [decided_by_index[i] for i in incoming
                            if decided_by_index[i]["selected"]]
                if not selected:
                    continue
                sources_done = all(
                    existing.get(graph.definition.edges[i].source, {}).get("status")
                    == "completed" for i in incoming if decided_by_index[i]["selected"])
                attempt = max(row["attempt"] for row in connection.execute(
                    sa.select(s.node_runs).where(s.node_runs.c.run_id == str(run_id),
                        s.node_runs.c.node_id == edge.target)).mappings()) + 1
                fresh = self._insert(connection, s.node_runs,
                    NodeRun(run_id=run_id, node_id=edge.target, attempt=attempt,
                            status="ready" if sources_done else "pending"))
                if sources_done:
                    seq = self._append_event(connection, stream_id=run_id,
                        event_type="node.ready",
                        payload={"node_id": edge.target, "attempt": attempt},
                        idempotency_key=f"node:{fresh.id}:ready")
                    created.append(fresh)
                else:
                    seq = self._append_event(connection, stream_id=run_id,
                        event_type="node.rearmed",
                        payload={"node_id": edge.target, "attempt": attempt},
                        idempotency_key=f"node:{fresh.id}:rearmed")
        statuses = [r["status"] for r in connection.execute(
            sa.select(s.node_runs.c.status).where(s.node_runs.c.run_id == str(run_id))).mappings()]
        if open_downstream and statuses and all(
                status in ("completed", "skipped") for status in statuses):
            seq = self._append_event(connection, stream_id=run_id, event_type="run.completed",
                payload={"phase": "complete"}, idempotency_key=f"run:{run_id}:completed")
            connection.execute(sa.update(s.runs).where(s.runs.c.id == str(run_id)).values(
                status="completed", current_phase="complete", revision=run["revision"] + 1,
                last_event_sequence=seq,
                context_generation=generation if generation is not None else run["context_generation"],
                updated_at=now))
            task = connection.execute(sa.select(s.tasks).where(
                s.tasks.c.id == run["task_id"]).with_for_update()).mappings().one()
            if task["status"] not in (TaskStatus.COMPLETED.value, TaskStatus.CANCELLED.value):
                self._append_event(
                    connection, stream_id=UUID(run["task_id"]), event_type="task.completed",
                    payload={"run_id": str(run_id)}, idempotency_key=f"run:{run_id}:task-completed")
                connection.execute(sa.update(s.tasks).where(s.tasks.c.id == run["task_id"]).values(
                    status=TaskStatus.COMPLETED.value, revision=task["revision"] + 1, updated_at=now))
        else:
            connection.execute(sa.update(s.runs).where(s.runs.c.id == str(run_id)).values(
                revision=run["revision"] + 1, last_event_sequence=seq,
                context_generation=generation if generation is not None else run["context_generation"],
                updated_at=now))
        return seq, created

    def complete_node_and_propagate(self, claim_id: UUID, worker_id: str, *, output_ref: str,
                                    input_hashes: dict[str, str] | None = None,
                                    node_input_hash: str | None = None,
                                    input_snapshot: dict[str, object] | None = None,
                                    condition_context: dict[str, object] | None = None,
                                    verification: VerificationRecord | None = None) -> list[NodeRun]:
        if not output_ref: raise ValueError("output_ref is required")
        input_hashes = input_hashes or {}
        with self._transaction() as connection:
            lease = connection.execute(sa.select(s.node_leases).where(s.node_leases.c.claim_id == str(claim_id)).with_for_update()).mappings().first()
            if lease is None: raise KeyError(claim_id)
            if lease["worker_id"] != worker_id or lease["released_at"] is not None: raise ConcurrencyConflict("lease is not owned by worker")
            node = connection.execute(sa.select(s.node_runs).where(s.node_runs.c.id == lease["node_run_id"]).with_for_update()).mappings().one()
            if node["status"] != "running": raise ConcurrencyConflict("node is not running")
            now = utc_now(); run_id = UUID(lease["run_id"])
            run = connection.execute(sa.select(s.runs).where(s.runs.c.id == str(run_id)).with_for_update()).mappings().one()
            graph = decode(GraphVersion, connection.execute(sa.select(s.graph_versions).where(
                s.graph_versions.c.graph_version_id == run["graph_version_id"])).mappings().one())
            graph_node = next(
                (item for item in graph.definition.nodes if item.id == lease["node_id"]), None,
            )
            if graph_node is None:
                raise KeyError(lease["node_id"])
            active_run = run["status"] not in (
                RunStatus.FAILED.value, RunStatus.CANCELLED.value, RunStatus.COMPLETED.value,
            )
            outgoing_decisions = decide_outgoing_edges(
                graph,
                run_id=run_id,
                source_node_id=lease["node_id"],
                evaluation_context=condition_context,
                evidence_ref=output_ref,
                source_attempt=int(node["attempt"]),
            ) if active_run else ()
            generation = None
            if input_snapshot is not None:
                computed_hash = context_input_hash(input_snapshot)
                if node_input_hash is not None and node_input_hash != computed_hash:
                    raise ValueError("node_input_hash does not match input_snapshot")
                node_input_hash = computed_hash
                generation = int(run["context_generation"]) + 1
                snapshot = ContextSnapshot(
                    run_id=run_id, node_run_id=UUID(lease["node_run_id"]), generation=generation,
                    input_hash=computed_hash, snapshot=input_snapshot,
                )
                self._insert(connection, s.context_snapshots, snapshot)
            self._validate_verification(
                verification,
                lease=lease,
                graph_node=graph_node,
                expected_verdicts=frozenset({VerificationVerdict.PASSED}),
                context_hash=node_input_hash,
            )
            if verification is not None:
                if verification.evidence_ref != output_ref:
                    raise ConcurrencyConflict("passed verifier output must be its evidence artifact")
                self._persist_verification(connection, verification)
            seq = self._append_event(connection, stream_id=run_id, event_type="node.completed", payload={
                "node_id": lease["node_id"], "claim_id": str(claim_id), "output_ref": output_ref,
                "input_hash": node_input_hash, "context_generation": generation,
            }, idempotency_key=f"claim:{claim_id}:completed")
            connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id == lease["node_run_id"]).values(status="completed", output_ref=output_ref, input_hash=node_input_hash, revision=node["revision"]+1, updated_at=now))
            connection.execute(sa.update(s.node_leases).where(s.node_leases.c.claim_id == str(claim_id)).values(released_at=now))
            # A failed/cancelled Run may still have workers finishing already
            # claimed nodes. Record those completions, but never open new
            # downstream work or revive the terminal Run.
            seq, created = self._propagate_completion(
                connection, run=run, graph=graph, run_id=run_id, now=now, seq=seq,
                outgoing_decisions=outgoing_decisions if active_run else (),
                open_downstream=active_run, input_hashes=input_hashes, generation=generation)
            if generation is not None:
                connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id == lease["node_run_id"]).values(context_generation=generation))
            return created

    def fail_node_and_propagate(self, claim_id: UUID, worker_id: str, *, error_code: str,
                                phase: str = "execute",
                                verification: VerificationRecord | None = None,
                                input_snapshot: dict[str, object] | None = None) -> NodeRun:
        """Atomically fail a claimed node and its Run, without opening edges.

        Failure is deliberately explicit: callers decide that the model/tool
        outcome is known to be failed. Unknown external side effects must use
        the operation ledger and reconciliation path instead.
        """
        if not error_code or len(error_code) > 200:
            raise ValueError("error_code is required and must be at most 200 characters")
        with self._transaction() as connection:
            lease = connection.execute(sa.select(s.node_leases).where(s.node_leases.c.claim_id == str(claim_id))).mappings().first()
            if lease is None:
                raise KeyError(claim_id)
            if lease["worker_id"] != worker_id:
                raise ConcurrencyConflict("lease is not owned by worker")
            node = connection.execute(sa.select(s.node_runs).where(s.node_runs.c.id == lease["node_run_id"])).mappings().first()
            if node is None:
                raise KeyError(lease["node_run_id"])
            if node["status"] == "failed" and lease["released_at"] is not None:
                if node["error_code"] != error_code:
                    raise ConcurrencyConflict("failed node already has a different error")
                return decode(NodeRun, node)
            if lease["released_at"] is not None or node["status"] != "running":
                raise ConcurrencyConflict("node is not running")
            run = connection.execute(sa.select(s.runs).where(s.runs.c.id == lease["run_id"])).mappings().one()
            if run["status"] in ("cancelled", "completed"):
                raise ConcurrencyConflict("run is already terminal")
            now = utc_now()
            graph = decode(GraphVersion, connection.execute(sa.select(s.graph_versions).where(
                s.graph_versions.c.graph_version_id == run["graph_version_id"])).mappings().one())
            graph_node = next(
                (item for item in graph.definition.nodes if item.id == lease["node_id"]), None,
            )
            if graph_node is None:
                raise KeyError(lease["node_id"])
            context_hash = context_input_hash(input_snapshot) if input_snapshot is not None else None
            if verification is not None:
                self._validate_verification(
                    verification,
                    lease=lease,
                    graph_node=graph_node,
                    expected_verdicts=frozenset(VerificationVerdict),
                    context_hash=context_hash,
                )
                generation = int(run["context_generation"]) + 1
                snapshot = ContextSnapshot(
                    run_id=UUID(lease["run_id"]), node_run_id=UUID(lease["node_run_id"]),
                    generation=generation, input_hash=context_hash, snapshot=input_snapshot,
                )
                self._insert(connection, s.context_snapshots, snapshot)
                self._persist_verification(connection, verification)
            else:
                generation = None
            self._append_event(
                connection, stream_id=UUID(lease["run_id"]), event_type="node.failed",
                payload={"node_id": lease["node_id"], "claim_id": str(claim_id), "error_code": error_code, "phase": phase},
                idempotency_key=f"claim:{claim_id}:failed")
            connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id == lease["node_run_id"]).values(
                status="failed", error_code=error_code,
                input_hash=context_hash if context_hash is not None else node["input_hash"],
                context_generation=generation if generation is not None else node["context_generation"],
                revision=node["revision"] + 1, updated_at=now))
            connection.execute(sa.update(s.node_leases).where(s.node_leases.c.claim_id == str(claim_id)).values(released_at=now))
            if run["status"] not in ("failed", "cancelled", "completed"):
                run_sequence = self._append_event(
                    connection, stream_id=UUID(lease["run_id"]), event_type="run.failed",
                    payload={"node_id": lease["node_id"], "claim_id": str(claim_id), "error_code": error_code, "phase": phase},
                    idempotency_key=f"run:{lease['run_id']}:failed")
                connection.execute(sa.update(s.runs).where(s.runs.c.id == lease["run_id"]).values(
                    status="failed", current_phase=phase, revision=run["revision"] + 1,
                    last_event_sequence=run_sequence,
                    context_generation=generation if generation is not None else run["context_generation"],
                    updated_at=now))
                task = connection.execute(sa.select(s.tasks).where(
                    s.tasks.c.id == run["task_id"]).with_for_update()).mappings().one()
                if task["status"] not in (TaskStatus.FAILED.value, TaskStatus.CANCELLED.value, TaskStatus.COMPLETED.value):
                    self._append_event(
                        connection, stream_id=UUID(run["task_id"]), event_type="task.failed",
                        payload={"run_id": lease["run_id"], "node_id": lease["node_id"], "error_code": error_code},
                        idempotency_key=f"run:{lease['run_id']}:task-failed")
                    connection.execute(sa.update(s.tasks).where(s.tasks.c.id == run["task_id"]).values(
                        status=TaskStatus.FAILED.value, revision=task["revision"] + 1, updated_at=now))
            return decode(NodeRun, connection.execute(sa.select(s.node_runs).where(s.node_runs.c.id == lease["node_run_id"])).mappings().one())

    def list_waiting_nodes(self, run_id: UUID | None = None, limit: int = 100) -> list[NodeRun]:
        """List nodes parked in human/event wait states, oldest first."""
        with self.engine.connect() as connection:
            query = sa.select(s.node_runs).where(
                s.node_runs.c.status.in_(["waiting_approval", "waiting_event"])
            ).order_by(s.node_runs.c.created_at, s.node_runs.c.id).limit(limit)
            if run_id is not None:
                query = query.where(s.node_runs.c.run_id == str(run_id))
            return [decode(NodeRun, row) for row in connection.execute(query).mappings()]

    def decide_approval(self, node_run_id: UUID, *, approved: bool, reason: str,
                        actor: str, output_ref: str,
                        condition_context: dict[str, object] | None = None) -> NodeRun:
        """Atomically record a human approval decision and advance the run.

        Approval completes the node and opens downstream edges; rejection
        fails the node, run and task without opening any edge. Idempotent
        per node: repeating the same decision returns the stored outcome,
        while a conflicting decision fails closed.
        """
        if not reason or len(reason) > 2000:
            raise ValueError("approval reason is required and must be at most 2000 characters")
        if not actor or len(actor) > 200:
            raise ValueError("approval actor is required and must be at most 200 characters")
        if approved and not output_ref:
            raise ValueError("approval completion requires output_ref")
        with self._transaction() as connection:
            node = connection.execute(sa.select(s.node_runs).where(
                s.node_runs.c.id == str(node_run_id)).with_for_update()).mappings().first()
            if node is None:
                raise KeyError(node_run_id)
            run_id = UUID(node["run_id"])
            run = connection.execute(sa.select(s.runs).where(
                s.runs.c.id == str(run_id)).with_for_update()).mappings().one()
            graph = decode(GraphVersion, connection.execute(sa.select(s.graph_versions).where(
                s.graph_versions.c.graph_version_id == run["graph_version_id"])).mappings().one())
            graph_node = next(
                (item for item in graph.definition.nodes if item.id == node["node_id"]), None)
            if graph_node is None:
                raise KeyError(node["node_id"])
            if graph_node.type.value not in ("approval", "human_task"):
                raise ValueError("node is not an approval gate")
            if run["status"] in ("failed", "cancelled", "completed"):
                raise ConcurrencyConflict("run is already terminal")
            now = utc_now()
            if approved:
                if node["status"] == "completed":
                    if node["output_ref"] != output_ref:
                        raise ConcurrencyConflict("approval already completed with different output")
                    return decode(NodeRun, node)
                if node["status"] != "waiting_approval":
                    raise ConcurrencyConflict("node is not awaiting approval")
                seq = self._append_event(connection, stream_id=run_id, event_type="node.approved",
                    payload={"node_id": node["node_id"], "actor": actor, "reason": reason},
                    idempotency_key=f"node:{node_run_id}:approved")
                connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id == str(node_run_id)).values(
                    status="completed", output_ref=output_ref,
                    revision=node["revision"] + 1, updated_at=now))
                outgoing = decide_outgoing_edges(
                    graph, run_id=run_id, source_node_id=node["node_id"],
                    evaluation_context=condition_context, evidence_ref=output_ref,
                    source_attempt=int(node["attempt"]))
                _, _ = self._propagate_completion(
                    connection, run=run, graph=graph, run_id=run_id, now=now, seq=seq,
                    outgoing_decisions=outgoing, open_downstream=True)
            else:
                if node["status"] == "failed" and node["error_code"] == "approval_rejected":
                    return decode(NodeRun, node)
                if node["status"] != "waiting_approval":
                    raise ConcurrencyConflict("node is not awaiting approval")
                seq = self._append_event(connection, stream_id=run_id, event_type="node.rejected",
                    payload={"node_id": node["node_id"], "actor": actor, "reason": reason},
                    idempotency_key=f"node:{node_run_id}:rejected")
                connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id == str(node_run_id)).values(
                    status="failed", error_code="approval_rejected",
                    revision=node["revision"] + 1, updated_at=now))
                seq = self._append_event(connection, stream_id=run_id, event_type="run.failed",
                    payload={"node_id": node["node_id"], "error_code": "approval_rejected"},
                    idempotency_key=f"run:{run_id}:failed")
                connection.execute(sa.update(s.runs).where(s.runs.c.id == str(run_id)).values(
                    status="failed", current_phase="approval_rejected",
                    revision=run["revision"] + 1, last_event_sequence=seq, updated_at=now))
                task = connection.execute(sa.select(s.tasks).where(
                    s.tasks.c.id == run["task_id"]).with_for_update()).mappings().one()
                if task["status"] not in (TaskStatus.FAILED.value, TaskStatus.CANCELLED.value,
                                            TaskStatus.COMPLETED.value):
                    self._append_event(connection, stream_id=UUID(run["task_id"]),
                        event_type="task.failed",
                        payload={"run_id": str(run_id), "node_id": node["node_id"],
                                 "error_code": "approval_rejected"},
                        idempotency_key=f"run:{run_id}:task-failed")
                    connection.execute(sa.update(s.tasks).where(s.tasks.c.id == run["task_id"]).values(
                        status=TaskStatus.FAILED.value, revision=task["revision"] + 1, updated_at=now))
            return decode(NodeRun, connection.execute(sa.select(s.node_runs).where(
                s.node_runs.c.id == str(node_run_id))).mappings().one())

    def resume_event(self, node_run_id: UUID, *, event_type: str, payload: dict,
                     output_ref: str, actor: str = "event",
                     condition_context: dict[str, object] | None = None) -> NodeRun:
        """Atomically resume a wait_for_event node and advance the run.

        When the node declares `wait_event` metadata, the ingress event type
        must match; otherwise any event resumes. The payload should already
        be persisted as an artifact by the caller; its reference completes
        the node and opens downstream edges.
        """
        if not event_type or len(event_type) > 200:
            raise ValueError("event type is required and must be at most 200 characters")
        if not output_ref:
            raise ValueError("event resume requires output_ref")
        with self._transaction() as connection:
            node = connection.execute(sa.select(s.node_runs).where(
                s.node_runs.c.id == str(node_run_id)).with_for_update()).mappings().first()
            if node is None:
                raise KeyError(node_run_id)
            if node["status"] == "completed":
                if node["output_ref"] != output_ref:
                    raise ConcurrencyConflict("event node already resumed with different output")
                return decode(NodeRun, node)
            if node["status"] != "waiting_event":
                raise ConcurrencyConflict("node is not waiting for an event")
            run_id = UUID(node["run_id"])
            run = connection.execute(sa.select(s.runs).where(
                s.runs.c.id == str(run_id)).with_for_update()).mappings().one()
            if run["status"] in ("failed", "cancelled", "completed"):
                raise ConcurrencyConflict("run is already terminal")
            graph = decode(GraphVersion, connection.execute(sa.select(s.graph_versions).where(
                s.graph_versions.c.graph_version_id == run["graph_version_id"])).mappings().one())
            graph_node = next(
                (item for item in graph.definition.nodes if item.id == node["node_id"]), None)
            if graph_node is None:
                raise KeyError(node["node_id"])
            if graph_node.type.value != "wait_for_event":
                raise ValueError("node is not an event wait")
            expected = (graph_node.metadata or {}).get("wait_event")
            if expected and expected != event_type:
                raise ValueError(f"event type {event_type!r} does not match wait {expected!r}")
            now = utc_now()
            seq = self._append_event(connection, stream_id=run_id, event_type="node.resumed",
                payload={"node_id": node["node_id"], "event_type": event_type,
                         "actor": actor, "payload": payload},
                idempotency_key=f"node:{node_run_id}:resumed:{event_type}")
            connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id == str(node_run_id)).values(
                status="completed", output_ref=output_ref,
                revision=node["revision"] + 1, updated_at=now))
            outgoing = decide_outgoing_edges(
                graph, run_id=run_id, source_node_id=node["node_id"],
                evaluation_context=condition_context, evidence_ref=output_ref,
                source_attempt=int(node["attempt"]))
            _, _ = self._propagate_completion(
                connection, run=run, graph=graph, run_id=run_id, now=now, seq=seq,
                outgoing_decisions=outgoing, open_downstream=True)
            return decode(NodeRun, connection.execute(sa.select(s.node_runs).where(
                s.node_runs.c.id == str(node_run_id))).mappings().one())

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

    def list_tool_operations(self, run_id: UUID) -> list[ToolOperation]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(s.tool_operations).where(
                s.tool_operations.c.run_id == str(run_id)).order_by(
                s.tool_operations.c.created_at, s.tool_operations.c.operation_id)).mappings()
            return [decode(ToolOperation, row) for row in rows]

    def _advance_run_event_pointer(self, connection, run_id: UUID, sequence: int) -> None:
        row = connection.execute(sa.select(s.runs.c.revision).where(
            s.runs.c.id == str(run_id)).with_for_update()).mappings().one()
        connection.execute(sa.update(s.runs).where(s.runs.c.id == str(run_id)).values(
            revision=row["revision"] + 1, last_event_sequence=sequence, updated_at=utc_now()))

    def admit_run(self, request: RunRequest) -> RunReceipt:
        request = RunRequest.model_validate(request.model_dump(mode="json"))
        canonical = json.dumps(request.model_dump(mode="json"), sort_keys=True, separators=(",", ":"), allow_nan=False)
        with self._transaction() as connection:
            self._lock(connection, f"admit:{request.trigger_id}:{request.idempotency_key}")
            previous = connection.execute(sa.select(s.run_admissions).where(
                s.run_admissions.c.trigger_id == str(request.trigger_id),
                s.run_admissions.c.idempotency_key == request.idempotency_key,
            )).mappings().first()
            if previous:
                if previous["request_json"] != canonical:
                    raise AdmissionConflict("occurrence key reused with different request content")
                return RunReceipt(**{field: previous[field] for field in RunReceipt.model_fields})
            trigger = connection.execute(sa.select(s.triggers).where(s.triggers.c.id == str(request.trigger_id))
                                         .with_for_update()).mappings().first()
            if trigger is None:
                raise KeyError(request.trigger_id)
            if not trigger["enabled"]:
                raise ValueError("trigger is disabled")
            graph = decode(GraphVersion, connection.execute(sa.select(s.graph_versions).where(
                s.graph_versions.c.graph_version_id == trigger["graph_version_id"])).mappings().first())
            if graph is None or graph.status is not GraphVersionStatus.PUBLISHED:
                raise ValueError("trigger must reference a published graph version")
            task = self._insert(connection, s.tasks, Task(objective=request.objective,
                constraints=request.constraints, success_criteria=request.success_criteria))
            run = self._insert(connection, s.runs, Run(task_id=task.id, graph_version_id=graph.graph_version_id,
                                                      last_event_sequence=1))
            for node in graph.definition.nodes:
                self._insert(connection, s.node_runs, NodeRun(run_id=run.id, node_id=node.id))
            self._append_event(connection, stream_id=task.id, event_type="task.created",
                               payload=task.model_dump(mode="json"), idempotency_key="admission")
            self._append_event(connection, stream_id=run.id, event_type="run.requested",
                payload={"task_id": str(task.id), "graph_version_id": str(graph.graph_version_id),
                         "trigger_id": str(request.trigger_id)}, idempotency_key="admission")
            message = RunDispatch(message_id=uuid4(), run_id=run.id, task_id=task.id,
                graph_version_id=graph.graph_version_id, inputs=request.inputs, created_at=utc_now())
            self._enqueue_dispatch(connection, message)
            receipt = RunReceipt(task_id=task.id, run_id=run.id, graph_version_id=graph.graph_version_id,
                                 message_id=message.message_id)
            connection.execute(sa.insert(s.run_admissions).values(trigger_id=str(request.trigger_id),
                idempotency_key=request.idempotency_key, request_json=canonical, **values(receipt)))
            return receipt

    def _enqueue_dispatch(self, connection, message):
        connection.execute(sa.insert(s.run_outbox).values(message_id=str(message.message_id),
            run_id=str(message.run_id), envelope=message.model_dump(mode="json")))

    def pending_dispatches(self, limit: int = 100) -> list[RunDispatch]:
        if limit <= 0:
            raise ValueError("batch size must be positive")
        with self.engine.connect() as connection:
            envelopes = connection.execute(sa.select(s.run_outbox.c.envelope).where(
                s.run_outbox.c.acknowledged_at.is_(None)).order_by(s.run_outbox.c.sequence).limit(limit)).scalars()
            return [RunDispatch.model_validate(envelope) for envelope in envelopes]

    def acknowledge_dispatch(self, message_id: UUID) -> None:
        with self._transaction() as connection:
            updated = connection.execute(sa.update(s.run_outbox).where(s.run_outbox.c.message_id == str(message_id))
                .values(acknowledged_at=sa.func.coalesce(s.run_outbox.c.acknowledged_at, utc_now())))
            if not updated.rowcount:
                raise KeyError(message_id)
