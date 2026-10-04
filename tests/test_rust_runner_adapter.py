import sys

import pytest

from anchor.rust_runner_adapter import RustRunnerAdapter, RustRunnerProtocolError


def fake_host(response: dict[str, object]) -> list[str]:
    response_source = repr(response)
    script = (
        "import json,struct,sys; "
        "data=sys.stdin.buffer.read(); "
        "request=json.loads(data[4:]); "
        f"response=json.dumps({response_source}).encode(); "
        "sys.stdout.buffer.write(struct.pack('>I',len(response))+response)"
    )
    return [sys.executable, "-c", script]


def test_adapter_sends_versioned_start_request_and_validates_response():
    adapter = RustRunnerAdapter(
        fake_host(
            {
                "kind": "run",
                "version": 1,
                "request_id": "req-1",
                "run_id": "run-1",
                "status": "completed",
                "error": None,
            }
        )
    )
    result = adapter.start_or_resume(
        request_id="req-1",
        run_id="run-1",
        snapshot={"entry": "node", "nodes": []},
        input={"question": "x"},
    )
    assert result["status"] == "completed"


def test_adapter_rejects_unsafe_identity_and_bad_response():
    adapter = RustRunnerAdapter(
        fake_host({"kind": "unknown", "version": 1, "request_id": "req"})
    )
    with pytest.raises(RustRunnerProtocolError, match="unsafe"):
        adapter.status(request_id="req", run_id="../escape")
    with pytest.raises(RustRunnerProtocolError, match="unknown"):
        adapter.status(request_id="req", run_id="safe")


def test_adapter_rejects_oversized_snapshot_before_spawning():
    adapter = RustRunnerAdapter(fake_host({}))
    with pytest.raises(RustRunnerProtocolError, match="1 MiB"):
        adapter.start_or_resume(
            request_id="req",
            run_id="safe",
            snapshot={"payload": "x" * (1024 * 1024)},
            input=None,
        )


def test_adapter_can_start_server_configured_bundle_without_path_field():
    adapter = RustRunnerAdapter(
        fake_host(
            {
                "kind": "run",
                "version": 1,
                "request_id": "bundle-req",
                "run_id": "bundle-run",
                "status": "completed",
                "error": None,
            }
        )
    )
    result = adapter.start_bundle(
        request_id="bundle-req", run_id="bundle-run", input={"value": 2}
    )
    assert result["kind"] == "run"
