"""Standalone conservative lease supervisor service with phase-1 progress observation."""

from __future__ import annotations

import asyncio
import json
import logging
from typing import Any
from uuid import UUID

from anchor.domain.models import ProgressEvidence
from anchor.runtime.preflight import require_environment
from anchor.runtime.supervisor import assess_leases
from anchor.runtime.settings import AnchorSettings
from anchor.runtime.watchdog import AdaptiveWatchdog, cycle_fingerprint
from anchor.state.relational import RelationalStateStore


log = logging.getLogger("anchor.supervisor")


def _safe_model_dump(model) -> dict[str, Any]:
    try:
        return model.model_dump(mode="json")
    except Exception:  # noqa: BLE001 - a maintenance loop must survive any cycle
        return {}


def _observation_for_run(store: RelationalStateStore, run_id: UUID) -> ProgressEvidence | None:
    run = store.get_run(run_id)
    if run is None:
        return None
    nodes = store.list_node_runs(run_id)
    verifications = store.list_verifications(run_id)
    operations = store.list_tool_operations(run_id)
    waiting_nodes = [node for node in nodes if node.status in {"waiting_approval", "waiting_event"}]
    waiting_for = waiting_nodes[0].status if waiting_nodes else None
    verified_progress_refs = tuple({record.evidence_ref for record in verifications if record.verdict.value == "passed"})
    tool_operation_ids = tuple({str(item.operation_id) for item in operations})
    completed_nodes = [node for node in nodes if node.status == "completed"]
    latest_completed = max(completed_nodes, key=lambda node: node.updated_at) if completed_nodes else None
    cycle_fingerprint_value = None
    if latest_completed is not None:
        output_refs = tuple(sorted({latest_completed.output_ref} if latest_completed.output_ref else set()))
        cycle_fingerprint_value = cycle_fingerprint(
            phase=run.current_phase,
            input_hash=latest_completed.input_hash,
            output_refs=output_refs,
            tool_operation_ids=tool_operation_ids,
        )
    return ProgressEvidence(
        run_id=run_id,
        node_run_id=latest_completed.id if latest_completed else None,
        state_revision=run.revision,
        phase=run.current_phase,
        artifact_refs=tuple(sorted({node.output_ref for node in nodes if node.output_ref})),
        verifier_passes=len([record for record in verifications if record.verdict.value == "passed"]),
        tool_operation_ids=tool_operation_ids,
        heartbeat_at=run.updated_at,
        waiting_for=waiting_for,
        worker_expected=run.status.value not in {"completed", "failed", "cancelled"},
        verified_progress_refs=verified_progress_refs,
        cycle_iteration=len(completed_nodes),
        cycle_fingerprint=cycle_fingerprint_value,
    )


async def run_supervisor(store, *, interval: float = 10.0, stale_after: float = 30.0,
                         stop: asyncio.Event | None = None) -> None:
    if interval <= 0 or stale_after <= 0:
        raise ValueError("interval and stale_after must be positive")
    stop = stop or asyncio.Event()
    settings = AnchorSettings()
    from anchor.runtime.content_commit import ContentCommitter
    from anchor.runtime.workspaces import WorkspaceManager
    committer = ContentCommitter(store, WorkspaceManager(store, root=settings.workspace_root))
    reported: dict[str, tuple[str, str]] = {}
    while not stop.is_set():
        expire = getattr(store, "expire_run_budgets", None)
        if expire is not None and settings.expire_run_budgets:
            expire()
        leases = store.list_active_leases()
        node_types: dict[str, str] = {}
        for lease in leases:
            run = store.get_run(lease.run_id)
            graph = store.get_graph_version(run.graph_version_id) if run else None
            if graph:
                node = next((n for n in graph.definition.nodes if n.id == lease.node_id), None)
                if node:
                    node_types[str(lease.claim_id)] = node.type.value
        assessments = assess_leases(leases, stale_after=stale_after, node_types=node_types)
        for assessment in assessments:
            if assessment.state != "healthy":
                key = str(assessment.lease.claim_id)
                fingerprint = (assessment.state, assessment.reason)
                if reported.get(key) != fingerprint:
                    payload = _safe_model_dump(assessment.lease)
                    payload["assessment"] = _safe_model_dump(assessment)
                    log.warning("lease assessment %s", json.dumps(payload, default=str, ensure_ascii=False))
                    reported[key] = fingerprint
        active = {str(item.lease.claim_id) for item in assessments}
        for key in list(reported):
            if key not in active:
                del reported[key]

        # Content-plane reconciliation: clear markers whose node is terminal,
        # report revisions whose bytes vanished. Never guess content.
        def _is_terminal(prepared) -> bool:
            node = store.get_node_run(prepared.node_run_id)
            return node is not None and node.status.value in {
                "completed", "failed", "cancelled", "skipped"}

        try:
            for outcome in committer.sweep_orphans(is_terminal=_is_terminal):
                if outcome.action == "inconsistent":
                    log.error("prepared revision %s@%s for node %s is unavailable",
                              outcome.prepared.workspace_id, outcome.prepared.revision,
                              outcome.prepared.node_run_id)
        except (RuntimeError, AttributeError):
            log.exception("prepared-revision reconciliation failed")

        # Observation + persistent diagnostics via adaptive watchdog.
        active_leases = [item for item in assessments if item.state != "healthy"]
        for assessment in active_leases:
            run_id = assessment.lease.run_id
            current = _observation_for_run(store, run_id)
            if current is None:
                continue
            try:
                watchdog = AdaptiveWatchdog(store)
                watchdog.assess(run_id=run_id, current=current)
            except (RuntimeError, AttributeError):
                log.exception("watchdog observation failed for run %s", run_id)

        try:
            await asyncio.wait_for(stop.wait(), timeout=interval)
        except asyncio.TimeoutError:
            pass


def main() -> None:
    settings = AnchorSettings()
    logging.basicConfig(level=settings.log_level)
    # This used to fall back to a developer's local SQLite file when the URL was unset, so
    # a misconfigured unit watched an empty database and reported nothing wrong. An
    # environment a supervisor cannot read is exactly the one it must refuse to observe.
    require_environment(role="supervisor", database_url=settings.database_url)
    asyncio.run(run_supervisor(RelationalStateStore(settings.require_database_url()),
                               interval=settings.supervisor_interval,
                               stale_after=settings.lease_stale_after))


if __name__ == "__main__":
    main()
