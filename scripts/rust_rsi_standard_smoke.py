"""Run the unchanged standard 18-node RSI Graph through the Rust Host.

The default local provider emits deterministic artifact-writing tool calls;
``--real`` uses the configured provider in the same isolated bundle.  Both
modes retain durable Run, Harness records and partial Artifacts on failure.
Successful mechanical closure alone is not research-content acceptance.
"""

from __future__ import annotations

import argparse
import base64
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import threading
import shlex
import sqlite3

from anchor.runtime.secrets import load_dotenv


ROOT = Path(__file__).resolve().parents[1]
DOMAINS = {
    "run-audit": "runs",
    "code-audit": "code",
    "graph-audit": "graphs",
    "plugin-audit": "plugins",
    "dependency-audit": "dependencies",
}


def _write_script(path: str, content: str) -> str:
    encoded = base64.b64encode(content.encode()).decode()
    return f"printf %s {encoded} | base64 -d > {path}"


def _agent_command(node: str) -> str:
    if node in DOMAINS:
        domain = DOMAINS[node]
        findings = json.dumps({
            "domain": domain,
            "summary": "controlled Rust standard RSI execution",
            "coverage": {
                "read": [{"path": f"/in/collect/evidence/domains/{domain}.json", "locator": "$"}],
                "not_reviewed": [], "limitations": ["controlled fixture"],
            },
            "findings": [], "follow_up": [],
        }, ensure_ascii=False)
        return "set -eu; " + _write_script("findings.json", findings) + "; printf '%s\\n' controlled > findings.md"
    if node == "analyst":
        return ("set -eu; python3 -c "
                "'import json; w=json.load(open(\"/in/collect/evidence/index.json\")); "
                "json.dump({\"window\":{\"start\":w[\"start\"],\"end_exclusive\":w[\"end_exclusive\"]},"
                "\"proposals\":[],\"carry_forward\":[]},open(\"evolution.json\",\"w\"))' ; "
                + _write_script("rsi-report.md", "# Controlled Rust standard RSI\n") + "; "
                + _write_script("sources.md", "Controlled local evidence only.\n"))
    if node in {"fact-review", "proposal-review"}:
        checks = {"facts": True, "research": True, "architecture": True, "writing": True}
        if node == "fact-review":
            checks = {"facts": True, "research": True}
        else:
            checks = {"architecture": True, "writing": True}
        review = {"decision": "publish", "reviewed_commit": "", "checks": checks,
                  "issues": [], "summary": "controlled"}
        code = ("import json,sys; d=" + repr(review) + "; d['reviewed_commit']=sys.argv[1]; "
                "json.dump(d,open('review.json','w'))")
        return ("set -eu; head=$(git --git-dir=/in/analyze/.git rev-parse HEAD); "
                f"python3 -c {shlex.quote(code)} \"$head\"; "
                "printf '%s\\n' publish > review.md")
    raise ValueError(f"unknown agent {node}")


def _runtime_trace(state: Path) -> dict:
    """Read the io-harness records without inventing a second event format."""
    trace = {"stores": [], "provider_calls": [], "tool_calls": [], "observations": [], "errors": []}
    for store in sorted((state / "io-harness/store").glob("*.sqlite3")):
        trace["stores"].append(str(store))
        try:
            with sqlite3.connect(f"file:{store}?mode=ro", uri=True) as db:
                trace["provider_calls"].extend(
                    {
                        "store": str(store), "id": row[0], "step": row[1],
                        "attempt": row[2], "model": row[3],
                        "finish_reason": row[4], "failure": row[5],
                    }
                    for row in db.execute(
                        "SELECT id, step, attempt, model, finish_reason, failure "
                        "FROM provider_calls ORDER BY id"
                    )
                )
                for step, serialized in db.execute("SELECT step, calls FROM step_turns ORDER BY step"):
                    for call in json.loads(serialized):
                        if call.get("name") in {"anchor_run", "final_result"}:
                            trace["tool_calls"].append({"store": str(store), "step": step, **call})
                for step, text in db.execute(
                    "SELECT step, text FROM ledger_observations "
                    "WHERE target='anchor_run' ORDER BY id"
                ):
                    trace["observations"].append({"store": str(store), "step": step, "text": text})
        except (sqlite3.Error, json.JSONDecodeError) as error:
            # A killed process may leave a store before its final schema write;
            # retain the store path and report the partial trace explicitly.
            trace["errors"].append({"store": str(store), "error": str(error)})
    return trace


def _collect_runtime(proof: Path, state: Path, run_name: str, evidence: dict) -> None:
    run_path = state / "runs" / f"{run_name}.json"
    record = {}
    if run_path.is_file():
        try:
            record = json.loads(run_path.read_text())
        except json.JSONDecodeError:
            evidence["run_record_error"] = "durable Run record is not valid JSON"
    trace = _runtime_trace(state)
    results = record.get("results", {})
    evidence.update({
        "run_status": record.get("status"),
        "run_record": str(run_path.relative_to(proof)) if run_path.exists() else None,
        "runs": results,
        "node_invocations": record.get("invocations", {}),
        "committed_node_invocations": {node: len(items) for node, items in results.items()},
        "parallel": record.get("parallel"),
        "runtime_error": record.get("error"),
        "provider_trace": trace,
        "provider_requests": len(trace["provider_calls"]),
        "provider_calls": trace["provider_calls"],
        "provider_failures": [call for call in trace["provider_calls"] if call["failure"]],
        "provider_model_observed": sorted({call["model"] for call in trace["provider_calls"] if call["model"]}),
        "tool_call_counts": dict(Counter(call["name"] for call in trace["tool_calls"])),
        "artifact_count": len({item["commit"]["id"] for items in results.values() for item in items}),
        "artifacts": [str(path.relative_to(proof)) for path in sorted((state / "artifacts").rglob("*"))
                      if path.is_file() and "/files/" in path.as_posix()],
        "recordings": [str(path.relative_to(proof)) for path in sorted(
            state.glob("io-harness/store/*.recordings/*/recording.json"))],
        "uncommitted_files": [str(path.relative_to(proof)) for path in sorted(
            (proof / "workspaces").rglob("*")) if path.is_file()],
        "research_quality": "not accepted by this mechanical collector",
        "runtime_closure": "passed" if evidence["status"] == "passed" else "not completed",
    })
    if results:
        evidence["branch_nodes"] = sorted(results)


class FixtureProvider(BaseHTTPRequestHandler):
    calls: list[dict] = []
    failures: list[str] = []

    def log_message(self, *_args: object) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802 - stdlib handler hook
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            serialized = json.dumps(request.get("messages", []), ensure_ascii=False)
            if "本领域 domain 固定为 runs" in serialized:
                node = "run-audit"
            elif "本领域 domain 固定为 code" in serialized:
                node = "code-audit"
            elif "本领域 domain 固定为 graphs" in serialized:
                node = "graph-audit"
            elif "本领域 domain 固定为 plugins" in serialized:
                node = "plugin-audit"
            elif "本领域 domain 固定为 dependencies" in serialized:
                node = "dependency-audit"
            elif "综合分析者" in serialized:
                node = "analyst"
            elif "checks.facts" in serialized:
                node = "fact-review"
            elif "checks.architecture" in serialized:
                node = "proposal-review"
            else:
                raise AssertionError("fixture could not identify RSI agent")
            tool_results = [message for message in request.get("messages", []) if message.get("role") == "tool"]
            if tool_results:
                name, arguments = "final_result", {"summary": "controlled RSI artifact committed"}
            else:
                name, arguments = "anchor_run", {"command": ["sh", "-c", _agent_command(node)]}
            self.calls.append({"node": node, "tool": name, "model": request.get("model")})
            payload = {
                "id": f"controlled-rsi-{len(self.calls)}", "object": "chat.completion", "created": 1,
                "model": request.get("model", "fixture-rsi"),
                "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                    "role": "assistant", "content": None,
                    "tool_calls": [{"id": f"call-{len(self.calls)}", "type": "function", "function": {
                        "name": name, "arguments": json.dumps(arguments, ensure_ascii=False),
                    }}],
                }}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            }
            status = 200
        except (AssertionError, KeyError, ValueError, json.JSONDecodeError) as error:
            self.failures.append(str(error))
            payload = {"error": {"message": str(error), "type": "controlled_fixture"}}
            status = 400
        encoded = json.dumps(payload, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/debug/anchor-runner-host")
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--real", action="store_true", help="use configured real provider")
    args = parser.parse_args()
    if args.real:
        load_dotenv(ROOT / ".env")
        required = ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")
        missing = [name for name in required if not os.environ.get(name)]
        if missing:
            raise SystemExit(f"Missing configured provider variables: {', '.join(missing)}")
    mode = "real" if args.real else "controlled"
    run_name = f"rsi-standard-{mode}"
    proof = Path(tempfile.mkdtemp(prefix=f"rust-rsi-standard-{mode}-", dir=ROOT / ".local"))
    print(f"Evidence directory: {proof}", flush=True)
    bundle = proof / "bundle"
    bundle.mkdir()
    source_graph = ROOT / "examples/graphs/rsi.json"
    shutil.copyfile(source_graph, bundle / "graph.json")
    (bundle / "manifest.json").write_text(json.dumps({"format": 1, "graph": "graph.json", "plugins": []}))
    anchor = proof / "anchor"
    anchor.mkdir()
    grants_root = proof / "local-inputs" / "rsi"
    grants_root.mkdir(parents=True)
    grants = {
        node: {"anchor": str(anchor), "source": str(ROOT), "code": str(ROOT), "grants": str(grants_root / "local-inputs.json")}
        for node in ("collect", "research", "review", "gate")
    }
    (grants_root / "local-inputs.json").write_text(json.dumps(grants))
    state = proof / "state"
    server = None
    thread = None
    if not args.real:
        server = ThreadingHTTPServer(("127.0.0.1", 0), FixtureProvider)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
    env = {
        "PATH": "/usr/bin:/bin",
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle),
        "ANCHOR_RUNNER_GRAPH_NAME": "rsi",
        "ANCHOR_RUNNER_LOCAL_INPUTS_ROOT": str(grants_root.parent),
        "ANCHOR_RUNNER_STATE_ROOT": str(state),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "workspaces"),
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,cp,printf,base64,python3,git,jq,mkdir,find,sed,grep",
    }
    if args.real:
        env.update({key: value for key, value in os.environ.items() if key.startswith("ANCHOR_MODEL_")})
    else:
        env.update({
            "ANCHOR_MODEL_API_KEY": "controlled-rsi-fixture",
            "ANCHOR_MODEL_URL": f"http://127.0.0.1:{server.server_port}/v1",
            "ANCHOR_MODEL_NAME": "fixture-rsi",
            "ANCHOR_MODEL_WIRE_API": "chat",
        })
    request = json.dumps({"op": "start_bundle", "version": 1, "request_id": run_name,
                          "run_id": run_name, "input": {}}).encode()
    evidence = {"status": "failed", "graph_sha256": hashlib.sha256(source_graph.read_bytes()).hexdigest(),
                "provider": "configured real provider" if args.real else "controlled local fixture",
                "provider_mode": mode,
                "graph_unchanged": (bundle / "graph.json").read_bytes() == source_graph.read_bytes(),
                "proof": str(proof),
                "publishing": "disabled: no external publication or channel configured"}
    try:
        result = subprocess.run([str(args.binary.resolve())], input=struct.pack(">I", len(request)) + request,
                                env=env, cwd=proof, capture_output=True, timeout=args.timeout)
        (proof / "host.stdout").write_bytes(result.stdout)
        (proof / "host.stderr").write_bytes(result.stderr)
        evidence["host_exit_code"] = result.returncode
        result.check_returncode()
        size = struct.unpack(">I", result.stdout[:4])[0]
        response = json.loads(result.stdout[4:4 + size])
        (proof / "response.json").write_text(json.dumps(response, indent=2, ensure_ascii=False))
        record = json.loads((state / "runs" / f"{run_name}.json").read_text())
        evidence.update(status=response.get("status"), response_status=response.get("status"),
                        run_status=record.get("status"), runs=record.get("results"),
                        provider_requests=len(FixtureProvider.calls) if not args.real else None,
                        tool_calls=dict(Counter(c["tool"] for c in FixtureProvider.calls)) if not args.real else None,
                        provider_failures=FixtureProvider.failures if not args.real else [])
        if response.get("status") != "completed":
            raise RuntimeError(f"{mode} RSI did not complete; inspect {proof / 'response.json'}")
        publish = state / "artifacts" / record["results"]["publish"][0]["commit"]["id"] / "files"
        for name in ("rsi-report.md", "evolution.json", "sources.md", "review.json", "gate.txt", "audit-manifest.json", "review-manifest.json"):
            if not (publish / name).is_file():
                raise RuntimeError(f"publish artifact missing: {name}")
        evidence["status"] = "passed"
    except subprocess.TimeoutExpired as error:
        evidence.update(status="timeout", error=f"provider/runtime timeout after {args.timeout}s: {error}")
        (proof / "host.stdout").write_bytes(error.stdout or b"")
        (proof / "host.stderr").write_bytes(error.stderr or b"")
        raise
    except subprocess.CalledProcessError as error:
        evidence.update(status="failed", error=f"Rust Host exited with status {error.returncode}")
        raise
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        evidence.update(status="failed", error=str(error))
        raise
    finally:
        if server is not None:
            server.shutdown()
            server.server_close()
        if thread is not None:
            thread.join(timeout=5)
        evidence["fixture_provider_calls"] = FixtureProvider.calls
        evidence["fixture_provider_failures"] = FixtureProvider.failures
        _collect_runtime(proof, state, run_name, evidence)
        (proof / "evidence.json").write_text(json.dumps(evidence, indent=2, ensure_ascii=False))
        print(json.dumps({key: value for key, value in {"evidence": str(proof / "evidence.json"), **evidence}.items()
                          if key not in {"runs", "provider_trace", "provider_calls", "artifacts", "recordings",
                                         "uncommitted_files", "parallel"}}, ensure_ascii=False))


if __name__ == "__main__":
    main()
