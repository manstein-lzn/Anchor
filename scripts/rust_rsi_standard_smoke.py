"""Run the unchanged standard 18-node RSI Graph through the Rust Host.

The local provider deliberately emits deterministic artifact-writing tool calls.
This validates Rust graph execution, parallel joins, native CommitRef review
binding and publication without making model or external publication requests.
It is not a research-quality or real-provider acceptance.
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
    proof = Path(tempfile.mkdtemp(prefix="rust-rsi-standard-controlled-", dir=ROOT / ".local"))
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
    request = json.dumps({"op": "start_bundle", "version": 1, "request_id": "rsi-standard-controlled",
                          "run_id": "rsi-standard-controlled", "input": {}}).encode()
    evidence = {"status": "failed", "graph_sha256": hashlib.sha256(source_graph.read_bytes()).hexdigest(),
                "provider": "configured real provider" if args.real else "controlled local fixture",
                "graph_unchanged": True}
    try:
        result = subprocess.run([str(args.binary.resolve())], input=struct.pack(">I", len(request)) + request,
                                env=env, cwd=proof, capture_output=True, timeout=args.timeout)
        (proof / "host.stderr").write_bytes(result.stderr)
        result.check_returncode()
        size = struct.unpack(">I", result.stdout[:4])[0]
        response = json.loads(result.stdout[4:4 + size])
        (proof / "response.json").write_text(json.dumps(response, indent=2, ensure_ascii=False))
        record = json.loads((state / "runs/rsi-standard-controlled.json").read_text())
        evidence.update(status=response.get("status"), response_status=response.get("status"),
                        run_status=record.get("status"), runs=record.get("results"),
                        provider_requests=len(FixtureProvider.calls) if not args.real else None,
                        tool_calls=dict(Counter(c["tool"] for c in FixtureProvider.calls)) if not args.real else None,
                        provider_failures=FixtureProvider.failures if not args.real else [])
        if response.get("status") != "completed":
            raise RuntimeError(f"controlled RSI did not complete; inspect {proof / 'response.json'}")
        publish = state / "artifacts" / record["results"]["publish"][0]["commit"]["id"] / "files"
        for name in ("rsi-report.md", "evolution.json", "sources.md", "review.json", "gate.txt", "audit-manifest.json", "review-manifest.json"):
            if not (publish / name).is_file():
                raise RuntimeError(f"publish artifact missing: {name}")
        evidence["status"] = "passed"
    except subprocess.TimeoutExpired as error:
        evidence.update(status="timeout", error=f"provider/runtime timeout after {args.timeout}s: {error}")
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
        evidence["provider_calls"] = FixtureProvider.calls
        evidence["provider_failures"] = FixtureProvider.failures
        (proof / "evidence.json").write_text(json.dumps(evidence, indent=2, ensure_ascii=False))
        print(json.dumps({"evidence": str(proof / "evidence.json"), **evidence}, ensure_ascii=False))


if __name__ == "__main__":
    main()
