"""Opt-in bounded real-provider acceptance for the native Pilot Session slice."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import tempfile
import time
import uuid

from rust_low_cost_regression import collect_usage, file_sha256, host_environment
from rust_platform_plugin_smoke import (
    Api, Evidence, MODEL_KEYS, Service, SmokeFailure, require, unused_ports, wait_until,
)


def sse_chunks(raw: str) -> list[dict]:
    chunks = []
    for block in raw.replace("\r\n", "\n").split("\n\n"):
        lines = block.splitlines()
        if any(line == "event: turn" for line in lines):
            continue
        data = "\n".join(line[5:].strip() for line in lines if line.startswith("data:"))
        if data:
            chunks.append(json.loads(data))
    return chunks


def check_first_turn(raw: str, marker: str) -> dict:
    chunks = sse_chunks(raw)
    calls = [chunk for chunk in chunks if chunk.get("type") == "tool-input-available"]
    require(len(calls) == 1 and calls[0].get("toolName") == "graph_read",
            "Pilot must use exactly one read-only graph_read call")
    outputs = [chunk for chunk in chunks if chunk.get("type") == "tool-output-available"
               and chunk.get("toolCallId") == calls[0].get("toolCallId")]
    require(len(outputs) == 1 and marker in json.dumps(outputs), "Graph tool result is missing its fixture marker")
    text = "".join(chunk.get("delta", "") for chunk in chunks if chunk.get("type") == "text-delta")
    require(marker in text, "Pilot reply does not reflect the actual Graph result")
    require("event: turn" in raw and '"status":"completed"' in raw, "SSE lacks a completed terminal event")
    return {"tool_calls": len(calls), "reply": text}


def wait_turn(api: Api, service: Service, session: str, turn: str, timeout: float) -> dict:
    def poll():
        service.check()
        record = api.expect("GET", f"/sessions/{session}/turns/{turn}")["turn"]
        if record["status"] == "running":
            return None
        require(record["status"] == "completed", "Pilot Turn did not complete; inspect its durable state")
        return record
    return wait_until(poll, timeout, "native Pilot Turn")


def run(args, evidence: Evidence) -> dict:
    root = evidence.root
    report = {"status": "failed", "provider": "real configured model", "business_effects": 0}
    service = None
    started = time.monotonic()
    try:
        require(args.binary.is_file() and not args.binary.is_symlink(), "Host binary must be a regular non-symlink file")
        binary = root / "host"
        shutil.copyfile(args.binary, binary)
        binary.chmod(0o700)
        report["binary_sha256"] = digest = file_sha256(binary)
        marker = uuid.uuid4().hex
        graph = {"objective": marker, "entry": "idle", "ops": {"idle": {"run": "true"}},
                 "nodes": [{"id": "idle", "op": "idle"}], "edges": []}
        bundle = root / "bundle"
        bundle.mkdir()
        (bundle / "graph.json").write_text(json.dumps(graph), encoding="utf-8")
        (bundle / "manifest.json").write_text(json.dumps({"format": 1, "graph": "graph.json", "plugins": []}), encoding="utf-8")
        port, _unused = unused_ports()
        env = host_environment(root, "pilot-fixture", port)
        api = Api(port, timeout=30)
        service = Service("host", [str(binary), "serve"], env, evidence)
        wait_until(lambda: api.ready(service, "/health"), 30, "native Pilot Host")
        session = api.expect("POST", "/sessions", {"id": "pilot-smoke"}, status=201)["session"]["id"]
        first = {"request_id": uuid.uuid4().hex, "message": (
            "仅验收原生会话。必须且只调用一次 graph_read 读取 pilot-fixture，不调用任何其他工具，不运行 Graph。"
            "回复该定义的 objective 原值，不要创造或更改这个值，其他内容一句话以内。")}
        accepted = api.expect("POST", f"/sessions/{session}/turns", first, status=202)["turn"]
        turn = wait_turn(api, service, session, accepted["id"], args.timeout)
        evidence.save("first-turn.json", turn)
        status, raw = api.raw("GET", f"/sessions/{session}/turns/{turn['id']}/events")
        require(status == 200, "Pilot SSE delivery failed")
        evidence.save("first-sse.json", raw.decode())
        report.update(check_first_turn(raw.decode(), marker))
        repeated = api.expect("POST", f"/sessions/{session}/turns", first, status=202)["turn"]
        require(repeated == turn, "request_id retry did not retain the same terminal Turn")
        second = {"request_id": uuid.uuid4().hex, "message": "先回忆上一轮，再必须且只调用一次 graph_read 读取 pilot-fixture，核对 objective 原值。仅回复原值，不调用其他工具。"}
        accepted = api.expect("POST", f"/sessions/{session}/turns", second, status=202)["turn"]
        second_turn = wait_turn(api, service, session, accepted["id"], args.timeout)
        evidence.save("second-turn.json", second_turn)
        _, raw = api.raw("GET", f"/sessions/{session}/turns/{second_turn['id']}/events")
        evidence.save("second-sse.json", raw.decode())
        check_first_turn(raw.decode(), marker)
        second_calls = sorted((root / "state/platform/pilot").glob(f"*/providers/{second_turn['id']}/provider.recordings/*/request.json"))
        require(second_calls and marker in json.dumps(json.loads(second_calls[0].read_text())["messages"]),
                "Second Turn's actual provider request lost the earlier native reply")
        history = api.expect("GET", f"/sessions/{session}/messages")
        evidence.save("messages.json", history)
        require(marker in json.dumps(history), "Native history does not contain the observed result")
        service.stop()
        service = Service("restarted-host", [str(binary), "serve"], env, evidence)
        wait_until(lambda: api.ready(service, "/health"), 30, "restarted native Host")
        require(api.expect("GET", f"/sessions/{session}/messages") == history, "Restart changed the native history")
        require(api.expect("POST", f"/sessions/{session}/turns", first, status=202)["turn"] == turn,
                "Restart request retry changed Turn identity")
        require(not (root / "state/runs").exists(), "Pilot unexpectedly executed a hidden Graph")
        require(file_sha256(binary) == digest, "Pinned Host binary changed")
        report.update(status="passed", session=session, turns=2, restart=True, idempotency=True, hidden_graph=False)
    except (SmokeFailure, OSError, ValueError, KeyError, TypeError, IndexError) as error:
        report["failure"] = evidence.redact(f"{type(error).__name__}: {error}")
    finally:
        if service is not None:
            service.stop()
        report.update(collect_usage(root / "state", "platform/pilot/*/providers/*/provider.recordings/*"))
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
        if report["status"] == "passed" and not (report["provider_attempts"] == 4 and report["usage_complete"]):
            report.update(status="failed", failure="Unexpected provider request count or incomplete usage")
        evidence.save("evidence.json", report)
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--evidence-root", type=Path)
    parser.add_argument("--timeout", type=float, default=120)
    args = parser.parse_args()
    if any(not os.environ.get(name) for name in MODEL_KEYS):
        parser.exit(2, "Missing provider configuration; no model request was made.\n")
    args.binary = args.binary.absolute()
    root = args.evidence_root.absolute() if args.evidence_root else Path(tempfile.mkdtemp(prefix="anchor-pilot-"))
    if args.evidence_root:
        root.mkdir(parents=True, exist_ok=False)
    evidence = Evidence(root, tuple(os.environ[name] for name in MODEL_KEYS))
    report = run(args, evidence)
    print(json.dumps({"status": report["status"], "evidence": str(root / "evidence.json"),
                      "provider_attempts": report["provider_attempts"], "reported_tokens": report["reported_tokens"],
                      "failure": report.get("failure")}, ensure_ascii=False), flush=True)
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
