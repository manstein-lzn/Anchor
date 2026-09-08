"""Graph authoring, immutable versions, triggers and durable admission."""

from __future__ import annotations

import json
from uuid import UUID, uuid4

import sqlalchemy as sa

from anchor.domain.admission import RunDispatch, RunReceipt, RunRequest
from anchor.domain.drafts import GraphDraft
from anchor.domain.graph import (GraphDefinition, GraphVersion, GraphVersionStatus,
                                 NodeType, Trigger)
from anchor.domain.models import NodeRun, Run, RunStatus, Task, TaskStatus, utc_now
from . import schema as s
from .base import decode, values, wait_status_for
from .errors import (AdmissionConflict, ConcurrencyConflict, DuplicateEvent,
                     GraphVersionConflict)


class GraphStoreMixin:
    """Drafts, publications, triggers and admission/outbox/inbox."""
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
    def list_runs(self, limit: int = 50, offset: int = 0, *, graph_id: str | None = None) -> list[Run]:
        with self.engine.connect() as connection:
            query = sa.select(s.runs)
            if graph_id is not None:
                query = query.join(s.graph_versions, s.runs.c.graph_version_id == s.graph_versions.c.graph_version_id).where(
                    s.graph_versions.c.graph_id == graph_id)
            rows = connection.execute(query.order_by(s.runs.c.created_at.desc(), s.runs.c.id)
                                      .limit(limit).offset(offset)).mappings()
            return [decode(Run, row) for row in rows]
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
                if row["status"] in ("cancelled", "failed"):
                    connection.execute(sa.insert(s.execution_inbox).values(message_id=str(message.message_id),
                        run_id=str(message.run_id), envelope=message.model_dump(mode="json"), accepted_at=utc_now()))
                    return decode(Run, row)
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
