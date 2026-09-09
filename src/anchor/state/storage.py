"""Storage accounting primitives for the retention budget.

The budget is deliberately simple: one global on-disk knob and one per-graph
knob. Everything here is read-only; eviction and garbage collection live in a
separate, explicitly audited path. Per-graph occupancy is measured on artifacts
(the dominant and most variable component) plus run count, while the global
knob measures the real on-disk total: database file + artifact directory.
"""

from __future__ import annotations

import json
import os
import re
from collections import defaultdict
from uuid import UUID

import sqlalchemy as sa

from .base import _StoreHost, utc_now
from . import schema as s

ARTIFACT_PATTERN = re.compile(r"artifact://sha256/[0-9a-f]{64}")


class StorageStoreMixin(_StoreHost):
    """Read-only footprint queries shared by the report and eviction planner."""

    def run_graph_index(self) -> dict[UUID, str]:
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(
                s.runs.c.id, s.graph_versions.c.graph_id).join(
                s.graph_versions,
                s.runs.c.graph_version_id == s.graph_versions.c.graph_version_id)).all()
        return {UUID(run_id): graph_id for run_id, graph_id in rows}

    def run_lifecycle_index(self) -> dict[UUID, dict]:
        """status + archived_at + created_at for every run, oldest first."""
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(
                s.runs.c.id, s.runs.c.status, s.runs.c.archived_at,
                s.runs.c.created_at).order_by(s.runs.c.created_at, s.runs.c.id)).all()
        return {UUID(run_id): {"status": status, "archived_at": archived_at,
                               "created_at": created_at}
                for run_id, status, archived_at, created_at in rows}

    def list_artifact_references(self) -> list[tuple[UUID, str]]:
        """Every artifact reference, including ones embedded in snapshot JSON."""
        refs: list[tuple[UUID, str]] = []
        with self.engine.connect() as connection:
            columns = (
                (s.node_runs, s.node_runs.c.output_ref),
                (s.tool_operations, s.tool_operations.c.result_ref),
                (s.edge_decisions, s.edge_decisions.c.evidence_ref),
                (s.verification_records, s.verification_records.c.evidence_ref),
            )
            for table, column in columns:
                rows = connection.execute(sa.select(
                    table.c.run_id, column).where(column.is_not(None))).all()
                refs.extend((UUID(run_id), ref) for run_id, ref in rows
                            if isinstance(ref, str) and ref.startswith("artifact://sha256/"))
            snapshots = connection.execute(sa.select(
                s.context_snapshots.c.run_id, s.context_snapshots.c.snapshot)).all()
            event_rows = connection.execute(sa.select(
                s.events.c.stream_id, s.events.c.payload)).all()
        for run_id, snapshot in snapshots:
            text = snapshot if isinstance(snapshot, str) else json.dumps(snapshot, sort_keys=True)
            refs.extend((UUID(run_id), ref) for ref in set(ARTIFACT_PATTERN.findall(text)))
        # Event payloads can reference artifacts that no column points at (for
        # example a node's model text when its output is a workspace revision).
        for stream_id, payload in event_rows:
            text = payload if isinstance(payload, str) else json.dumps(payload, sort_keys=True)
            for ref in set(ARTIFACT_PATTERN.findall(text)):
                try:
                    refs.append((UUID(stream_id), ref))
                except ValueError:
                    continue
        return refs

    def database_bytes(self) -> int | None:
        backend = self.engine.url.get_backend_name()
        if backend == "sqlite":
            path = self.engine.url.database
            if not path or path == ":memory:" or not os.path.exists(path):
                return None
            return os.path.getsize(path)
        with self.engine.connect() as connection:
            return int(connection.execute(
                sa.text("SELECT pg_database_size(current_database())")).scalar_one())


    GLOBAL_SCOPE = "__global__"

    def get_storage_budgets(self) -> dict[str, int | None]:
        """scope -> byte budget. ``None`` means no budget configured."""
        with self.engine.connect() as connection:
            rows = connection.execute(sa.select(
                s.storage_budgets.c.scope, s.storage_budgets.c.bytes)).all()
        return {scope: size for scope, size in rows}

    def set_storage_budget(self, scope: str, bytes: int | None) -> None:
        """Upsert a budget; ``None`` clears it. Takes effect on the next read."""
        if bytes is not None and bytes < 0:
            raise ValueError("storage budget must not be negative")
        with self._transaction() as connection:
            connection.execute(sa.delete(s.storage_budgets).where(
                s.storage_budgets.c.scope == scope))
            if bytes is not None:
                connection.execute(sa.insert(s.storage_budgets).values(
                    scope=scope, bytes=bytes, updated_at=utc_now()))


def artifact_sizes(root: str | os.PathLike) -> dict[str, int]:
    """digest reference -> byte size for every blob currently on disk."""
    sizes: dict[str, int] = {}
    directory = os.path.expanduser(str(root))
    if not os.path.isdir(directory):
        return sizes
    for name in os.listdir(directory):
        path = os.path.join(directory, name)
        if os.path.isfile(path):
            sizes[f"artifact://sha256/{name}"] = os.path.getsize(path)
    return sizes


def storage_report(store, *, artifact_root: str | os.PathLike,
                   global_budget: int | None = None,
                   graph_budgets: dict[str, int | None] | None = None) -> dict:
    """Current footprint, per graph and in total. Read-only."""
    sizes = artifact_sizes(artifact_root)
    references = store.list_artifact_references()
    graph_of = store.run_graph_index()
    lifecycle = store.run_lifecycle_index()

    graph_artifacts: dict[str, set[str]] = defaultdict(set)
    for run_id, ref in references:
        graph_id = graph_of.get(run_id)
        if graph_id is not None:
            graph_artifacts[graph_id].add(ref)

    # A blob referenced by more than one graph cannot be attributed exclusively,
    # so it is reported separately and never counted against a single budget.
    owners: dict[str, set[str]] = defaultdict(set)
    for graph_id, refs in graph_artifacts.items():
        for ref in refs:
            owners[ref].add(graph_id)

    runs_per_graph: dict[str, int] = defaultdict(int)
    for graph_id in graph_of.values():
        runs_per_graph[graph_id] += 1

    budgets = graph_budgets or {}
    graphs = []
    for graph_id in sorted(set(graph_artifacts) | set(runs_per_graph)):
        refs = graph_artifacts.get(graph_id, set())
        exclusive = sum(sizes.get(ref, 0) for ref in refs if len(owners[ref]) == 1)
        shared = sum(sizes.get(ref, 0) for ref in refs if len(owners[ref]) > 1)
        budget = budgets.get(graph_id)
        graphs.append({
            "graph_id": graph_id,
            "runs": runs_per_graph.get(graph_id, 0),
            "artifact_files": len(refs),
            "artifact_bytes": exclusive + shared,
            "exclusive_bytes": exclusive,
            "shared_bytes": shared,
            "budget_bytes": budget,
            # A per-graph target charges shared bytes too; the global knob is
            # what actually bounds the whole install.
            "over_budget": bool(budget is not None and exclusive + shared > budget),
        })

    database_bytes = store.database_bytes()
    artifact_bytes = sum(sizes.values())
    total = (database_bytes or 0) + artifact_bytes
    return {
        "database_bytes": database_bytes,
        "artifacts_bytes": artifact_bytes,
        "artifacts_files": len(sizes),
        "total_bytes": total,
        "budget": {"global_bytes": global_budget},
        "over_global_budget": bool(global_budget is not None and total > global_budget),
        "graphs": graphs,
        "runs_total": len(lifecycle),
        "runs_terminal": sum(1 for item in lifecycle.values()
                             if item["status"] in ("completed", "failed", "cancelled")),
    }
