"""Opt-in real Pilot acceptance through the Python Session API and Rust HTTP.

The original PydanticAI/Harness Pilot creates, queries and controls one Rust
Op-only Run. Both services restart with their retained roots before the same
Session observes that Run again. Evidence stays under .local/rust-pilot-*.
No business messages are sent and no runtime facts are edited by this script.
"""
from __future__ import annotations

import argparse
import asyncio
import hashlib
from http.client import HTTPConnection
import json
import math
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import time
from urllib.parse import urlsplit
from uuid import uuid4

from rust_platform_plugin_smoke import Api, Evidence, Service, SmokeFailure, require, unused_ports, wait_until


ROOT = Path(__file__).resolve().parents[1]
SESSION = "rust-pilot"
GRAPH = "pilot-op-check"
MODEL_KEYS = ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def json_value(value):
    if isinstance(value, str):
        try:
            return json.loads(value)
        except ValueError:
            pass
    return value


def tool_results(chunks: list[dict]) -> list[dict]:
    calls = {}
    results = []
    for chunk in chunks:
        identifier = chunk.get("toolCallId")
        kind = chunk.get("type")
        if identifier and kind in {"tool-input-start", "tool-input-available"}:
            call = calls.setdefault(identifier, {"id": identifier})
            call.update({key: chunk[key] for key in ("toolName", "input") if key in chunk})
        elif identifier and kind == "tool-output-available":
            require(identifier in calls, "A tool result has no preceding model call")
            results.append({**calls[identifier], "output": json_value(chunk["output"])})
    return results


def must_tool(turn: dict, name: str, predicate) -> dict:
    matches = [item for item in turn["tools"] if item.get("toolName") == name and predicate(item)]
    require(bool(matches), f"Missing successful {name} tool evidence in {turn['label']}")
    return matches[-1]


def definition(nonce: str, controls: bool, timeout: float) -> dict:
    first = f"printf '%s\\n' 'first-ok {nonce}' > first.txt"
    if controls:
        first = (
            f"printf '%s\\n' '{nonce}' > gate.ready; i=0; "
            "while [ ! -f gate.release ]; do i=$((i + 1)); "
            f"[ \"$i\" -le {math.ceil(timeout * 10)} ] || exit 73; sleep 0.1; done; "
            + first
        )
    return {
        "entry": "first", "objective": "Isolated Pilot and Rust Run acceptance", "agents": {},
        "ops": {
            "first": {"run": first, "writes": ["first.txt"], "wall_time_limit_seconds": timeout + 60},
            "finish": {"run": "cat /in/first/first.txt > result.txt; "
                       f"printf '%s\\n' 'second-ok {nonce}' >> result.txt", "writes": ["result.txt"]},
        },
        "nodes": [{"id": "first", "op": "first"}, {"id": "finish", "op": "finish"}],
        "edges": [{"from": "first", "to": "finish"}],
    }


class Deployment:
    def __init__(self, repo: Path, python: Path, binary: Path, evidence: Evidence):
        self.repo, self.python, self.binary, self.evidence = repo, python, binary, evidence
        self.paths = {name: evidence.root / name for name in ("platform", "bootstrap", "catalog", "state", "work")}
        self.services: list[Service] = []
        self.generation = 0
        for path in (self.paths["bootstrap"], self.paths["catalog"], self.paths["platform"] / "library/plugins",
                     self.paths["platform"] / "library/tools"):
            path.mkdir(parents=True)
        (self.paths["bootstrap"] / "graph.json").write_text(json.dumps({
            "entry": "start", "objective": "Untriggered bootstrap", "agents": {},
            "ops": {"start": {"run": "true"}}, "nodes": [{"id": "start", "op": "start"}], "edges": [],
        }), encoding="utf-8")
        (self.paths["bootstrap"] / "manifest.json").write_text(json.dumps({
            "format": 1, "graph": "graph.json", "plugins": [],
        }), encoding="utf-8")
        (evidence.root / "runtime.json").write_text("{}\n", encoding="utf-8")

    def start(self) -> None:
        self.generation += 1
        rust_port, python_port = unused_ports()
        base = {"PATH": "/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1", "PYTHONUNBUFFERED": "1",
                "ANCHOR_API_KEYS": "", "ANCHOR_API_KEY": ""}
        rust_env = {
            **base, "ANCHOR_RUNNER_BUNDLE_ROOT": str(self.paths["bootstrap"]),
            "ANCHOR_RUNNER_CATALOG_ROOT": str(self.paths["catalog"]),
            "ANCHOR_RUNNER_STATE_ROOT": str(self.paths["state"]),
            "ANCHOR_RUNNER_WORKSPACE_ROOT": str(self.paths["work"]),
            "ANCHOR_RUNNER_LIBRARY_ROOT": str(self.paths["platform"] / "library"),
            "ANCHOR_RUNNER_GRAPH_NAME": "bootstrap", "ANCHOR_RUNNER_LISTEN": f"127.0.0.1:{rust_port}",
            "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,printf,sleep,true",
        }
        python_env = {**base, **{name: os.environ[name] for name in MODEL_KEYS},
                      "PYTHONPATH": str(self.repo / "src"), "ANCHOR_RUNTIME_BACKEND": "rust",
                      "ANCHOR_RUNTIME_URL": f"http://127.0.0.1:{rust_port}"}
        for name in ("ANCHOR_MODEL_WIRE_API", "ANCHOR_MODEL_CONTEXT_WINDOW", "SSL_CERT_FILE", "SSL_CERT_DIR",
                     "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"):
            if os.environ.get(name):
                python_env[name] = os.environ[name]
        python_env.setdefault("ANCHOR_MODEL_WIRE_API", "responses")
        self.rust, self.api = Api(rust_port), Api(python_port)
        self.services.append(Service(f"rust-{self.generation}", [str(self.binary), "serve"], rust_env, self.evidence))
        wait_until(lambda: self.rust.ready(self.services[0], "/health"), 30, "Rust HTTP readiness")
        self.services.append(Service(f"python-{self.generation}", [str(self.python), "-m", "anchor", "--root",
                                     str(self.paths["platform"]), "--config", str(self.evidence.root / "runtime.json"),
                                     "--host", "127.0.0.1", "--port", str(python_port)], python_env, self.evidence))
        wait_until(lambda: self.api.ready(self.services[1], "/graphs"), 30, "Python HTTP readiness")

    def check(self) -> None:
        for service in self.services:
            service.check()

    def stop(self) -> None:
        for service in reversed(self.services):
            service.stop()
        self.services.clear()


class Pilot:
    def __init__(self, deployment: Deployment, timeout: float):
        self.deployment, self.timeout = deployment, timeout
        self.turns: list[dict] = []

    def session(self) -> dict:
        return self.deployment.api.expect("GET", f"/sessions/{SESSION}")["session"]

    def turn(self, label: str, prompt: str) -> dict:
        api, evidence = self.deployment.api, self.deployment.evidence
        body = {"request_id": f"rust-pilot-{label}-{uuid4()}", "message": prompt}
        path = f"/sessions/{SESSION}/turns"
        accepted = api.expect("POST", path, body, status=202)["turn"]
        require(api.expect("POST", path, body, status=202)["turn"]["id"] == accepted["id"],
                "A repeated submission started another Pilot turn")
        endpoint = urlsplit(api.base)
        client = HTTPConnection(endpoint.hostname, endpoint.port, timeout=20)
        chunks, wire, terminal = [], [], None
        result = {"label": label, "request": body, "id": accepted["id"], "tools": [], "terminal": None}
        self.turns.append(result)
        deadline = time.monotonic() + self.timeout
        try:
            client.request("GET", f"{path}/{accepted['id']}/events")
            response = client.getresponse()
            require(response.status == 200, "Pilot SSE did not open")
            while terminal is None:
                self.deployment.check()
                require(time.monotonic() < deadline, f"Pilot turn timed out: {label}")
                line = response.readline()
                require(bool(line), "Pilot SSE ended before its terminal record")
                wire.append(line.decode("utf-8"))
                if line.startswith(b"data: "):
                    item = json.loads(line[6:])
                    chunks.append(item)
                    if item.get("id") == accepted["id"] and "status" in item:
                        terminal = item
            result.update({"tools": tool_results(chunks), "terminal": terminal})
            require(terminal["status"] == "completed", f"Pilot turn did not complete: {label}")
            require(api.expect("POST", path, body, status=202)["turn"]["id"] == accepted["id"],
                    "A settled submission started another Pilot turn")
            return result
        finally:
            client.close()
            evidence.save(f"turn-{label}.json", {**result, "chunks": chunks, "wire": "".join(wire)})


def run_record(deployment: Deployment, run: str) -> dict:
    return read_json(deployment.paths["state"] / "runs" / f"{run}.json")


def wait_run(deployment: Deployment, run: str, status: str, timeout: float) -> dict:
    def ready():
        deployment.check()
        detail = deployment.api.expect("GET", f"/runs/{run}")
        deployment.evidence.save(f"run-{status}.json", detail)
        require(detail.get("backend") == "rust", "The platform Run is not Rust-owned")
        require(detail["state"]["status"] not in {"failed", "aborted", "stopped", "waiting_recovery"},
                "The Rust Run reached an unexpected terminal or recovery state")
        return detail if detail["state"]["status"] == status and not detail["active"] else None

    return wait_until(ready, timeout, f"Run {status}")


def create_graph(pilot: Pilot, graph: dict) -> None:
    turn = pilot.turn("create", "请使用工具完成以下操作：先 graph_list 查看已有图，然后 graph_validate 校验下面的完整 JSON；"
                      f"校验成功后 graph_create 创建名为 {GRAPH} 的图。只创建，不运行，也不改动定义。"
                      "工具返回后用一句话报告结果，不请求额外确认。\n" + json.dumps(graph, ensure_ascii=False))
    must_tool(turn, "graph_list", lambda item: any(row.get("graph") == "bootstrap" for row in (
        item["output"].get("graphs", []) if isinstance(item["output"], dict) else item["output"])))
    must_tool(turn, "graph_validate", lambda item: item["input"]["definition"] == graph
              and item["output"].get("valid") is True)
    must_tool(turn, "graph_create", lambda item: item["input"]["name"] == GRAPH
              and item["input"]["definition"] == graph and item["output"].get("http_status") == 201)
    require(pilot.deployment.api.expect("GET", f"/graphs/{GRAPH}")["definition"] == graph,
            "The Pilot changed the requested Graph definition")
    require(pilot.deployment.api.expect("GET", "/runs")["runs"] == [], "Creating or validating the Graph started a Run")


def start_run(pilot: Pilot, graph: dict, controls: bool) -> str:
    control = "拿到 graph_run 返回的 Run ID 后立即 run_pause 请求暂停这个 Run；本地验收程序会放行第一节点。" if controls else ""
    turn = pilot.turn("start", f"先 graph_read 核查 {GRAPH} 的定义，然后只调用一次 graph_run 启动它。"
                      + control + "不得重新创建或更新图，不得启动第二个 Run。工具返回后简短报告结果。")
    must_tool(turn, "graph_read", lambda item: item["output"].get("definition") == graph)
    starts = [item for item in turn["tools"] if item.get("toolName") == "graph_run"
              and item["output"].get("http_status") == 202]
    require(len(starts) == 1, "Expected exactly one successful Pilot graph_run")
    output = starts[0]["output"]
    run = output["run"]
    require(output.get("session") == SESSION and pilot.session()["run_ids"] == [run],
            "The Pilot did not associate the Rust Run with this Session")
    if controls:
        must_tool(turn, "run_pause", lambda item: item["input"]["run"] == run
                  and item["output"].get("http_status") == 202 and item["output"].get("run") == run)
    return run


def release_first(deployment: Deployment, run: str, nonce: str, timeout: float) -> str:
    detail = deployment.api.expect("GET", f"/runs/{run}")
    require(detail["state"]["status"] == "running" and detail.get("control_requested") == "pause",
            "Rust did not retain the Pilot pause request while the first Op was active")

    def marker():
        matches = [path for path in (deployment.paths["work"] / run).rglob("gate.ready")
                   if path.read_text(encoding="utf-8") == nonce + "\n"]
        require(len(matches) <= 1, "Multiple first-Op fixture gates exist")
        return matches[0] if matches else None

    ready = wait_until(marker, timeout, "the first Op fixture gate")
    release = ready.with_name("gate.release")
    require(not release.exists(), "The first Op was already released")
    # This is the Op's declared fixture input, not a Run status or completion fact.
    release.write_text("release after the real Pilot pause request\n", encoding="utf-8")
    deployment.evidence.save("fixture-release.json", {"run": run, "before": detail,
                             "workspace_input": str(release.relative_to(deployment.evidence.root))})
    wait_run(deployment, run, "paused", timeout)
    record = run_record(deployment, run)
    require(set(record["results"]) == {"first"} and len(record["results"]["first"]) == 1,
            "The pause boundary did not stop before the second Op")
    return record["results"]["first"][0]["commit"]["id"]


def inspect_run(pilot: Pilot, label: str, run: str, node: str, path: str, text: str, status: str,
                session_wait: bool = False) -> dict:
    wait = "先调用 session_wait 核查本会话已关联的 Run，然后" if session_wait else "请"
    turn = pilot.turn(label, f"{wait} run_status 查询现有 Run {run}，再 artifact_read 读取节点 {node} 的 {path}。"
                      "只查询和读取，不要创建、更新、运行或控制任何图，不调用 session_ask。工具返回后简短报告实际结果。")
    must_tool(turn, "run_status", lambda item: item["input"]["run"] == run
              and item["output"].get("backend") == "rust" and item["output"]["state"]["status"] == status)
    must_tool(turn, "artifact_read", lambda item: item["input"] == {"run": run, "node": node, "path": path}
              and item["output"].get("http_status") == 200 and item["output"].get("text") == text)
    if session_wait:
        must_tool(turn, "session_wait", lambda item: item["output"]["session"]["id"] == SESSION
                  and item["output"]["session"]["run_ids"] == [run]
                  and len(item["output"]["runs"]) == 1 and item["output"]["runs"][0]["run"] == run)
    return turn


def resume_run(pilot: Pilot, run: str) -> None:
    turn = pilot.turn("resume", f"这是同一个会话在两服务重启后的继续。先 session_wait 核查已经关联的 Run {run}，"
                      "确认现有 Run 仍暂停后，只 run_resume 一次恢复它。不得 graph_run 或创建新的 Run。"
                      "工具返回后简短报告，不提前声称已经完成。")
    must_tool(turn, "session_wait", lambda item: item["output"]["session"]["run_ids"] == [run]
              and len(item["output"]["runs"]) == 1 and item["output"]["runs"][0]["run"] == run
              and item["output"]["runs"][0]["state"]["status"] == "paused")
    must_tool(turn, "run_resume", lambda item: item["input"]["run"] == run
              and item["output"].get("http_status") == 202 and item["output"].get("run") == run)


def harness_records(pilot: Pilot, store, conversation_id: str) -> dict:
    headers = asyncio.run(store.list_runs(conversation_id=conversation_id))
    require(bool(headers), "The native Pilot Harness record is empty")
    recorded_calls = {}
    for header in headers:
        require(asyncio.run(store.get_run(run_id=header.run_id)) is not None, "A native Harness header is missing")
        events = asyncio.run(store.list_events(run_id=header.run_id))
        snapshot = asyncio.run(store.latest_snapshot(run_id=header.run_id, include_interrupted=True))
        require(bool(events) and snapshot is not None and bool(snapshot.messages),
                "A native Pilot Harness event record or message snapshot is missing")
        recorded_calls[header.run_id] = {(event.tool_call_id, event.tool_name) for event in events
                                        if event.tool_call_id and event.tool_name}
    bindings = {}
    for turn in pilot.turns:
        wanted = {(tool["id"], tool["toolName"]) for tool in turn["tools"]}
        require(bool(wanted), "A Pilot turn contains no actual tool results")
        matches = [run for run, calls in recorded_calls.items() if wanted <= calls]
        require(len(matches) == 1, "A Pilot turn could not be bound to one native Harness execution")
        bindings[turn["id"]] = matches[0]
        for tool in turn["tools"]:
            effect = asyncio.run(store.get_tool_effect(run_id=matches[0], tool_call_id=tool["id"]))
            require(effect is not None and effect.tool_name == tool["toolName"] and effect.status == "completed",
                    "A streamed tool result lacks its completed native Harness effect")
    return {"native_harness_runs": [header.run_id for header in headers], "turn_harness_bindings": bindings}


def native_history(pilot: Pilot, label: str) -> dict:
    from pydantic_ai.messages import ModelMessagesTypeAdapter
    from anchor.pilot import step_store
    from anchor.session import SessionStore

    root, evidence = pilot.deployment.paths["platform"], pilot.deployment.evidence
    sessions = SessionStore(root)
    session = sessions.get(SESSION)
    saved = asyncio.run(sessions.conversation_store().get(conversation_id=session.conversation_id))
    evidence.save(f"model-history-{label}.json", json.loads(ModelMessagesTypeAdapter.dump_json(saved.messages)))
    calls, returns, models = [], [], []
    for message in saved.messages:
        if message.kind == "response":
            models.append(message.model_name)
        for part in message.parts:
            if part.part_kind == "tool-call":
                calls.append(part)
            elif part.part_kind == "tool-return":
                returns.append(part)
    for turn in pilot.turns:
        for tool in turn["tools"]:
            require(any(part.tool_call_id == tool["id"] and part.tool_name == tool["toolName"]
                        and part.args_as_dict() == tool["input"] for part in calls),
                    "A streamed model tool call is absent from the native conversation")
            require(any(part.tool_call_id == tool["id"] and part.tool_name == tool["toolName"]
                        and json_value(part.content) == tool["output"] and part.outcome == "success" for part in returns),
                    "A streamed tool result differs from the native conversation")
    records = harness_records(pilot, step_store(root), session.conversation_id)
    require(bool(models), "No real Pilot model responses were persisted")
    return {"conversation_id": session.conversation_id, "pilot_model_responses": len(models),
            "pilot_models_observed": sorted({model for model in models if model}),
            "native_tool_results_verified": len(returns), **records}


def verify_final(pilot: Pilot, run: str, graph: dict, nonce: str, first_commit: str | None) -> dict:
    deployment = pilot.deployment
    platform = deployment.paths["platform"]
    runs = deployment.api.expect("GET", "/runs")["runs"]
    require([item["run"] for item in runs] == [run], "A second Run exists after the Session resumed")
    require(pilot.session()["run_ids"] == [run], "The Session no longer refers to exactly the original Run")
    require(len(list((deployment.paths["state"] / "runs").glob("*.json"))) == 1, "Rust persisted multiple Runs")
    record = run_record(deployment, run)
    require(record["status"] == "completed" and set(record["results"]) == {"first", "finish"},
            "The two-Op Rust Run did not complete")
    artifact_hashes = {}
    for node, path, expected in (("first", "first.txt", f"first-ok {nonce}\n"),
                                 ("finish", "result.txt", f"first-ok {nonce}\nsecond-ok {nonce}\n")):
        results = record["results"][node]
        require(len(results) == 1 and results[0]["key"]["invocation"] == 1, "A completed Op was replayed")
        commit = results[0]["commit"]["id"]
        if node == "first" and first_commit is not None:
            require(commit == first_commit, "The first Op commit changed after pause and restart")
        artifact = deployment.paths["state"] / "artifacts" / commit / "files" / path
        raw = artifact.read_bytes()
        require(raw == expected.encode(), "The authoritative Rust artifact contains unexpected data")
        status, downloaded = deployment.api.raw("GET", f"/runs/{run}/files/{node}/{path}?download=1")
        require(status == 200 and downloaded == raw, "The platform download differs from the Rust artifact")
        artifact_hashes[f"{node}/{path}"] = hashlib.sha256(raw).hexdigest()
    require(read_json(deployment.paths["catalog"] / GRAPH / "graph.json") == graph, "The saved Rust Graph changed")
    require(not list((platform / "workspaces").iterdir()) and not list(platform.rglob("graph.json")),
            "Python created an authoritative Graph copy")
    require(all(path.is_relative_to(platform / "state/pilot-steps") for path in platform.rglob("run.json")),
            "Python created a Graph Run record outside the original Pilot Harness store")
    require(all(not (platform / name).exists() for name in ("runs", "artifacts", "io-harness", "run-metadata")),
            "Rust execution facts were duplicated into the Python root")
    return {"rust_artifact_sha256": artifact_hashes, "rust_run_count": 1,
            "python_authoritative_graph_run_copies": 0, "op_invocations": {"first": 1, "finish": 1}}


def exercise(pilot: Pilot, controls: bool, nonce: str) -> dict:
    deployment = pilot.deployment
    graph = definition(nonce, controls, pilot.timeout)
    create_graph(pilot, graph)
    run = start_run(pilot, graph, controls)
    first_commit = release_first(deployment, run, nonce, pilot.timeout) if controls else None
    if not controls:
        wait_run(deployment, run, "completed", pilot.timeout)
    inspect_run(pilot, "before-restart", run, "first", "first.txt", f"first-ok {nonce}\n",
                "paused" if controls else "completed")
    before = pilot.session()
    deployment.evidence.save("session-before-restart.json", before)
    native_history(pilot, "before-restart")
    previous = pilot.turns[-1]
    deployment.stop()
    deployment.start()
    after = pilot.session()
    deployment.evidence.save("session-after-restart.json", after)
    require(after["conversation_id"] == before["conversation_id"] and after["run_ids"] == [run],
            "Restart lost the original Session conversation or Run association")
    replay = deployment.api.expect("POST", f"/sessions/{SESSION}/turns", previous["request"], status=202)["turn"]
    require(replay["id"] == previous["id"] and replay["status"] == "completed",
            "Restart replayed a settled Pilot request")
    deployment.evidence.save("settled-turn-replay.json", replay)
    if controls:
        wait_run(deployment, run, "paused", pilot.timeout)
        resume_run(pilot, run)
        wait_run(deployment, run, "completed", pilot.timeout)
    inspect_run(pilot, "after-restart", run, "finish", "result.txt", f"first-ok {nonce}\nsecond-ok {nonce}\n",
                "completed", session_wait=True)
    final = verify_final(pilot, run, graph, nonce, first_commit)
    native = native_history(pilot, "after-restart")
    deployment.evidence.save("session-final.json", pilot.session())
    deployment.evidence.save("rust-run-final.json", deployment.rust.expect("GET", f"/runs/{run}"))
    return {"run": run, "session": SESSION, "graph": GRAPH, "service_generations": deployment.generation,
            "pilot_turns": len(pilot.turns), "real_pilot_pause_resume": controls,
            "control_scope": "Real Pilot pause/resume with an Op fixture gate" if controls
            else "Not covered; --skip-controls delegates control to the deterministic browser fixture",
            "rust_graph": "Op-only; no Rust model requests", **final, **native}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=ROOT)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--python", type=Path)
    parser.add_argument("--timeout", type=float, default=300, help="Per-turn and Run boundary timeout in seconds")
    parser.add_argument("--skip-controls", action="store_true", help="Verify the core chain without Pilot pause/resume")
    args = parser.parse_args()
    repo = args.repo.expanduser().resolve()
    # Keep the venv interpreter path instead of resolving it to the system Python.
    python = (args.python or repo / ".venv/bin/python").expanduser().absolute()
    binary = (args.binary or repo / "rust/target/release/anchor-runner-host").expanduser().resolve()
    sys.path.insert(0, str(repo / "src"))
    from anchor.runtime.secrets import load_dotenv

    load_dotenv(repo / ".env")
    if any(not os.environ.get(name) for name in MODEL_KEYS):
        parser.exit(2, "Missing ANCHOR_MODEL_API_KEY/URL/NAME; no model request made\n")
    if not binary.is_file() or not python.is_file() or not (repo / "src/anchor/serve.py").is_file():
        parser.exit(2, "Provide the repository, Python venv and built Rust host; no model request made\n")
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.exit(2, "--timeout must be positive and finite; no model request made\n")
    (repo / ".local").mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-pilot-", dir=repo / ".local"))
    print(f"Evidence directory: {proof}", flush=True)
    evidence = Evidence(proof, tuple(os.environ.get(name, "") for name in (
        "ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY",
    )))
    deployment = None
    try:
        deployment = Deployment(repo, python, binary, evidence)
        deployment.start()
        deployment.api.expect("POST", "/sessions", {"id": SESSION}, status=201)
        result = exercise(Pilot(deployment, args.timeout), not args.skip_controls, uuid4().hex)
        evidence.save("evidence.json", {"status": "passed", "provider": "real configured Pilot model", **result})
        print(evidence.redact(json.dumps({"status": "passed", "run": result["run"], "pilot_turns": result["pilot_turns"],
                                         "real_pilot_pause_resume": result["real_pilot_pause_resume"],
                                         "evidence": str(proof / "evidence.json")}, indent=2)))
        return 0
    except (SmokeFailure, OSError, ValueError, KeyError, IndexError, TypeError, sqlite3.Error) as error:
        failure = {"status": "failed", "error_kind": type(error).__name__,
                   "check": str(error) if isinstance(error, SmokeFailure) else "Inspect retained service, SSE and native Harness records"}
        evidence.save("evidence.json", failure)
        print(evidence.redact(json.dumps({**failure, "evidence": str(proof / "evidence.json")}, indent=2)))
        return 1
    finally:
        if deployment is not None:
            deployment.stop()


if __name__ == "__main__":
    raise SystemExit(main())
