"""Token spend reporting: the gross counter is not the bill.

An agent tool loop re-sends its whole conversation on every model call, so a
node's gross prompt count grows quadratically while the provider serves most of
it from a prefix cache. This route separates the frightening counter from the
amount actually charged, per node and per run.
"""

from uuid import UUID

from fastapi import FastAPI


def register_usage_routes(app: FastAPI, *, auth, db, required) -> None:
    """Register the spend report; ``db`` is the store dependency type."""

    def register_usage(*, app: FastAPI = app, auth=auth, db=db, required=required) -> None:

        @app.get("/api/runs/{run_id}/usage", dependencies=auth)
        def run_usage(run_id: UUID, store: db):
            """Token spend for the run, per node.

            An agent tool loop re-sends its conversation on every call, so a
            node attempt can cost millions of gross prompt tokens while most of
            them are cached. `billed_input_tokens` is input minus the cached
            share: that is the number that decides affordability.
            """
            required(store.get_run(run_id))
            by_node: dict[str, dict] = {}
            total: dict = {"input_tokens": 0, "cached_tokens": 0, "billed_input_tokens": 0,
                           "output_tokens": 0, "requests": 0, "cost": 0.0, "calls": 0}
            for event in store.list_events(run_id):
                if event.get("event_type") != "model.usage":
                    continue
                payload = event.get("payload") or {}
                node = str(payload.get("node_id") or "?")
                entry = by_node.setdefault(node, {"node_id": node, "input_tokens": 0,
                                                  "cached_tokens": 0, "billed_input_tokens": 0,
                                                  "output_tokens": 0, "requests": 0,
                                                  "cost": 0.0, "calls": 0})
                for key in ("input_tokens", "output_tokens", "requests"):
                    value = int(payload.get(key) or 0)
                    entry[key] += value
                    total[key] += value
                cached = int(payload.get("cache_read_tokens") or 0)
                entry["cached_tokens"] += cached
                total["cached_tokens"] += cached
                if payload.get("cost") is not None:
                    entry["cost"] += float(payload["cost"])
                    total["cost"] += float(payload["cost"])
                entry["calls"] += 1
                total["calls"] += 1
            for bucket in (total, *by_node.values()):
                bucket["billed_input_tokens"] = max(
                    bucket["input_tokens"] - bucket["cached_tokens"], 0)
            return {"run_id": str(run_id), "total": total,
                    "by_node": sorted(by_node.values(), key=lambda item: -item["input_tokens"])}

    register_usage()
