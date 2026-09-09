"""Rolling retention: keep the install under its storage budgets.

A budget sweep only ever evicts finished history. It never terminates a running
node, never touches a run that is in flight or waiting for an operator, and
records every action in ``retention_audit``. Artifacts are content addressed and
shared, so reclaiming disk is a mark-and-sweep pass, not a per-run file delete.
"""

from __future__ import annotations

import logging

from anchor.state.retention import TERMINAL_STATUSES
from anchor.state.storage import artifact_sizes, storage_report

logger = logging.getLogger("anchor.retention")


def collect_garbage(store, artifacts) -> dict:
    """Delete blobs no surviving row references. Returns counts and freed bytes."""
    referenced = {ref for _, ref in store.list_artifact_references()}
    removed = 0
    freed = 0
    for ref, size in artifact_sizes(artifacts.root).items():
        if ref in referenced:
            continue
        if artifacts.delete(ref):
            removed += 1
            freed += size
    return {"removed": removed, "freed_bytes": freed}


def plan_storage_budgets(store, artifacts) -> dict:
    """Dry run: what a sweep would consider, with no deletion at all."""
    from collections import defaultdict

    budgets = store.get_storage_budgets()
    global_budget = budgets.get(store.GLOBAL_SCOPE)
    graph_budgets = {scope: size for scope, size in budgets.items()
                     if scope != store.GLOBAL_SCOPE}
    report = _report(store, artifacts, global_budget, graph_budgets)
    sizes = artifact_sizes(artifacts.root)
    owners: dict[str, set] = defaultdict(set)
    run_refs: dict = defaultdict(set)
    for run_id, ref in store.list_artifact_references():
        run_refs[run_id].add(ref)
        owners[ref].add(run_id)
    graph_of = store.run_graph_index()
    lifecycle = store.run_lifecycle_index()
    protected = store.protected_run_ids()
    candidates = []
    for run_id, info in lifecycle.items():  # oldest first
        if info["status"] not in TERMINAL_STATUSES or run_id in protected:
            continue
        exclusive = sum(sizes.get(ref, 0) for ref in run_refs.get(run_id, ())
                        if len(owners[ref]) == 1)
        candidates.append({"run_id": str(run_id), "graph_id": graph_of.get(run_id),
                           "created_at": info["created_at"].isoformat(),
                           "exclusive_bytes": exclusive})
    return {
        "needed": report["over_global_budget"] or any(
            item["over_budget"] for item in report["graphs"]),
        "global_budget": global_budget,
        "total_bytes": report["total_bytes"],
        "over_global_budget": report["over_global_budget"],
        "graphs": [item for item in report["graphs"] if item["over_budget"]],
        "candidates": candidates,
        "protected_runs": len(protected),
    }


def _report(store, artifacts, global_budget, graph_budgets):
    return storage_report(store, artifact_root=artifacts.root,
                          global_budget=global_budget, graph_budgets=graph_budgets)


def enforce_storage_budgets(store, artifacts, *, trigger: str = "scheduler",
                            batch: int = 25, max_rounds: int = 40) -> dict:
    """Evict oldest finished runs until the install is under its budgets.

    Returns a summary; nothing is deleted when no budget is configured.
    """
    budgets = store.get_storage_budgets()
    global_budget = budgets.get(store.GLOBAL_SCOPE)
    graph_budgets = {scope: size for scope, size in budgets.items()
                     if scope != store.GLOBAL_SCOPE}
    if global_budget is None and not graph_budgets:
        return {"evicted": 0, "freed_bytes": 0, "reason": "no_budget"}

    before = _report(store, artifacts, global_budget, graph_budgets)
    evicted: list[str] = []
    reclaimed = 0
    for _ in range(max_rounds):
        report = _report(store, artifacts, global_budget, graph_budgets)
        over_graphs = [item for item in report["graphs"] if item["over_budget"]]
        if not over_graphs and not report["over_global_budget"]:
            break
        protected = store.protected_run_ids()
        graph_of = store.run_graph_index()
        lifecycle = store.run_lifecycle_index()  # oldest first
        done = set(evicted)
        candidates = [str(run_id) for run_id, info in lifecycle.items()
                      if info["status"] in TERMINAL_STATUSES
                      and str(run_id) not in protected and str(run_id) not in done]
        if not candidates:
            break
        chosen: list[str] = []
        for item in sorted(over_graphs,
                           key=lambda g: g["artifact_bytes"] - (g["budget_bytes"] or 0),
                           reverse=True):
            chosen = [rid for rid in candidates
                      if graph_of.get(rid) == item["graph_id"]][:batch]
            if chosen:
                break
        if not chosen:
            chosen = candidates[:batch]
        for run_id in chosen:
            store.purge_run(run_id)
            evicted.append(run_id)
        reclaimed += collect_garbage(store, artifacts)["freed_bytes"]

    database_before = store.database_bytes()
    if evicted:
        store.vacuum()
        reclaimed += collect_garbage(store, artifacts)["freed_bytes"]
    database_after = store.database_bytes()

    after = _report(store, artifacts, global_budget, graph_budgets)
    # Artifact bytes are exact on every backend. The database file only shrinks
    # on SQLite; PostgreSQL reclaims space internally without shrinking the file.
    database_shrink = max(0, (database_before or 0) - (database_after or 0))
    freed = reclaimed + database_shrink
    summary = {"evicted": len(evicted), "freed_bytes": freed,
               "total_bytes": after["total_bytes"],
               "over_global_budget": after["over_global_budget"]}
    if evicted:
        store.record_retention_audit(
            trigger=trigger, evicted_runs=len(evicted), freed_bytes=freed,
            detail={"run_ids": evicted[:200], "global_budget": global_budget,
                    "graph_budgets": graph_budgets, "total_after": after["total_bytes"]})
        logger.info("retention evicted %d runs, freed %d bytes", len(evicted), freed)
    return summary
