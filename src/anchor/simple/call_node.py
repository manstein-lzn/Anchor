"""Bridge a trusted Graph service call into the existing node completion protocol."""

from __future__ import annotations

import json
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path

from anchor.node import COMPLETED, FAILED, NodeOutcome, node_key
from anchor.node.recovery import CompletionFact, record_completion


def run_call(*, handler: Callable | None, spec: dict, node_id: str, invocation: int,
             directory: Path, control: Path, run_input: dict, inputs: tuple[dict, ...],
             cancelled: Callable[[], bool]) -> NodeOutcome:
    """The handler owns admission/recovery and files; this bridge owns accepted completion."""
    if handler is None:
        return NodeOutcome(status=FAILED, reason="Graph calls require a service call_handler; standalone execution is unsupported")
    try:
        payload = handler(spec=spec, node_id=node_id, invocation=invocation,
                          directory=directory, control=control, run_input=run_input,
                          inputs=inputs, cancelled=cancelled)
        _validate_result(payload, spec, directory)
        submission = json.dumps(payload, ensure_ascii=False, sort_keys=True)
    except Exception as exc:  # noqa: BLE001 - callback failure is a failed call node
        return NodeOutcome(status=FAILED, reason=f"Graph call failed: {type(exc).__name__}: {exc}")
    # The same native fact the runner already reads after an interrupted node. The service's
    # accepted reference closes the earlier admission gap; no separate recovery engine lives here.
    record_completion(control, CompletionFact(
        node=node_key(node_id), run=payload["run"], kind="graph_call", submission=submission,
        route=None, command="", at=datetime.now(timezone.utc).isoformat()))
    return NodeOutcome(status=COMPLETED, submission=submission)


def _regular_result(directory: Path, relative: str) -> None:
    path = directory
    for part in relative.split("/"):
        path = path / part
        if path.is_symlink():
            raise ValueError(f"call output must not be a symlink: {relative}")
    if not path.is_file():
        raise ValueError(f"call handler did not produce required file: {relative}")


def _validate_result(payload: object, spec: dict, directory: Path) -> None:
    if not isinstance(payload, dict):
        raise ValueError("call handler must return a JSON object")
    if payload.get("graph") != spec["graph"] or payload.get("mode") != spec["mode"]:
        raise ValueError("call handler returned a different graph or mode")
    if not isinstance(payload.get("run"), str) or not payload["run"]:
        raise ValueError("call handler did not return a child run reference")
    statuses = ("finished",) if spec["mode"] == "wait" else ("accepted", "queued", "running", "finished")
    if payload.get("status") not in statuses:
        raise ValueError(f"call handler did not confirm {spec['mode']} completion: {payload.get('status')!r}")
    if spec["mode"] == "detach" and ("result" in payload or "summary" in payload):
        raise ValueError("detach returns an accepted reference, not business results")
    _regular_result(directory, "call.json")
    if json.loads((directory / "call.json").read_text(encoding="utf-8")) != payload:
        raise ValueError("call.json does not match the accepted call result")
    for name in spec.get("result", {}).get("files", ()):
        _regular_result(directory, f"result/{name}")
