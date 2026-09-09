"""Commit a node outcome and propagate it through the pinned graph."""

from __future__ import annotations

from datetime import datetime
from uuid import UUID

import sqlalchemy as sa

from anchor.domain.context import input_hash as context_input_hash
from anchor.domain.graph import GraphVersion
from anchor.domain.models import (ContextSnapshot, EdgeDecision, NodeRun, RunStatus,
                                  TaskStatus, VerificationRecord, VerificationVerdict,
                                  utc_now)
from anchor.domain.propagation import decide_outgoing_edges, plan_propagation
from . import schema as s
from .base import _StoreHost, decode, wait_status_for
from .errors import ConcurrencyConflict


class CheckpointStoreMixin(_StoreHost):
    """The shared completion/failure/retry tail and human wait decisions."""
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
                                    verification: VerificationRecord | None = None,
                                    event_payload: dict[str, object] | None = None) -> list[NodeRun]:
        if not output_ref: raise ValueError("output_ref is required")
        input_hashes = input_hashes or {}
        with self._transaction() as connection:
            lease, node = self._locked_running_node(connection, claim_id, worker_id)
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
            payload: dict[str, object] = {
                "node_id": lease["node_id"], "claim_id": str(claim_id), "output_ref": output_ref,
                "input_hash": node_input_hash, "context_generation": generation,
            }
            if event_payload:
                payload.update(event_payload)
            seq = self._append_event(connection, stream_id=run_id, event_type="node.completed",
                                     payload=payload,
                                     idempotency_key=f"claim:{claim_id}:completed")
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
            if input_snapshot is not None:
                generation = int(run["context_generation"]) + 1
                snapshot = ContextSnapshot(
                    run_id=UUID(lease["run_id"]), node_run_id=UUID(lease["node_run_id"]),
                    generation=generation, input_hash=context_hash, snapshot=input_snapshot,
                )
                self._insert(connection, s.context_snapshots, snapshot)
            else:
                generation = None
            if verification is not None:
                self._persist_verification(connection, verification)
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
    def retry_node_and_propagate(self, claim_id: UUID, worker_id: str, *, error_code: str,
                                 phase: str = "agent",
                                 error_class: str | None = None,
                                 next_attempt_at: datetime | None = None,
                                 input_snapshot: dict[str, object] | None = None) -> NodeRun:
        """Release a transiently failed Agent lease and queue a new attempt.

        The prior attempt remains immutable evidence of the failure. The Run
        stays active, while the fresh attempt is claimed by the normal worker
        loop. No downstream edges are opened until an attempt completes.

        `error_class` and `next_attempt_at` persist the recovery plan so a
        worker restart does not lose the backoff schedule; the fresh attempt is
        not claimable before `next_attempt_at`.
        """
        if not error_code or len(error_code) > 200:
            raise ValueError("error_code is required and must be at most 200 characters")
        if error_class is not None and len(error_class) > 64:
            raise ValueError("error_class must be at most 64 characters")
        with self._transaction() as connection:
            lease, node = self._locked_running_node(connection, claim_id, worker_id)
            run = connection.execute(sa.select(s.runs).where(
                s.runs.c.id == lease["run_id"]).with_for_update()).mappings().one()
            if run["status"] in (RunStatus.FAILED.value, RunStatus.CANCELLED.value, RunStatus.COMPLETED.value):
                raise ConcurrencyConflict("run is already terminal")
            now = utc_now()
            # Keep the failed attempt and its lease history, then expose the
            # next attempt to the ordinary typed claim path.
            connection.execute(sa.update(s.node_runs).where(s.node_runs.c.id == node["id"]).values(
                status="failed", error_code=error_code, last_error_class=error_class,
                revision=node["revision"] + 1, updated_at=now))
            connection.execute(sa.update(s.node_leases).where(
                s.node_leases.c.claim_id == str(claim_id)).values(released_at=now))
            next_attempt = int(node["attempt"]) + 1
            fresh = NodeRun(run_id=UUID(lease["run_id"]), node_id=lease["node_id"],
                            attempt=next_attempt, status="ready",
                            input_hash=node["input_hash"], context_generation=node["context_generation"],
                            next_attempt_at=next_attempt_at)
            self._insert(connection, s.node_runs, fresh)
            seq = self._append_event(
                connection, stream_id=UUID(lease["run_id"]), event_type="node.retrying",
                payload={"node_id": lease["node_id"], "from_attempt": int(node["attempt"]),
                         "attempt": next_attempt, "claim_id": str(claim_id),
                         "error_code": error_code, "phase": phase,
                         "error_class": error_class,
                         "next_attempt_at": next_attempt_at.isoformat() if next_attempt_at else None},
                idempotency_key=f"claim:{claim_id}:retry:{next_attempt}")
            connection.execute(sa.update(s.runs).where(s.runs.c.id == lease["run_id"]).values(
                status=RunStatus.RUNNING.value, current_phase="node.retrying",
                revision=run["revision"] + 1, last_event_sequence=seq, updated_at=now))
            return fresh
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
