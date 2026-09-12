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
            """Token spend for the run, per node, and what the prompt was made of.

            An agent tool loop re-sends its conversation on every call, so a node attempt can
            cost millions of gross prompt tokens while most of them are cached.
            `billed_input_tokens` is input minus the cached share: that is the number that
            decides affordability, and the counter that looks alarming is not it.

            Two questions follow, and the payload answers both:

            - *What is stable?* A stable prefix is served from the provider's cache and is
              nearly free; anything that moves invalidates it and is billed at the full rate,
              which on our provider is fifty times the cached price. The segment hashes say
              which of the three parts changed, because token counts cannot.
            - *What is new, and what was re-sent?* Each call re-sends every earlier prompt, so
              the gross total is `resent + new`, decomposed here. Reducing the working set
              helps the new part once and the re-sent part on every turn.
            """
            required(store.get_run(run_id))
            by_node: dict[str, dict] = {}
            calls_by_node: dict[str, list[dict]] = {}
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
                calls_by_node.setdefault(node, []).append({
                    "attempt": payload.get("attempt"),
                    "input_tokens": int(payload.get("input_tokens") or 0),
                    "cached_tokens": cached,
                    "output_tokens": int(payload.get("output_tokens") or 0),
                    "prefix_hash": payload.get("prefix_hash"),
                    "declared_hash": payload.get("declared_hash"),
                    "working_set_hash": payload.get("working_set_hash"),
                    "prompt_chars": payload.get("prompt_chars"),
                    "segments_recorded": payload.get("segments") != "unavailable",
                })
            for node, entry in by_node.items():
                entry.update(_prompt_shape(calls_by_node.get(node, [])))
            for bucket in (total, *by_node.values()):
                bucket["billed_input_tokens"] = max(
                    bucket["input_tokens"] - bucket["cached_tokens"], 0)
            return {"run_id": str(run_id), "total": total,
                    "by_node": sorted(by_node.values(), key=lambda item: -item["input_tokens"])}

    register_usage()


def _prompt_shape(calls: list[dict]) -> dict:
    """Which part of the prompt moved, and how the gross count splits.

    A segment that never changed is one the provider could cache. A segment that changed on
    every call could not be, which is what turns a large gross count into a large bill.

    The split uses the fact that each call re-sends every earlier prompt. Of the tokens in a
    call, `min(this, previous)` were already sent and the excess is growth. Written that way
    the two add up to the gross count exactly, including when a prompt shrinks — memory gets
    trimmed and a snapshot gets corrected, and a decomposition that assumed monotonic growth
    would quietly stop adding up while still looking plausible.
    """
    if not calls:
        return {"prompt_shape": None}
    distinct = {name: {call[name] for call in calls if call.get(name) is not None}
                for name in ("prefix_hash", "declared_hash", "working_set_hash")}
    resent = 0
    new = 0
    previous = 0
    for call in calls:
        resent += min(call["input_tokens"], previous)
        new += max(call["input_tokens"] - previous, 0)
        previous = call["input_tokens"]
    return {
        "prompt_shape": {
            "segments_recorded": all(call["segments_recorded"] for call in calls),
            "prefix_hashes": len(distinct["prefix_hash"]),
            "declared_hashes": len(distinct["declared_hash"]),
            "working_set_hashes": len(distinct["working_set_hash"]),
            "prefix_stable": len(distinct["prefix_hash"]) <= 1,
            "declared_stable": len(distinct["declared_hash"]) <= 1,
            "working_set_stable": len(distinct["working_set_hash"]) <= 1,
            "new_input_tokens": new,
            "resent_input_tokens": resent,
        },
        "calls": calls,
    }
