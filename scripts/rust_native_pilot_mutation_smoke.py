"""Opt-in F2b1 acceptance: one native Pilot Turn creates, updates and runs an Op Graph.

Requires an explicitly supplied Host binary and MODEL_KEYS in the environment.
No dotenv, build, AgentNode, Plugin, business service or model retry is involved.
Host HTTP projections, native locator files and provider recordings are retained;
the driver never queries private SQLite tables or implements an Agent loop.
"""
from __future__ import annotations

import argparse
import json
import math
import os
from pathlib import Path
import re
import shutil
import tempfile
import time
import uuid
from urllib.parse import quote

from rust_low_cost_regression import (
    check_file, collect_usage, file_sha256, host_environment, wait_run,
)
from rust_platform_plugin_smoke import (
    Api, Evidence, MODEL_KEYS, Service, SmokeFailure, read_json, require, unused_ports, wait_until,
)


PILOT_RECORDINGS = "platform/pilot/*/providers/*/provider.recordings/*"
TOOL_NAMES = ("graph_create", "graph_update", "graph_run")
FAILURES = (SmokeFailure, OSError, ValueError, KeyError, TypeError, IndexError)


class EvidenceApi(Api):
    def __init__(self, port: int, evidence: Evidence, timeout: float):
        super().__init__(port, timeout)
        self.evidence = evidence
        self.sequence = 0

    def raw(self, method: str, path: str, body=None) -> tuple[int, bytes]:
        self.sequence += 1
        name = f"http-{self.sequence:04d}.json"
        record = {"method": method, "path": path, "request": body}
        self.evidence.save(name, record)
        try:
            status, raw = super().raw(method, path, body)
        except OSError as error:
            record["error"] = f"{type(error).__name__}: {error}"
            self.evidence.save(name, record)
            raise
        record.update(status=status, response=raw.decode("utf-8", errors="replace"))
        self.evidence.save(name, record)
        return status, raw


def mutation_case(marker: str, graph: str) -> dict:
    require(re.fullmatch(r"[0-9a-f]{32}", marker) is not None, "Marker must be a random lowercase UUID hex")
    initial = {"objective": "Write one local mutation receipt", "input": {}, "entry": "receipt", "agents": {},
               "ops": {"receipt": {"run": "true", "network": False, "wall_time_limit_seconds": 30}},
               "nodes": [{"id": "receipt", "op": "receipt"}], "edges": []}
    updated = {**initial, "ops": {"receipt": {**initial["ops"]["receipt"],
                                            "run": f"sh -c 'printf {marker} > receipt.txt'"}}}
    arguments = [
        {"name": graph, "definition": initial},
        {"graph": graph, "definition": updated},
        {"graph": graph, "objective": updated["objective"], "input": {"marker": marker}},
    ]
    steps = "\n".join(f"{index}. {name} {json.dumps(value, ensure_ascii=False)}"
                      for index, (name, value) in enumerate(zip(TOOL_NAMES, arguments, strict=True), 1))
    prompt = (
        "仅验收隔离本地环境的 Graph 变更与执行。这是我明确授权的一次业务 Turn。"
        "必须严格按以下顺序恰好调用三个工具，每次只调用一个，等待真实工具结果后才执行下一步，参数必须完整原样使用：\n"
        f"{steps}\n"
        "初始 Graph 的 Op 只运行 true，没有产物；更新后仅一个 Op 写 receipt.txt。"
        "不要调用任何额外工具，尤其不要 graph_read、graph_validate 或 run_status；driver 会通过 HTTP 检查。"
        "不得新增 AgentNode、Plugin、网络访问，不浏览论文、不发消息、不发布。"
        "第三个工具返回真实 run ID 后，才给一句简短非空最终回复；此前不要输出文本。"
        "工具失败就报告失败，不重试、不编造工具结果或 run ID。"
    )
    return {"marker": marker, "graph": graph, "initial": initial, "updated": updated,
            "arguments": arguments, "request": {"request_id": uuid.uuid4().hex, "message": prompt}}


def sse_events(raw: str) -> list[dict]:
    events = []
    for block in raw.replace("\r\n", "\n").split("\n\n"):
        lines = block.splitlines()
        data = "\n".join(line[5:].lstrip() for line in lines if line.startswith("data:"))
        if not data:
            continue
        kinds = [line[6:].strip() for line in lines if line.startswith("event:")]
        require(len(kinds) <= 1, "Ambiguous SSE event type")
        value = json.loads(data)
        require(isinstance(value, dict), "SSE data must be an object")
        events.append({"event": kinds[0] if kinds else "message", "data": value})
    return events


def tool_output(value) -> dict:
    require(isinstance(value, list) and len(value) == 1, "Tool output must contain one native content item")
    content = value[0]
    require(isinstance(content, dict), "Tool output content must be an object")
    if content.get("type") == "text":
        require(isinstance(content.get("text"), str), "Native text result is missing text")
        result = json.loads(content["text"])
    else:
        require(content.get("type") == "json", "Unexpected native tool result content")
        result = content.get("value")
    require(isinstance(result, dict), "Tool result must be a JSON object")
    return result


def same_json(left, right) -> bool:
    return json.dumps(left, sort_keys=True, allow_nan=False) == json.dumps(right, sort_keys=True, allow_nan=False)


def check_native(native) -> None:
    require(isinstance(native, dict) and set(native) == {"scope", "session", "run"},
            "Turn must have a complete NativeAssociation")
    require(isinstance(native["scope"], str) and re.fullmatch(r"[0-9a-fA-F]{64}", native["scope"]) is not None,
            "Native scope is not 64 hex digits")
    require(all(type(native[name]) is int and 0 < native[name] < 2 ** 63 for name in ("session", "run")),
            "Native Session/Run IDs must be positive i64 values")


def check_turn(raw: str, case: dict, turn: dict) -> dict:
    events = sse_events(raw)
    require(bool(events) and events[-1]["event"] == "turn", "SSE must end in a durable terminal Turn")
    require(sum(event["event"] == "turn" for event in events) == 1, "SSE has multiple terminal Turns")
    require(same_json(events[-1]["data"], turn) and turn.get("status") == "completed" and not turn.get("error"),
            "SSE terminal Turn differs from its completed durable record")
    require(turn.get("request_id") == case["request"]["request_id"]
            and turn.get("prompt") == case["request"]["message"], "Turn input identity differs")
    check_native(turn.get("native"))
    chunks = [event["data"] for event in events[:-1]]
    require(all(event["event"] == "message" for event in events[:-1]), "SSE contains a delivery error")
    check_tool_sequence(chunks, case)
    calls = [chunk for chunk in chunks if chunk.get("type") == "tool-input-available"]
    outputs = [tool_output(chunk.get("output")) for chunk in chunks if chunk.get("type") == "tool-output-available"]
    for index, definition in enumerate((case["initial"], case["updated"])):
        require(same_json(outputs[index], {"graph": case["graph"], "definition": definition}),
                f"{TOOL_NAMES[index]} did not return the exact saved definition")
    run = outputs[2].get("run")
    require(isinstance(run, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", run) is not None,
            "graph_run did not return a safe actual Run ID")
    require(outputs[2] == {"run": run, "graph": case["graph"], "session": turn.get("session"), "accepted": True}
            and outputs[2].get("accepted") is True, "graph_run admission output differs")
    require(turn.get("runs") == [run], "graph_run output ID differs from Turn.runs")
    for call in calls:
        require(call["toolCallId"].startswith(f"pilot-{turn['native']['run']}-tool-"),
                "Tool call identity differs from the native Run association")
    deltas = [chunk.get("delta") for chunk in chunks if chunk.get("type") == "text-delta"]
    require(all(isinstance(delta, str) for delta in deltas), "Invalid text delta")
    reply = "".join(deltas)
    require(bool(reply.strip()), "Pilot must give a nonempty final text reply")
    return {"tool_calls": 3, "run": run, "native": turn["native"], "reply": reply}


def check_tool_sequence(chunks: list[dict], case: dict) -> None:
    actions = [chunk for chunk in chunks if chunk.get("type", "").startswith("tool-")
               and chunk.get("type") != "tool-input-start"]
    require([chunk.get("type") for chunk in actions] == [
        kind for _name in TOOL_NAMES for kind in ("tool-input-available", "tool-output-available")
    ], "Pilot must use exactly three ordered input/output pairs with no extra actions")
    starts = [chunk for chunk in chunks if chunk.get("type") == "tool-input-start"]
    require(len(starts) == 3, "Pilot must start exactly three native tool calls")
    call_ids = []
    for index, name in enumerate(TOOL_NAMES):
        call, output = actions[index * 2:index * 2 + 2]
        identifier = call.get("toolCallId")
        require(isinstance(identifier, str) and bool(identifier) and identifier not in call_ids,
                "Missing or duplicate tool call ID")
        call_ids.append(identifier)
        require(call.get("toolName") == name and same_json(call.get("input"), case["arguments"][index]),
                f"Expected exact {name} input in strict create/update/run order")
        require(output.get("toolCallId") == identifier, "Tool output belongs to a different call")
        require(starts[index].get("toolName") == name and starts[index].get("toolCallId") == identifier,
                "Unexpected or mismatched tool start")
        require(chunks.index(starts[index]) < chunks.index(call), "Tool input arrived before its start")
        if index:
            require(chunks.index(starts[index]) > chunks.index(actions[index * 2 - 1]),
                    "Pilot issued concurrent or premature tool calls")
    last_output = chunks.index(actions[-1])
    require(not any(chunk.get("type") == "error" for chunk in chunks), "Pilot reported a stream error")
    require(all(index > last_output for index, chunk in enumerate(chunks) if chunk.get("type") == "text-delta"),
            "Pilot replied before the three actual tool results")


def expected_snapshot(definition: dict) -> dict:
    return {**definition, "_module_rounds": {}, "nodes": [
        {"agent": None, "with": None, "plugins": [], "max_rounds": None, **node} for node in definition["nodes"]
    ]}


def check_run(root: Path, case: dict, turn: dict, detail: dict, record: dict, metadata: dict) -> list[dict]:
    run = turn["runs"][0]
    require(detail.get("run") == run and detail.get("graph") == case["graph"]
            and detail.get("active") is False and detail["state"]["status"] == "completed",
            "HTTP Run wrapper does not describe the completed admitted Run")
    require(record.get("run_id") == run and record.get("status") == "completed" and not record.get("error"),
            "Saved Run is not the completed admitted Run")
    require(same_json(record.get("snapshot"), expected_snapshot(case["updated"])),
            "Run frozen Graph differs from graph_update")
    require(same_json(record.get("input"), case["arguments"][2]["input"])
            and same_json(detail["state"]["input"], record.get("input")), "Run input differs")
    require(detail["state"]["objective"] == case["updated"]["objective"], "Run objective differs")
    require(record.get("invocations") == {"receipt": 1} and set(record.get("results", {})) == {"receipt"}
            and len(record["results"]["receipt"]) == 1, "Run did not execute exactly one receipt Op")
    require(not record.get("plugin_bindings") and not record.get("graph_calls") and not detail.get("calls"),
            "Unexpected Plugin or child Graph activity")
    require(metadata.get("run_id") == run and metadata.get("graph") == case["graph"]
            and metadata.get("graph_digest") == record["graph_digest"], "Run metadata identity differs")
    require(metadata.get("pilot") == {"owner": "local", "session": turn["session"], "turn": turn["id"]},
            "Run metadata lost the Pilot Session/Turn association")
    result = record["results"]["receipt"][0]
    require(result["key"] == {"run_id": run, "graph_digest": record["graph_digest"],
                              "node_id": "receipt", "invocation": 1}, "Artifact invocation identity differs")
    files = [check_file(root, record, "receipt", "receipt.txt", case["marker"].encode())]
    manifest = read_json(root / "state/artifacts" / result["commit"]["id"] / "manifest.json")
    require(set(manifest["files"]) == {"receipt.txt"}, "Op committed unexpected files")
    return files


def check_association(root: Path, turn: dict, session: dict, history: dict, case: dict, reply: str) -> dict:
    native = turn.get("native")
    check_native(native)
    require(session.get("id") == turn.get("session") and session.get("run_ids") == turn.get("runs")
            and len(turn.get("runs", [])) == 1, "Session/Turn Run association differs")
    scope = root / "state/platform/pilot" / native["scope"]
    require(scope.is_dir() and not scope.is_symlink(), "Native scope is missing or unsafe")
    locator_path = scope / "session.json"
    store_path = scope / "framework.sqlite3"
    require(all(path.is_file() and not path.is_symlink() for path in (locator_path, store_path)),
            "Native history Store/Session locator did not land")
    locator = read_json(locator_path)
    require(locator == {"version": 1, "framework": "io-harness", "framework_version": "0.86.0",
                        "root": str(scope), "session_id": native["session"]}, "Native Session locator differs from Turn")
    with store_path.open("rb") as store:
        require(store.read(16) == b"SQLite format 3\0", "Native history Store is not SQLite")
    messages = history.get("messages")
    require(messages == [{"role": "user", "text": case["request"]["message"]},
                         {"role": "assistant", "text": reply}], "Public native history differs from the actual Turn")
    return {"native": native, "locator": locator, "history": history,
            "store": str(store_path.relative_to(root)), "private_sql_read": False}


def usage_snapshot(root: Path, evidence: Evidence, name: str) -> dict:
    state = root / "state"
    usage = collect_usage(state, PILOT_RECORDINGS)
    usage["graph_provider_attempts"] = collect_usage(state)["provider_attempts"]
    usage["recording_hashes"] = {
        str(path.relative_to(state)): file_sha256(path)
        for directory in sorted(state.glob(PILOT_RECORDINGS)) if directory.is_dir()
        for path in sorted(directory.rglob("*")) if path.is_file()
    }
    evidence.save(name, usage)
    return usage


def check_usage(usage: dict, turn: dict) -> None:
    require(usage.get("provider_attempts") == usage.get("usage_reported_attempts") == 4
            and usage.get("usage_complete") is True and usage.get("graph_provider_attempts") == 0,
            "Expected exactly four fully metered Pilot requests and no Graph provider requests")
    prefix = f"platform/pilot/{turn['native']['scope']}/providers/{turn['id']}/provider.recordings/"
    for attempt in usage["attempts"]:
        require(attempt["recording"].startswith(prefix) and attempt["status"] == "succeeded",
                "Provider recording belongs to a different Turn or failed request")
        require(all(type(attempt["usage"].get(name)) is int and attempt["usage"][name] >= 0
                    for name in ("prompt_tokens", "completion_tokens", "total_tokens")),
                "Incomplete provider token usage")


def wait_turn(api: Api, service: Service, session: str, turn: str, evidence: Evidence, timeout: float) -> dict:
    def poll():
        service.check()
        record = api.expect("GET", f"/sessions/{quote(session, safe='')}/turns/{quote(turn, safe='')}")["turn"]
        evidence.save("turn-observed.json", record)
        if record["status"] == "running":
            return None
        require(record["status"] == "completed", "Pilot Turn did not complete; no model retry will be made")
        return record
    return wait_until(poll, timeout, "the native mutation Turn")


def prepare(args, evidence: Evidence) -> tuple[Path, dict, dict, int]:
    root = evidence.root
    require(args.binary.is_file() and not args.binary.is_symlink(), "Host binary must be a regular non-symlink file")
    digest = file_sha256(args.binary)
    binary = root / "host"
    shutil.copyfile(args.binary, binary)
    binary.chmod(0o700)
    require(file_sha256(binary) == digest, "Host binary copy hash differs")
    case = mutation_case(uuid.uuid4().hex, f"pilot-mutation-{uuid.uuid4().hex}")
    evidence.save("case.json", case)
    bundle = root / "bundle"
    bundle.mkdir()
    (bundle / "graph.json").write_text(json.dumps(case["initial"]), encoding="utf-8")
    (bundle / "manifest.json").write_text(json.dumps({"format": 1, "graph": "graph.json", "plugins": []}),
                                          encoding="utf-8")
    (root / "catalog").mkdir()
    (root / "library").mkdir()
    port, _unused = unused_ports()
    env = host_environment(root, "mutation-bootstrap", port)
    env["ANCHOR_RUNNER_CATALOG_ROOT"] = str(root / "catalog")
    evidence.save("host-config.json", {"binary_sha256": digest, "environment": env, "timeout": args.timeout})
    return binary, case, env, port


def observe(api: Api, root: Path, evidence: Evidence, case: dict, turn: dict, reply: str, label: str) -> dict:
    session = api.expect("GET", f"/sessions/{turn['session']}")["session"]
    history = api.expect("GET", f"/sessions/{turn['session']}/messages")
    graph = api.expect("GET", f"/graphs/{case['graph']}")
    detail = api.expect("GET", f"/runs/{turn['runs'][0]}")
    record = read_json(root / "state/runs" / f"{turn['runs'][0]}.json")
    metadata = read_json(root / "state/run-metadata" / f"{turn['runs'][0]}.json")
    facts = {"turn": turn, "session": session, "history": history, "graph": graph,
             "run_detail": detail, "run_record": record, "run_metadata": metadata}
    evidence.save(f"{label}-facts.json", facts)
    require(graph.get("graph") == case["graph"] and same_json(graph.get("definition"), case["updated"]),
            "Saved Graph definition differs from graph_update")
    facts["files"] = check_run(root, case, turn, detail, record, metadata)
    facts["association"] = check_association(root, turn, session, history, case, reply)
    require(sorted(path.stem for path in (root / "state/runs").glob("*.json")) == turn["runs"],
            "Pilot unexpectedly admitted an additional Run")
    require(read_json(root / "bundle/graph.json") == case["initial"], "Pilot mutated the bootstrap Graph")
    evidence.save(f"{label}-facts.json", facts)
    return facts


def repeat_turn(api: Api, case: dict, turn: dict, evidence: Evidence, label: str) -> None:
    repeated = api.expect("POST", f"/sessions/{turn['session']}/turns", case["request"], status=202)["turn"]
    evidence.save(f"{label}-turn.json", repeated)
    require(repeated == turn, "request_id replay changed the terminal Turn or native/Run association")


def retain_provider_records(root: Path, evidence: Evidence) -> None:
    for index, directory in enumerate(sorted((root / "state").glob(PILOT_RECORDINGS)), 1):
        if directory.is_dir():
            for name in ("recording.json", "outcome.json"):
                if (directory / name).is_file():
                    evidence.save(f"provider-{index:02d}-{name}", read_json(directory / name))


def run(args, evidence: Evidence) -> dict:
    root = evidence.root
    report = {"status": "failed", "provider": "real configured model", "expected_provider_attempts": 4,
              "business_turns": 1, "model_retry": False}
    service = None
    started = time.monotonic()
    try:
        binary, case, env, port = prepare(args, evidence)
        digest = file_sha256(binary)
        report.update(binary_sha256=digest, graph=case["graph"], marker=case["marker"])
        api = EvidenceApi(port, evidence, min(args.timeout, 30))
        service = Service("host", [str(binary), "serve"], env, evidence)
        wait_until(lambda: api.ready(service, "/health"), 30, "isolated native mutation Host")
        session = api.expect("POST", "/sessions", {"id": f"pilot-mutation-{uuid.uuid4().hex}"}, status=201)["session"]["id"]
        accepted = api.expect("POST", f"/sessions/{session}/turns", case["request"], status=202)["turn"]
        evidence.save("accepted-turn.json", accepted)
        turn = wait_turn(api, service, session, accepted["id"], evidence, args.timeout)
        evidence.save("turn.json", turn)
        require(turn["id"] == accepted["id"] and turn["session"] == session, "Turn admission identity changed")
        status, raw = api.raw("GET", f"/sessions/{session}/turns/{turn['id']}/events")
        evidence.save("turn-sse.json", raw.decode())
        require(status == 200, "Pilot SSE delivery failed")
        checked = check_turn(raw.decode(), case, turn)
        report.update(checked, session=session, turn=turn["id"])
        wait_run(api, service, checked["run"], evidence, args.timeout)
        facts = observe(api, root, evidence, case, turn, checked["reply"], "completed")
        usage = usage_snapshot(root, evidence, "usage-before-replay.json")
        check_usage(usage, turn)
        repeat_turn(api, case, turn, evidence, "replayed")
        require(usage_snapshot(root, evidence, "usage-after-replay.json") == usage,
                "request_id replay appended or changed provider requests")
        service.stop()
        service = Service("host-restarted", [str(binary), "serve"], env, evidence)
        wait_until(lambda: api.ready(service, "/health"), 30, "restarted native mutation Host")
        restored = api.expect("GET", f"/sessions/{session}/turns/{turn['id']}")["turn"]
        require(restored == turn, "Host restart changed the terminal Turn")
        require(observe(api, root, evidence, case, restored, checked["reply"], "restarted") == facts,
                "Host restart changed Graph/Run/Session/native history facts")
        repeat_turn(api, case, turn, evidence, "restart-replayed")
        require(usage_snapshot(root, evidence, "usage-after-restart.json") == usage,
                "Restart or replay appended or changed provider requests")
        require(file_sha256(binary) == digest, "Pinned Host binary changed")
        report.update(status="passed", files=facts["files"], idempotency=True, restart=True,
                      native_history="public Host projection + SSE + persisted native locator/Store")
    except FAILURES as error:
        report["failure"] = evidence.redact(f"{type(error).__name__}: {error}")
    finally:
        if service is not None:
            service.stop()
        try:
            final_usage = usage_snapshot(root, evidence, "usage-final.json")
            report.update(final_usage)
            retain_provider_records(root, evidence)
            if report["status"] == "passed":
                check_usage(final_usage, turn)
                require(final_usage == usage, "Provider requests changed during Host shutdown")
        except FAILURES as error:
            report.update(status="failed", failure=report.get("failure", evidence.redact(f"{type(error).__name__}: {error}")))
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
        evidence.save("evidence.json", report)
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="Existing Host binary; never builds implicitly")
    parser.add_argument("--evidence-root", type=Path, help="New, nonexistent directory; defaults to a fresh temporary root")
    parser.add_argument("--timeout", type=float, default=120, help="Positive Turn/Run wait timeout in seconds (default: 120)")
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be finite and positive")
    if any(not os.environ.get(name) for name in MODEL_KEYS):
        parser.exit(2, "Missing explicit MODEL_KEYS configuration; no model request was made.\n")
    args.binary = args.binary.absolute()
    if not args.binary.is_file() or args.binary.is_symlink():
        parser.error("--binary must be a regular non-symlink Host binary")
    if args.evidence_root:
        root = args.evidence_root.absolute()
        try:
            root.mkdir(mode=0o700, parents=True, exist_ok=False)
        except OSError as error:
            parser.exit(2, f"Evidence root must be new and nonexistent: {error}\n")
    else:
        root = Path(tempfile.mkdtemp(prefix="anchor-native-pilot-mutation-"))
    evidence = Evidence(root, tuple(os.environ[name] for name in MODEL_KEYS))
    report = run(args, evidence)
    print(json.dumps({"status": report["status"], "evidence": str(root / "evidence.json"),
                      "provider_attempts": report.get("provider_attempts"), "reported_tokens": report.get("reported_tokens"),
                      "failure": report.get("failure")}, ensure_ascii=False), flush=True)
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
