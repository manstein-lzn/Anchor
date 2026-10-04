from __future__ import annotations

import hashlib
import io
import json
import threading
import time

import pytest

from anchor.graph_call_rpc import (
    GraphCallRpc, MAX_FRAME_BYTES, PROTOCOL_VERSION, RpcError, _canonical_digest, _kernel_snapshot,
    encode_frame, read_frame, serve_one,
)
from anchor.serve import Scheduler
from anchor.simple import run as runner


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value))


@pytest.fixture
def rpc(tmp_path):
    config = tmp_path / "config.json"
    write(config, {"models": []})
    definition = {"ops": {"call": {"call": {"graph": "child", "mode": "detach"}}},
                  "nodes": [{"id": "invoke", "op": "call"}], "edges": []}
    write(tmp_path / "workspaces/parent/graph.json", definition)
    write(tmp_path / "workspaces/child/graph.json",
          {"ops": {"work": {"run": "printf ok > result.txt"}},
           "nodes": [{"id": "work", "op": "work"}], "edges": []})
    scheduler = Scheduler(tmp_path, config)
    parent = tmp_path / "workspaces/parent/runs/parent-run"
    parent.mkdir(parents=True)
    (parent / "graph.json").write_text(json.dumps(definition))
    runner.RunState(objective="parent", started=runner._now(), status="running").save(parent)
    facade = GraphCallRpc(scheduler)
    spec = definition["ops"]["call"]["call"]
    spec_digest = _canonical_digest(spec)
    graph_digest = _canonical_digest(_kernel_snapshot(definition))
    identity = {"parent_run_id": "parent-run", "parent_graph_digest": graph_digest,
                "node_id": "invoke", "invocation": 1, "call_spec_digest": spec_digest}
    req = {"version": PROTOCOL_VERSION, "op": "invoke", "identity": identity,
           "spec": spec, "input": {}}
    return scheduler, facade, req


def test_facade_admits_idempotently_and_status_is_scoped(rpc):
    scheduler, facade, req = rpc
    first = facade.handle(req)
    again = facade.handle(req)
    assert first["ok"] and first["result"]["status"] == "accepted"
    assert again == first
    status = facade.handle({**req, "op": "status"})
    assert status["ok"] and status["result"]["run"] == first["result"]["run"]
    changed = {**req, "input": {"x": 1}}
    assert not facade.handle(changed)["ok"]
    assert len(list((scheduler.workspace("child") / "runs").glob("*/admission.json"))) == 1


def test_facade_rejects_unverifiable_identity_and_unsafe_artifact_shapes(rpc):
    _, facade, req = rpc
    bad_digest = {**req, "identity": {**req["identity"], "parent_graph_digest": "0" * 64}}
    assert not facade.handle(bad_digest)["ok"]
    bad_spec = {**req, "spec": {**req["spec"], "files": [{"node": "x", "path": "secret", "as": "secret"}]}}
    bad_spec["identity"] = {**req["identity"], "call_spec_digest": _canonical_digest(bad_spec["spec"])}
    # The changed spec no longer belongs to the parent snapshot and is rejected before artifacts.
    assert not facade.handle(bad_spec)["ok"]


def test_versioned_frame_is_bounded_and_roundtrips():
    request = {"version": 1, "op": "status"}
    assert read_frame(io.BytesIO(encode_frame(request))) == request
    with pytest.raises(RpcError, match="size limit"):
        encode_frame({"data": "x" * MAX_FRAME_BYTES})
    out = io.BytesIO()
    serve_one(io.BytesIO(b"\x00\x00\x00\x00"), out, GraphCallRpc(object()))
    response = read_frame(io.BytesIO(out.getvalue()))
    assert response["error"]["code"] == "protocol_error"


def test_protocol_digest_is_sha256_of_canonical_json():
    payload = {"z": 1, "a": [True, None]}
    expected = hashlib.sha256(json.dumps(payload, sort_keys=True, separators=(",", ":"),
                                       ensure_ascii=False, allow_nan=False).encode()).hexdigest()
    assert _canonical_digest(payload) == expected


def test_wait_call_status_and_cancel_stay_with_admitted_identity(rpc):
    scheduler, facade, req = rpc
    parent_workspace = scheduler.workspace("parent")
    parent_snapshot = json.loads((parent_workspace / "runs/parent-run/graph.json").read_text())
    parent_snapshot["ops"]["call"]["call"]["mode"] = "wait"
    (parent_workspace / "runs/parent-run/graph.json").write_text(json.dumps(parent_snapshot))
    write(parent_workspace / "graph.json", parent_snapshot)
    req["spec"] = {"graph": "child", "mode": "wait"}
    req["identity"]["call_spec_digest"] = _canonical_digest(req["spec"])
    req["identity"]["parent_graph_digest"] = _canonical_digest(
        _kernel_snapshot(parent_snapshot))
    write(scheduler.workspace("child") / "graph.json",
          {"ops": {"work": {"run": "sleep 3"}}, "nodes": [{"id": "work", "op": "work"}], "edges": []})
    errors = []
    worker = threading.Thread(target=lambda: errors.append(facade.handle(req)))
    worker.start()
    import hashlib
    control = (parent_workspace / "runs/parent-run/control/.graph-calls" /
               hashlib.sha256(json.dumps(["invoke", 1], ensure_ascii=False,
                                         separators=(",", ":")).encode()).hexdigest())
    deadline = time.monotonic() + 5
    while not (control / "graph-call.json").exists() and time.monotonic() < deadline:
        time.sleep(.01)
    status = facade.handle({**req, "op": "status"})
    assert status["ok"] and status["result"]["status"] in {"running", "stopped"}, status
    cancelled = facade.handle({**req, "op": "cancel"})
    assert cancelled["ok"] and cancelled["result"]["accepted"]
    worker.join(5)
    assert not worker.is_alive()
    assert errors and not errors[0]["ok"]
