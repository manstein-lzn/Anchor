"""Opt-in real HTTP/provider acceptance for Rust Run pause, restart and resume.

Python is an operator harness only. The API, Runner, MCP and Sandbox are Rust.
Uses disposable roots and never changes production Graphs or schedules.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import select
import shlex
import socket
import struct
import subprocess
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
import uuid

from anchor.runtime.secrets import load_dotenv

ROOT = Path(__file__).resolve().parents[1]


def until(check, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.1)
    raise TimeoutError("Acceptance observation deadline exceeded; inspect retained proof")


def terminate(process):
    if process and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


class Api:
    def __init__(self, env, proof):
        self.env = env
        self.proof = proof
        self.process = None
        self.generation = 0
        self.base = "http://" + env["ANCHOR_RUNNER_LISTEN"]

    def start(self):
        self.generation += 1
        with (self.proof / f"http-{self.generation}.log").open("wb") as log:
            self.process = subprocess.Popen(
                [str(ROOT / "rust/target/debug/anchor-runner-host"), "serve"],
                env=self.env, stdout=log, stderr=log,
            )
        until(self.ready)

    def ready(self):
        if self.process.poll() is not None:
            raise RuntimeError("Rust API exited; inspect HTTP log")
        try:
            return self.request("GET", "/health")[0] == 200
        except (URLError, ConnectionError):
            return False

    def request(self, method, path, body=None):
        request = Request(self.base + path, method=method,
                          data=None if body is None else json.dumps(body).encode(),
                          headers={"Content-Type": "application/json"})
        try:
            with urlopen(request, timeout=15) as response:
                raw = response.read()
                return response.status, json.loads(raw) if raw else None
        except HTTPError as error:
            return error.code, json.loads(error.read())

    def expect(self, method, path, body=None, status=200):
        actual, value = self.request(method, path, body)
        if actual != status:
            raise AssertionError(f"{method} {path}: expected {status}, got {actual}; {value}")
        return value

    def wait_status(self, run, wanted, seconds=30):
        def check():
            value = self.expect("GET", f"/runs/{run}")
            current = value["state"]["status"]
            if current == wanted:
                return value
            if current in {"failed", "completed"}:
                raise AssertionError(f"Unexpected terminal {current}, wanted {wanted}")
            return None
        return until(check, seconds)

    def close(self):
        terminate(self.process)


def saved(proof, run):
    return json.loads((proof / "state/runs" / f"{run}.json").read_text())


def framed(env, request):
    payload = json.dumps(request).encode()
    result = subprocess.run(
        [str(ROOT / "rust/target/debug/anchor-runner-host")],
        input=struct.pack(">I", len(payload)) + payload, env=env,
        capture_output=True, timeout=15, check=True,
    )
    size = struct.unpack(">I", result.stdout[:4])[0]
    return json.loads(result.stdout[4:4 + size])


def started_workspace(proof, run, node):
    record = saved(proof, run)
    key = (record.get("cursor") or {}).get("key")
    if not key or key["node_id"] != node:
        return None
    identity = ":".join(str(key[name]) for name in ("run_id", "graph_digest", "node_id", "invocation"))
    digest = hashlib.sha256(identity.encode()).hexdigest()
    marker = proof / "state/facts" / f"nf1-{digest}.started"
    workspace = proof / "work" / run / digest
    return workspace if marker.exists() and workspace.exists() else None


def main():
    load_dotenv(ROOT / ".env")
    keys = ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")
    if any(not os.environ.get(key) for key in keys):
        raise SystemExit("Missing model configuration; no requests made")
    (ROOT / ".local").mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-lifecycle-", dir=ROOT / ".local"))
    print(json.dumps({"proof": str(proof), "status": "running"}), flush=True)
    api = None
    fixture = subprocess.Popen(
        [str(ROOT / "rust/target/debug/examples/http_fixture"), str(proof / "mcp-call.json")],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        env={"PATH": os.environ.get("PATH", "/usr/bin:/bin")},
    )
    try:
        if not select.select([fixture.stdout], [], [], 15)[0]:
            raise RuntimeError("Rust MCP fixture did not become ready")
        endpoint = fixture.stdout.readline().strip()
        if not endpoint.startswith("http://127.0.0.1:"):
            raise RuntimeError("Unexpected fixture endpoint")
        bundle = proof / "catalog/lifecycle"
        plugin = bundle / "plugins/fixture"
        plugin.mkdir(parents=True)
        resource = b'{"name":"Local acceptance fixture","mcpServers":{"fixture":{}}}'
        (plugin / "plugin.json").write_bytes(resource)
        digest = hashlib.sha256(b"plugin.json" + hashlib.sha256(resource).digest()).hexdigest()
        token = "lifecycle-" + uuid.uuid4().hex
        command = lambda script: shlex.join(["sh", "-c", script])
        graph = {
            "objective": "Preserve the original Run across API pause and process restart.",
            "entry": "producer",
            "agents": {"worker": {
                "model": os.environ["ANCHOR_MODEL_NAME"], "network": True,
                "instructions": "Read /in/producer/source.txt using anchor_run. Search attached MCP tools for fixture_suffix, then call the exact returned tool using anchor_mcp__call_tool with its server_id, tool_name, and arguments={\"text\": <exact source text>}. Write only its returned text field to /workspace/report.txt. Read the file back, then finish with summary and route=verify. Do not guess or compute the suffix yourself.",
            }},
            "ops": {
                "producer": {"run": command(f"printf %s {shlex.quote(token)} > source.txt; sleep 2")},
                "verify": {"run": command(f'test "$(cat /in/worker/report.txt)" = {shlex.quote(token + "-checked")} && cp /in/worker/report.txt verified.txt')},
            },
            "nodes": [{"id": "producer", "op": "producer"}, {"id": "worker", "agent": "worker", "plugins": ["fixture"]}, {"id": "verify", "op": "verify"}],
            "edges": [{"from": "producer", "to": "worker"}, {"from": "worker", "to": "verify"}],
        }
        (bundle / "graph.json").write_text(json.dumps(graph))
        (bundle / "manifest.json").write_text(json.dumps({"format": 1, "graph": "graph.json", "plugins": [
            {"id": "fixture", "digest": digest, "resources": ["plugin.json"], "mcp_servers": ["fixture"]}
        ]}))
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            port = reserved.getsockname()[1]
        env = {key: os.environ[key] for key in keys}
        env.update({
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "ANCHOR_MODEL_WIRE_API": os.environ.get("ANCHOR_MODEL_WIRE_API", "chat"),
            "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle),
            "ANCHOR_RUNNER_CATALOG_ROOT": str(proof / "catalog"),
            "ANCHOR_RUNNER_GRAPH_NAME": "lifecycle",
            "ANCHOR_RUNNER_STATE_ROOT": str(proof / "state"),
            "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "work"),
            "ANCHOR_RUNNER_LISTEN": f"127.0.0.1:{port}",
            "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,cp,printf,test,sleep",
            "ANCHOR_RUST_MCP_SERVERS": json.dumps({"fixture": {"transport": "http", "endpoint": endpoint, "allowed_tools": ["fixture_suffix"]}}),
        })
        api = Api(env, proof)
        api.start()
        run = api.expect("POST", "/trigger", {"graph": "lifecycle"}, 202)["run"]
        until(lambda: started_workspace(proof, run, "producer"))
        api.expect("POST", f"/runs/{run}/pause", status=202)
        api.wait_status(run, "paused")
        before = saved(proof, run)
        assert list(before["results"]) == ["producer"]
        observed = framed(env, {"op": "status", "version": 1, "request_id": "read", "run_id": run})
        assert observed["status"] == "paused"
        blocked = framed(env, {
            "op": "start_or_resume", "version": 1, "request_id": "writer-conflict",
            "run_id": "writer-conflict", "input": {},
            "snapshot": {"objective": "Second writer must be denied", "entry": "work",
                         "ops": {"work": {"run": "sh -c 'printf duplicate'"}},
                         "nodes": [{"id": "work", "op": "work"}], "edges": []},
        })
        assert blocked["kind"] == "rejected", blocked
        assert not (proof / "state/runs/writer-conflict.json").exists()
        api.expect("POST", "/trigger", {"graph": "lifecycle"}, 409)
        api.close()
        graph["objective"] = "Edited after admission; must not replace old Run"
        graph["ops"]["producer"]["run"] = "sh -c 'exit 19'"
        (bundle / "graph.json").write_text(json.dumps(graph))
        api.start()
        assert api.expect("GET", f"/runs/{run}")["graph"] == "lifecycle"
        (plugin / "plugin.json").write_bytes(resource + b"\n")
        api.expect("POST", f"/runs/{run}/resume", status=422)
        assert saved(proof, run)["status"] == "paused"
        (plugin / "plugin.json").write_bytes(resource)
        api.expect("POST", f"/runs/{run}/resume", status=202)
        api.wait_status(run, "completed", seconds=240)
        after = saved(proof, run)
        assert after["graph_digest"] == before["graph_digest"]
        assert after["results"]["producer"] == before["results"]["producer"]
        final_commit = after["results"]["verify"][-1]["commit"]["id"]
        output = proof / "state/artifacts" / final_commit / "files/verified.txt"
        assert output.read_text().strip() == token + "-checked"
        assert json.loads((proof / "mcp-call.json").read_text())["input"] == token
        api.expect("POST", f"/runs/{run}/resume", status=409)
        print(json.dumps({"phase": "provider_pause_restart_resume", "status": "passed", "run": run}), flush=True)

        # Kill while a real sandbox command has started and written an effect.
        api.expect("POST", "/graphs", {"name": "interrupted"}, 201)
        interrupted = {"objective": "Unknown effect must not replay", "entry": "effect", "ops": {"effect": {"run": command("printf 'once\\n' >> effect.txt; sleep 30")}}, "nodes": [{"id": "effect", "op": "effect"}], "edges": []}
        api.expect("PUT", "/graphs/interrupted", {"definition": interrupted})
        killed_run = api.expect("POST", "/trigger", {"graph": "interrupted"}, 202)["run"]
        workspace = until(lambda: started_workspace(proof, killed_run, "effect"))
        until(lambda: (workspace / "effect.txt").exists())
        api.process.kill()
        api.process.wait()
        api.start()
        assert api.expect("GET", f"/runs/{killed_run}")["graph"] == "interrupted"
        api.expect("POST", f"/runs/{killed_run}/resume", status=202)
        failure = api.wait_status(killed_run, "failed")
        assert "uncertain" in failure["state"]["error"].lower() or "automatic replay" in failure["state"]["error"].lower()
        assert (workspace / "effect.txt").read_text() == "once\n"
        assert not saved(proof, killed_run)["results"]
        evidence = {
            "status": "passed", "run": run, "interrupted_run": killed_run,
            "provider": "real configured model", "mcp": "local Rust HTTP fixture",
            "verified": ["HTTP durable trigger", "pause after current node", "paused graph rejects another trigger", "stdio read allowed and second writer denied", "service restart", "Plugin drift rejects resume", "resume frozen snapshot despite graph edit", "completed producer not repeated", "real Agent + MCP + sandbox artifact", "stable graph identity", "kill during command remains uncertain without replay"],
            "model_requests": sum(item["completion"]["model_requests"] for values in after["results"].values() for item in values),
            "artifact": str(output.relative_to(proof)),
            "limits": "No browser, production switch, safe replay of unknown effects, or arbitrary Agent kill/restart acceptance.",
        }
        (proof / "evidence.json").write_text(json.dumps(evidence, indent=2))
        print(json.dumps({"proof": str(proof), **evidence}, indent=2))
    except Exception as error:
        (proof / "acceptance-failure.json").write_text(json.dumps({"type": type(error).__name__, "status": "failed", "reason": str(error)}, indent=2))
        raise
    finally:
        if api:
            api.close()
        terminate(fixture)


if __name__ == "__main__":
    main()
