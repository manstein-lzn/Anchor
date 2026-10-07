"""Run small Runtime contract Graphs, with fixtures and/or a real model.

Only disposable test roots are used. No research, business services, publishing,
production backend changes, or alternative Agent loop are involved.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
from urllib.parse import quote

from rust_platform_plugin_smoke import (
    Api, Evidence, MODEL_KEYS, Service, SmokeFailure, read_json, require, unused_ports, wait_until,
)


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests/fixtures/runtime_regression"
CASES = ("serial", "feedback", "parallel")
MODEL_OPTIONS = (
    "ANCHOR_MODEL_WIRE_API", "ANCHOR_MODEL_CONTEXT_WINDOW", "SSL_CERT_FILE", "SSL_CERT_DIR",
    "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
)


def file_sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def invocation_digest(result: dict) -> str:
    key = result["key"]
    durable = ":".join(str(key[name]) for name in ("run_id", "graph_digest", "node_id", "invocation"))
    return hashlib.sha256(durable.encode()).hexdigest()


def collect_usage(state: Path, pattern: str = "io-harness/store/*.recordings/*") -> dict:
    attempts = []
    for directory in sorted(state.glob(pattern)):
        if not directory.is_dir():
            continue
        outcome_path = directory / "outcome.json"
        outcome = read_json(outcome_path) if outcome_path.exists() else {"status": "incomplete"}
        recording_path = directory / "recording.json"
        exchanges = read_json(recording_path)["exchanges"] if recording_path.exists() else []
        require(len(exchanges) <= 1, "Unexpected provider recording exchange count")
        response = exchanges[0]["response"] if exchanges else {}
        attempts.append({
            "recording": str(directory.relative_to(state)), "status": outcome["status"],
            "usage": response.get("usage"),
        })
    reported = [attempt["usage"] for attempt in attempts if attempt["usage"] is not None]
    return {
        "provider_attempts": len(attempts), "usage_reported_attempts": len(reported),
        "usage_complete": bool(attempts) and len(reported) == len(attempts),
        "reported_tokens": {
            name: sum(usage[name] for usage in reported) if reported else None
            for name in ("prompt_tokens", "completion_tokens", "total_tokens")
        },
        "attempts": attempts,
    }


def prepare_bundle(case: str, root: Path) -> tuple[dict, str]:
    bundle = root / "bundle"
    bundle.mkdir()
    shutil.copyfile(FIXTURES / f"{case}.json", bundle / "graph.json")
    graph = read_json(bundle / "graph.json")
    bindings = []
    if case == "serial":
        plugin = bundle / "plugins/regression"
        shutil.copytree(FIXTURES / "plugins/regression", plugin)
        resources = sorted(path.relative_to(plugin).as_posix() for path in plugin.rglob("*") if path.is_file())
        digest = hashlib.sha256()
        for resource in resources:
            digest.update(resource.encode())
            digest.update(hashlib.sha256((plugin / resource).read_bytes()).digest())
        bindings.append({"id": "regression", "digest": digest.hexdigest(), "resources": resources,
                         "mcp_servers": []})
    (bundle / "manifest.json").write_text(json.dumps({"format": 1, "graph": "graph.json", "plugins": bindings}))
    return graph, file_sha256(bundle / "graph.json")


def host_environment(root: Path, case: str, port: int) -> dict[str, str]:
    env = {name: os.environ[name] for name in MODEL_KEYS}
    env.update({name: os.environ[name] for name in MODEL_OPTIONS if os.environ.get(name)})
    env.update({
        "PATH": "/usr/bin:/bin", "TZ": "UTC",
        "ANCHOR_MODEL_WIRE_API": env.get("ANCHOR_MODEL_WIRE_API", "responses"),
        "ANCHOR_MODEL_ALIASES": json.dumps({"models.regression": os.environ["ANCHOR_MODEL_NAME"]}),
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(root / "bundle"),
        "ANCHOR_RUNNER_GRAPH_NAME": case,
        "ANCHOR_RUNNER_STATE_ROOT": str(root / "state"),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(root / "work"),
        "ANCHOR_RUNNER_LISTEN": f"127.0.0.1:{port}",
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,printf,cp,git,cmp",
    })
    return env


def wait_run(api: Api, service: Service, run: str, evidence: Evidence, timeout: float) -> dict:
    def completed():
        service.check()
        detail = api.expect("GET", f"/runs/{quote(run, safe='')}")
        evidence.save("run-detail.json", detail)
        status = detail["state"]["status"]
        require(status in {"ready", "running", "completed"}, f"Unexpected Run status: {status}")
        return detail if status == "completed" and not detail["active"] else None

    return wait_until(completed, timeout, "the small test Graph")


def check_file(root: Path, record: dict, node: str, name: str, expected: bytes,
               invocation: int = 1) -> dict:
    result = record["results"][node][invocation - 1]
    artifact = root / "state/artifacts" / result["commit"]["id"]
    manifest = read_json(artifact / "manifest.json")
    require(manifest["key"] == result["key"], f"Artifact invocation differs: {node}/{invocation}")
    require(manifest["completion"] == result["completion"], f"Artifact completion differs: {node}/{invocation}")
    path = artifact / "files" / name
    require(path.read_bytes() == expected, f"Artifact bytes differ: {node}/{invocation}/{name}")
    digest = hashlib.sha256(expected).hexdigest()
    require(manifest["files"][name]["sha256"] == digest, f"Artifact hash differs: {node}/{invocation}/{name}")
    require(manifest["files"][name]["bytes"] == len(expected), f"Artifact length differs: {node}/{invocation}/{name}")
    workspace = root / "work" / record["run_id"] / invocation_digest(result) / name
    require(workspace.read_bytes() == expected, f"Workspace bytes differ: {node}/{invocation}/{name}")
    return {"node": node, "invocation": invocation, "file": name, "bytes": len(expected), "sha256": digest,
            "artifact": str(path.relative_to(root)), "workspace": str(workspace.relative_to(root))}


def check_histories(root: Path, graph: dict, record: dict, detail: dict) -> list[dict]:
    checked = []
    for node in graph["nodes"]:
        if "agent" not in node:
            continue
        for result in record["results"][node["id"]]:
            invocation = result["key"]["invocation"]
            trace_key = json.dumps([node["id"], invocation], separators=(",", ":"))
            history = detail["traces"].get(trace_key, [])
            require(bool(history), f"Missing native history: {trace_key}")
            commands = [command for message in history for command in message.get("commands", [])]
            require(any(command.startswith("anchor_run ") for command in commands),
                    f"No real tool call: {trace_key}")
            require(any(message["role"] == "tool" and "exit_code" in message.get("text", "") for message in history),
                    f"No persisted tool result: {trace_key}")
            recordings = root / "state/io-harness/store" / f"np1-{invocation_digest(result)}.recordings"
            attempts = sorted(recordings.iterdir())
            require(bool(attempts), f"Missing provider recordings: {trace_key}")
            final = read_json(attempts[-1] / "recording.json")["exchanges"][0]["response"]["tool_calls"]
            require(len(final) == 1 and final[0]["name"] == "final_result", f"Non-native completion: {trace_key}")
            require(final[0]["arguments"]["summary"] == result["completion"]["submission"],
                    f"Completion summary differs: {trace_key}")
            require(final[0]["arguments"].get("route") == result["completion"]["route"],
                    f"Completion route differs: {trace_key}")
            checked.append({"node": node["id"], "invocation": invocation, "messages": len(history),
                            "anchor_run_calls": sum(command.startswith("anchor_run ") for command in commands),
                            "route": result["completion"]["route"]})
    return checked


def check_serial(root: Path, record: dict, detail: dict) -> list[dict]:
    require(record["invocations"] == {"producer": 1, "worker": 1, "verify": 1}, "Serial invocation counts differ")
    commands = json.dumps(detail["traces"])
    for required in ("/in/producer/source.txt", "/plugins/regression/skills/check/SKILL.md",
                     "/plugins/regression/resources/input.txt"):
        require(required in commands, f"Missing actual input/Plugin read: {required}")
    return [
        check_file(root, record, "producer", "source.txt", b"seed\n"),
        check_file(root, record, "worker", "report.txt", b"seed\nfixture-resource\n"),
        check_file(root, record, "worker", "effects.txt", b"once\n"),
        check_file(root, record, "verify", "verified.txt", b"seed\nfixture-resource\n"),
    ]


def check_feedback(root: Path, record: dict, _detail: dict) -> list[dict]:
    require(record["invocations"] == {"writer": 2, "reviewer": 2, "done": 1}, "Feedback did not execute exactly once")
    reviews = record["results"]["reviewer"]
    require([result["completion"]["route"] for result in reviews] == ["writer", "done"], "Feedback routes differ")
    writers = record["results"]["writer"]
    require(writers[0]["commit"]["id"] != writers[1]["commit"]["id"], "Feedback overwrote the first Artifact")
    checked = []
    for invocation in (1, 2):
        checked.extend([
            check_file(root, record, "writer", "draft.txt", f"draft-v{invocation}\n".encode(), invocation),
            check_file(root, record, "writer", "notes.txt", b"retained\n", invocation),
            check_file(root, record, "writer", "effects.txt",
                       b"writer-1\n" if invocation == 1 else b"writer-1\nwriter-2\n", invocation),
            check_file(root, record, "reviewer", "review.txt",
                       b"revise once\n" if invocation == 1 else b"approved\n", invocation),
            check_file(root, record, "reviewer", "effects.txt",
                       b"reviewer-1\n" if invocation == 1 else b"reviewer-1\nreviewer-2\n", invocation),
        ])
    checked.append(check_file(root, record, "done", "final.txt", b"draft-v2\n"))
    return checked


def check_parallel(root: Path, record: dict, _detail: dict) -> list[dict]:
    expected = {"seed": 1, "fork": 1, "left": 1, "right": 1, "join": 1, "verify": 1}
    require(record["invocations"] == expected, "Parallel invocation counts differ")
    checked = [check_file(root, record, "verify", "combined.txt", b"seedLseedR")]
    for node, expected_bytes in (("left", b"seedL"), ("right", b"seedR")):
        checked.extend([
            check_file(root, record, node, "result.txt", expected_bytes),
            check_file(root, record, node, "effects.txt", b"once\n"),
        ])
    artifact = root / "state/artifacts" / record["results"]["verify"][0]["commit"]["id"]
    joined = read_json(artifact / "files/join-copy.json")
    require(len(joined["branches"]) == 2, "Join did not include both branches")
    for branch in joined["branches"]:
        for node in branch["nodes"]:
            require(node["commit"] == record["results"][node["node"]][0]["commit"], "Join input lineage differs")
    return checked


CHECKS = {"serial": check_serial, "feedback": check_feedback, "parallel": check_parallel}


def run_live_case(case: str, binary: Path, root: Path, secrets: tuple[str, ...], timeout: float) -> dict:
    root.mkdir()
    evidence = Evidence(root, secrets)
    report = {"case": case, "status": "failed", "provider": "real configured model"}
    started = time.monotonic()
    service = None
    try:
        graph, digest = prepare_bundle(case, root)
        report.update(graph_sha256=digest, manifest_sha256=file_sha256(root / "bundle/manifest.json"))
        port, _unused = unused_ports()
        api = Api(port)
        service = Service("host", [str(binary), "serve"], host_environment(root, case, port), evidence)
        wait_until(lambda: api.ready(service, "/health"), 30, "isolated Rust Host")
        status, raw = api.raw("POST", "/trigger", {"graph": case})
        accepted = json.loads(raw)
        evidence.save("trigger-response.json", accepted)
        require(status == 202, f"Graph admission failed: HTTP {status}; inspect trigger-response.json")
        report["run"] = run = accepted["run"]
        detail = wait_run(api, service, run, evidence, timeout)
        record = read_json(root / "state/runs" / f"{run}.json")
        evidence.save("run-record.json", record)
        require(record["status"] == "completed", "Run is not durably completed")
        report["files"] = CHECKS[case](root, record, detail)
        report["histories"] = check_histories(root, graph, record, detail)
        require(file_sha256(root / "bundle/graph.json") == digest, "The executed Graph changed")
        report.update(status="passed", invocations=record["invocations"])
    except (SmokeFailure, OSError, ValueError, KeyError, TypeError, IndexError) as error:
        report["failure"] = evidence.redact(f"{type(error).__name__}: {error}")
        raise
    finally:
        if service is not None:
            service.stop()
        report.update(elapsed_seconds=round(time.monotonic() - started, 3), **collect_usage(root / "state"))
        evidence.save("evidence.json", report)
    return report


def run_fixture(root: Path, target: Path) -> dict:
    evidence_root = root / "fixture"
    evidence_root.mkdir()
    env = dict(os.environ, CARGO_TARGET_DIR=str(target), ANCHOR_TEST_EVIDENCE_ROOT=str(evidence_root))
    build_command = ["cargo", "build", "--manifest-path", str(ROOT / "rust/Cargo.toml"),
                     "-p", "anchor-wecom-tools", "-p", "anchor-docmost-tools", "--bins"]
    command = ["cargo", "test", "--manifest-path", str(ROOT / "rust/Cargo.toml"), "-p", "anchor-runner-host",
               "--features", "legacy-regression",
               "--test", "runtime_contract", "--test", "native_plugins", "--", "--test-threads=4", "--nocapture"]
    started = time.monotonic()
    with (root / "fixture.log").open("wb") as log:
        built = subprocess.run(build_command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT, check=False)
        result = subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT, check=False) \
            if built.returncode == 0 else built
    reports = [read_json(path) for path in sorted(evidence_root.glob("*.json"))]
    return {"status": "passed" if result.returncode == 0 and reports
            and all(report["status"] == "passed" for report in reports) else "failed",
            "command": command, "build_command": build_command,
            "build_exit_code": built.returncode, "exit_code": result.returncode, "scenario_evidence": len(reports),
            "elapsed_seconds": round(time.monotonic() - started, 3), "real_model_requests": 0}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("fixture", "live", "all"), default="all")
    parser.add_argument("--case", choices=CASES, action="append", help="Select live cases; default: all three")
    parser.add_argument("--target-dir", type=Path,
                        default=Path(os.environ.get("CARGO_TARGET_DIR", "/tmp/anchor-runtime-contract-target")))
    parser.add_argument("--binary", type=Path, help="Already built Host; default: <target-dir>/debug/anchor-runner-host")
    parser.add_argument("--evidence-root", type=Path, help="Parent for a new evidence directory; default: system temp")
    parser.add_argument("--timeout", type=float, default=180, help="Maximum seconds per live Graph, not a token limit")
    args = parser.parse_args()
    if args.timeout <= 0 or (args.mode == "fixture" and args.case):
        parser.error("--timeout must be positive; --case requires live or all mode")
    if args.mode != "fixture":
        from anchor.runtime.secrets import load_dotenv

        load_dotenv(ROOT / ".env")
        if any(not os.environ.get(name) for name in MODEL_KEYS):
            parser.exit(2, "Missing ANCHOR_MODEL_API_KEY/URL/NAME; no model request made; live regression NOT passed\n")
    parent = args.evidence_root.expanduser().resolve() if args.evidence_root else None
    if parent:
        parent.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="anchor-low-cost-", dir=parent))
    root.chmod(0o700)
    secrets = tuple(os.environ.get(name, "") for name in ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", *MODEL_OPTIONS)
                    if name != "ANCHOR_MODEL_WIRE_API" and name != "ANCHOR_MODEL_CONTEXT_WINDOW")
    evidence = Evidence(root, secrets)
    report = {"status": "failed", "mode": args.mode, "live_cases": [],
              "scope": "small Runtime Graph regression; not all product features or business-content acceptance"}
    started = time.monotonic()
    print(f"Evidence directory: {root}", flush=True)
    try:
        target = args.target_dir.expanduser().resolve()
        if args.mode != "live":
            report["fixture"] = run_fixture(root, target)
            require(report["fixture"]["status"] == "passed", "Fixture regression failed; inspect fixture.log")
        if args.mode != "fixture":
            source = (args.binary or target / "debug/anchor-runner-host").expanduser().resolve()
            binary = root / "anchor-runner-host"
            shutil.copy2(source, binary)
            report["host_sha256"] = file_sha256(binary)
            report["wire_api"] = os.environ.get("ANCHOR_MODEL_WIRE_API", "responses")
            for case in dict.fromkeys(args.case or CASES):
                result = run_live_case(case, binary, root / case, secrets, args.timeout)
                report["live_cases"].append(result)
                print(f"{case}: passed; attempts={result['provider_attempts']}; "
                      f"reported_tokens={result['reported_tokens']['total_tokens']}; "
                      f"seconds={result['elapsed_seconds']}", flush=True)
        report["status"] = "passed"
    except (SmokeFailure, OSError, ValueError, KeyError, TypeError, IndexError) as error:
        report["failure"] = evidence.redact(f"{type(error).__name__}: {error}")
    finally:
        live = [read_json(path) for path in sorted(root.glob("*/evidence.json"))]
        report["live_cases"] = live
        report["provider_attempts"] = sum(case["provider_attempts"] for case in live)
        reported = [case["reported_tokens"]["total_tokens"] for case in live
                    if case["reported_tokens"]["total_tokens"] is not None]
        report["reported_total_tokens"] = sum(reported) if reported else None
        report["usage_complete"] = bool(live) and all(case["usage_complete"] for case in live)
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
        evidence.save("evidence.json", report)
        print(json.dumps({"status": report["status"], "evidence": str(root / "evidence.json"),
                          "provider_attempts": report["provider_attempts"],
                          "reported_total_tokens": report["reported_total_tokens"],
                          "failure": report.get("failure")}, ensure_ascii=False), flush=True)
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
