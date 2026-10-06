"""Run the unchanged weekly-work-report Graph through the Rust Host safely.

The acceptance bundle keeps the production Graph bytes unchanged, but replaces
the ``docmost`` Plugin resource with a local stdio MCP gate.  The gate rejects
all page writes, so a successful run proves the report/feedback/artifact path
without changing a Docmost page or sending a channel message.

``--mode fixture`` uses a deterministic provider and is always available.
``--mode real`` uses the configured OpenAI-compatible provider and fails before
starting the host when provider credentials are absent.  Evidence is written
under a disposable ``.local/rust-weekly-acceptance-*`` directory.
"""

from __future__ import annotations

import argparse
from collections import Counter
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import socket
import tempfile
import threading
import time
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, build_opener, ProxyHandler

from anchor.runtime.secrets import load_dotenv


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "examples/graphs/weekly-work-report.json"
COLLECTOR = ROOT / "scripts/weekly_work_report"
MODEL_KEYS = ("ANCHOR_MODEL_URL", "ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_NAME")
EXPECTED_NODES = ["collect", "understand", "write", "review", "gate", "publish", "docmost"]


def _json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def _tool_response(request: dict[str, Any], name: str, arguments: dict[str, Any]) -> bytes:
    return (_json({
        "id": f"weekly-{request.get('id', 1)}",
        "object": "chat.completion",
        "created": 1,
        "model": request.get("model", "fixture"),
        "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
            "role": "assistant", "content": None,
            "tool_calls": [{"id": f"call-{request.get('id', 1)}", "type": "function",
                             "function": {"name": name, "arguments": _json(arguments)}}],
        }}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    }) + "\n").encode()


class FixtureProvider(BaseHTTPRequestHandler):
    calls: list[dict[str, Any]] = []
    failures: list[str] = []

    def log_message(self, *_args: object) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler protocol
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            messages = request.get("messages", [])
            serialized = json.dumps(messages, ensure_ascii=False)
            tools = [item.get("function", {}) for item in request.get("tools", [])]
            tool_names = [item.get("name") for item in tools]
            if any(name and name.startswith("docmost-") for name in tool_names):
                target = next(name for name in tool_names if name.startswith("docmost-"))
                previous = any(message.get("role") == "tool" for message in messages)
                if previous:
                    name, arguments = "final_result", {
                        "summary": "安全门禁拒绝了 Docmost 写入；报告仅保留在隔离 Artifact。",
                        "route": None,
                    }
                else:
                    name, arguments = target, {"safe_gate": True}
            else:
                previous = any(message.get("role") == "tool" for message in messages)
                if "你负责理解项目并作出编辑取舍" in serialized:
                    command = (
                        "set -eu; cat /in/collect/evidence/index.json > evidence-seen.json; "
                        "if test -f analysis.md; then printf '%s\\n' 'fixture analysis revised from gate feedback' > analysis.md; "
                        "else printf '%s\\n' 'fixture analysis with project meaning and bounded evidence' > analysis.md; fi; "
                        "printf '%s\\n' 'fixture source evidence' > sources.md"
                    )
                elif "你为负责人和协作同事撰写" in serialized:
                    command = (
                        "set -eu; mkdir -p assets; "
                        "if test -f report.md; then printf '%s\\n' '# 隔离周报（修订稿）' > report.md; "
                        "else printf '%s\\n' '# 隔离周报' > report.md; fi; "
                        "printf '%s\\n' '' '本周完成 Rust 周报执行路径验证，结果保留在隔离运行目录。' >> report.md; "
                        "printf '%s\\n' '![安全边界](assets/boundary.svg)' >> report.md; "
                        "printf '%s\\n' 'fixture sources' > sources.md; "
                        "printf '%s\\n' '<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"240\" height=\"80\"><text x=\"8\" y=\"42\">isolated safe gate</text></svg>' > assets/boundary.svg"
                    )
                elif "你是面向业务负责人的周报独立评审者" in serialized:
                    command = (
                        "set -eu; head=$(git --git-dir=/in/write/.git rev-parse HEAD); "
                        "if test -f review.json; then "
                        "printf '%s' '{\"decision\":\"publish\",\"reviewed_commit\":\"'\"$head\"'\",\"summary\":\"修订稿通过，安全门禁验收\",\"checks\":{\"facts\":true,\"business\":true,\"reasoning\":true,\"writing\":true},\"issues\":[{\"id\":\"R1\",\"location\":\"首轮稿件\",\"evidence\":\"fixture evidence\",\"impact\":\"需要修订后再发布\",\"required_change\":\"保留隔离边界\",\"acceptance\":\"修订稿明确安全边界\",\"status\":\"resolved\",\"resolution\":\"修订稿已明确安全边界\"}]}' > review.json; "
                        "else printf '%s' '{\"decision\":\"understand\",\"reviewed_commit\":\"'\"$head\"'\",\"summary\":\"首轮稿件需要明确安全边界\",\"checks\":{\"facts\":false,\"business\":true,\"reasoning\":true,\"writing\":true},\"issues\":[{\"id\":\"R1\",\"location\":\"首轮稿件\",\"evidence\":\"fixture evidence\",\"impact\":\"需要修订后再发布\",\"required_change\":\"保留隔离边界\",\"acceptance\":\"修订稿明确安全边界\",\"status\":\"open\"}]}' > review.json; fi; "
                        "printf '%s\\n' 'fixture review' > review.md"
                    )
                else:
                    raise AssertionError("fixture could not identify AgentNode")
                name, arguments = ("final_result", {"summary": "fixture completed", "route": None}) if previous else (
                    "anchor_run", {"command": ["sh", "-c", command]})
            self.calls.append({"tool": name, "model": request.get("model")})
            payload = _tool_response(request, name, arguments)
            self.send_response(200)
        except Exception as error:  # provider errors are retained as evidence
            self.failures.append(f"{type(error).__name__}: {error}")
            payload = _json({"error": {"message": str(error), "type": "fixture_error"}}).encode()
            self.send_response(400)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


SAFE_GATE_SERVER = r'''import json, sys
TOOLS = [
    ("list_spaces", "List spaces (safe gate: no production access)."),
    ("list_pages", "List pages (safe gate: no production access)."),
    ("get_page", "Read a page (safe gate: no production access)."),
    ("create_page", "Create page; always rejected by the acceptance gate."),
    ("update_page", "Update page; always rejected by the acceptance gate."),
    ("attachments_upload_page_image", "Upload image; always rejected by the acceptance gate."),
]
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {"protocolVersion":"2024-11-05", "capabilities":{"tools":{}}, "serverInfo":{"name":"weekly-safe-gate","version":"1"}}
    elif method == "tools/list":
        result = {"tools":[{"name":name,"description":description,"inputSchema":{"type":"object","additionalProperties":True}} for name, description in TOOLS]}
    elif method == "tools/call":
        result = {"isError":True,"content":[{"type":"text","text":"SAFE_GATE_REJECTED: no Docmost page or attachment write is permitted"}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc":"2.0","id":request["id"],"result":result}), flush=True)
'''


def make_bundle(proof: Path) -> tuple[Path, str]:
    bundle = proof / "bundle"
    plugin = bundle / "plugins/docmost"
    (plugin / "skills/docmost").mkdir(parents=True)
    shutil.copyfile(SOURCE, bundle / "graph.json")
    (plugin / "plugin.json").write_text(_json({
        "name": "Docmost safe acceptance gate",
        "description": "Local reject-only substitute; never connects to Docmost.",
        "skills": "skills/",
        "mcpServers": {"docmost": {"command": "python3", "args": ["server.py"]}},
    }) + "\n")
    (plugin / "server.py").write_text(SAFE_GATE_SERVER)
    (plugin / "skills/docmost/SKILL.md").write_text("Safe reject-only acceptance fixture.\n")
    resources = sorted(path.relative_to(plugin).as_posix() for path in plugin.rglob("*") if path.is_file())
    digest = hashlib.sha256()
    for name in resources:
        digest.update(name.encode())
        digest.update(hashlib.sha256((plugin / name).read_bytes()).digest())
    (bundle / "manifest.json").write_text(_json({
        "format": 1, "graph": "graph.json",
        "plugins": [{"id": "docmost", "digest": digest.hexdigest(), "resources": resources, "mcp_servers": ["docmost"]}],
    }) + "\n")
    return bundle, hashlib.sha256(SOURCE.read_bytes()).hexdigest()


def make_inputs(proof: Path) -> Path:
    root = proof / "operator-workspaces/weekly-work-report"
    for name in ("codex", "deepseek"):
        source = proof / name
        source.mkdir(parents=True)
        (source / "session.jsonl").write_text(_json({"type": "session", "id": name, "cwd": "/isolated"}) + "\n")
        (source / "session.jsonl").write_text(
            _json({"type": "session", "id": name, "cwd": "/isolated"}) + "\n" +
            _json({"type": "user/message", "time": 1790800000000, "data": {"message": {"content": [{"type": "text", "text": "isolated evidence"}]}}}) + "\n"
        )
    root.mkdir(parents=True)
    (root / "local-inputs.json").write_text(_json({
        "collect": {"codex": str(proof / "codex"), "deepseek": str(proof / "deepseek"), "collector": str(COLLECTOR)},
    }) + "\n")
    return root.parent


def _unused_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _api_json(opener, base: str, method: str, path: str, value: dict[str, Any] | None = None) -> tuple[int, dict[str, Any]]:
    request = Request(base + path, method=method, data=None if value is None else _json(value).encode(), headers={"Content-Type": "application/json"})
    try:
        with opener.open(request, timeout=10) as response:
            return response.status, json.loads(response.read())
    except HTTPError as error:
        with error:
            return error.code, json.loads(error.read())


def _run_real_service(binary: Path, env: dict[str, str], proof: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    """Use the persistent HTTP Host so provider completion is observed via API."""
    port = _unused_port()
    env = {**env, "ANCHOR_RUNNER_LISTEN": f"127.0.0.1:{port}", "ANCHOR_API_KEYS": ""}
    log = (proof / "host.log").open("wb")
    process = subprocess.Popen([str(binary.resolve()), "serve"], env=env, stdout=log, stderr=subprocess.STDOUT)
    opener = build_opener(ProxyHandler({}))
    base = f"http://127.0.0.1:{port}"
    try:
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError("Rust HTTP Host exited before readiness")
            try:
                status, _ = _api_json(opener, base, "GET", "/health")
                if status == 200:
                    break
            except (OSError, URLError):
                pass
            time.sleep(0.2)
        else:
            raise RuntimeError("Timed out waiting for Rust HTTP Host")
        status, accepted = _api_json(opener, base, "POST", "/trigger", {"graph": "weekly-work-report", "input": {}})
        if status != 202 or not accepted.get("run"):
            raise RuntimeError(f"Rust HTTP trigger rejected: {status} {accepted}")
        run_id = accepted["run"]
        while time.monotonic() < deadline + 900:
            status, detail = _api_json(opener, base, "GET", f"/runs/{run_id}")
            if status != 200:
                raise RuntimeError(f"Rust HTTP run query failed: {status} {detail}")
            state = detail.get("state", {}).get("status")
            if state == "completed":
                return {"status": "completed", "run_id": run_id}, detail
            if state in {"failed", "aborted", "stopped", "waiting_recovery"}:
                raise RuntimeError(f"Rust HTTP weekly run did not complete: {state}")
            time.sleep(1)
        raise RuntimeError("Timed out waiting for Rust HTTP weekly Run")
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
        log.close()


def run(binary: Path, mode: str) -> dict[str, Any]:
    load_dotenv(ROOT / ".env")
    if mode == "real" and any(not os.environ.get(key) for key in MODEL_KEYS):
        raise SystemExit("Missing model configuration; no provider request made")
    proof = Path(tempfile.mkdtemp(prefix="rust-weekly-acceptance-", dir=ROOT / ".local"))
    bundle, source_hash = make_bundle(proof)
    local_inputs = make_inputs(proof)
    # The production Op strings intentionally invoke ``python``.  The host's
    # base sandbox contains ``python3`` but not the /usr/local alias used by
    # this checkout, so expose that operator-installed interpreter through the
    # existing Library tool-environment contract.
    library = proof / "library/tools/python"
    library.mkdir(parents=True)
    (library / "tool.json").write_text(_json({
        "entrypoint": "/usr/local/bin/python", "environment": "/usr/local",
    }) + "\n")
    state = proof / "state"
    provider = None
    thread = None
    if mode == "fixture":
        provider = ThreadingHTTPServer(("127.0.0.1", 0), FixtureProvider)
        thread = threading.Thread(target=provider.serve_forever, daemon=True)
        thread.start()
    env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1"}
    if mode == "fixture":
        env.update({"ANCHOR_MODEL_API_KEY": "fixture-only", "ANCHOR_MODEL_URL": f"http://127.0.0.1:{provider.server_port}/v1", "ANCHOR_MODEL_NAME": "fixture-weekly", "ANCHOR_MODEL_WIRE_API": "chat"})
    else:
        env.update({key: os.environ[key] for key in MODEL_KEYS})
        env["ANCHOR_MODEL_WIRE_API"] = os.environ.get("ANCHOR_MODEL_WIRE_API", "responses")
    env.update({
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle), "ANCHOR_RUNNER_GRAPH_NAME": "weekly-work-report",
        "ANCHOR_RUNNER_LOCAL_INPUTS_ROOT": str(local_inputs), "ANCHOR_RUNNER_STATE_ROOT": str(state),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "workspaces"),
        "ANCHOR_RUNNER_LIBRARY_ROOT": str(proof / "library"),
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,cp,git,grep,head,mkdir,printf,python,python3,test",
    })
    # Explicitly prevent accidental reuse of production transport credentials.
    for key in ("DOCMOST_API_KEY", "ANCHOR_CHANNEL_CONTROL_DESCRIPTOR", "ANCHOR_WECOM_SEND_USERS", "ANCHOR_WECOM_USERS"):
        env.pop(key, None)
    if any(key in env for key in ("DOCMOST_API_KEY", "ANCHOR_CHANNEL_CONTROL_DESCRIPTOR", "ANCHOR_WECOM_SEND_USERS", "ANCHOR_WECOM_USERS")):
        raise RuntimeError("production channel or Docmost credentials leaked into acceptance environment")
    if any("docmost" in value.lower() and "cwise" in value.lower() for value in env.values()):
        raise RuntimeError("production Docmost endpoint leaked into acceptance environment")
    request = _json({"op": "start_bundle", "version": 1, "request_id": "weekly-acceptance", "run_id": "weekly-acceptance", "input": {}}).encode()
    evidence: dict[str, Any] = {"status": "failed", "mode": mode, "graph_sha256": source_hash, "external_publish": False}
    try:
        if mode == "real":
            response, detail = _run_real_service(binary, env, proof)
            (proof / "http-run.json").write_text(json.dumps(detail, indent=2, ensure_ascii=False))
        else:
            result = subprocess.run([str(binary.resolve())], input=struct.pack(">I", len(request)) + request, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=900, check=False)
            (proof / "host.stderr").write_bytes(result.stderr)
            result.check_returncode()
            response = json.loads(result.stdout[4:])
        (proof / "response.json").write_text(json.dumps(response, indent=2, ensure_ascii=False))
        if response.get("status") != "completed":
            raise RuntimeError(f"Rust weekly run did not complete: {response}")
        run_id = response.get("run_id", "weekly-acceptance")
        record = json.loads((state / "runs" / f"{run_id}.json").read_text())
        counts = {node: len(values) for node, values in record["results"].items()}
        if set(counts) != set(EXPECTED_NODES):
            raise RuntimeError(f"weekly graph did not execute every node: {counts}")
        if mode == "fixture" and (counts["understand"] < 2 or counts["write"] < 2 or counts["review"] < 2):
            raise RuntimeError(f"weekly feedback path was not exercised by the fixture: {counts}")
        for node in EXPECTED_NODES:
            for item in record["results"].get(node, []):
                files = state / "artifacts" / item["commit"]["id"] / "files"
                if not files.is_dir():
                    raise RuntimeError(f"missing immutable Artifact for {node}")
        if (bundle / "graph.json").read_bytes() != SOURCE.read_bytes():
            raise RuntimeError("production Graph bytes changed in the acceptance bundle")
        docmost_files = []
        for item in record["results"].get("docmost", []):
            docmost_files.extend(path.name for path in (state / "artifacts" / item["commit"]["id"] / "files").rglob("*"))
        if "docmost.json" in docmost_files:
            raise RuntimeError("safe gate run produced Docmost metadata")
        evidence.update(status="passed", node_passes=counts, artifact_commits=sum(counts.values()), source_graph_unchanged=True, safe_gate="stdio reject-only MCP; no production endpoint or channel credentials")
        (proof / "run.json").write_text(json.dumps(record, indent=2, ensure_ascii=False))
        return {"evidence": str(proof / "evidence.json"), **evidence}
    finally:
        if provider is not None:
            provider.shutdown(); provider.server_close(); thread.join(timeout=5)
            evidence["provider_requests"] = len(FixtureProvider.calls)
            evidence["provider_tools"] = dict(Counter(item["tool"] for item in FixtureProvider.calls))
            evidence["provider_failures"] = FixtureProvider.failures
        (proof / "evidence.json").write_text(json.dumps(evidence, indent=2, ensure_ascii=False))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/release/anchor-runner-host")
    parser.add_argument("--mode", choices=("fixture", "real"), default="fixture")
    args = parser.parse_args()
    result = run(args.binary, args.mode)
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
