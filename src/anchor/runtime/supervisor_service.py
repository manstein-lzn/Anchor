"""Standalone conservative lease supervisor service."""
from __future__ import annotations

import asyncio
import json
import logging
from dataclasses import asdict
from anchor.runtime.supervisor import assess_leases
from anchor.runtime.settings import AnchorSettings
from anchor.state.relational import RelationalStateStore

log = logging.getLogger("anchor.supervisor")


async def run_supervisor(store, *, interval: float = 10.0, stale_after: float = 30.0,
                         stop: asyncio.Event | None = None) -> None:
    if interval <= 0 or stale_after <= 0:
        raise ValueError("interval and stale_after must be positive")
    stop = stop or asyncio.Event()
    reported: dict[str, tuple[str, str]] = {}
    while not stop.is_set():
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
                    payload = asdict(assessment)
                    payload["lease"] = assessment.lease.model_dump(mode="json")
                    log.warning("lease assessment %s", json.dumps(payload, default=str, ensure_ascii=False))
                    reported[key] = fingerprint
        active = {str(item.lease.claim_id) for item in assessments}
        for key in list(reported):
            if key not in active:
                del reported[key]
        try:
            await asyncio.wait_for(stop.wait(), timeout=interval)
        except asyncio.TimeoutError:
            pass


def main() -> None:
    settings = AnchorSettings()
    logging.basicConfig(level=settings.log_level)
    url = settings.database_url or "sqlite:///./.local/api.sqlite"
    asyncio.run(run_supervisor(RelationalStateStore(url), interval=settings.supervisor_interval,
                               stale_after=settings.lease_stale_after))


if __name__ == "__main__":
    main()
