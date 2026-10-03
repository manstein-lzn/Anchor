"""Opt-in real-provider HTTP acceptance for same-Run io-harness recovery.

The script creates a disposable Rust Graph bundle, kills the API host while an
Anchor sandbox tool has written a unique marker but is still running, restarts
the host, and resolves that exact Harness attempt as Completed. The downstream
Op verifies that the side effect was not replayed. No production Graphs or
schedules are read or changed. Provider credentials are loaded from local
configuration and are never printed or copied into the evidence file.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import select
import shlex
import socket
import subprocess
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
import uuid

from anchor.runtime.secrets import load_dotenv


ROOT = Path(__file__).resolve().parents[1]
MODEL_KEYS = ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")


def until(check, seconds: float, what: str):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise TimeoutError(f"Timed out waiting for {what}")


def terminate(process: subprocess.Popen | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


class Api:
    def __init__(self, env: dict[str, str], proof: Path, port: int):
        self.env = env
        self.proof = proof
        self.base = f"http://127.0.0.1:{port}"
        self.process: subprocess.Popen | None = None
        self.generation = 0

    def start(self) -> None:
        self.generation += 1
        log_path = self.proof / f"host-{self.generation}.log"
        with log_path.open("wb") as log:
            self.process = subprocess.Popen(
                [str(ROOT / "rust/target/debug/anchor-runner-host"), "serve"],
                env=self.env,
                stdout=log,
                stderr=log,
            )
        until(self.ready, 20, "Rust HTTP host readiness")

    def ready(self) -> bool:
        if self.process is not None and self.process.poll() is not None:
            raise RuntimeError("Rust HTTP host exited; inspect the retained proof log")
        try:
            return self.request("GET", "/health")[0] == 200
        except (URLError, ConnectionError):
            return False

    def request(self, method: str, path: str, body=None):
        request = Request(
            self.base + path,
            method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        try:
            with urlopen(request, timeout=20) as response:
                raw = response.read()
                return response.status, json.loads(raw) if raw else None
        except HTTPError as error:
            raw = error.read()
            return error.code, json.loads(raw) if raw else None

    def expect(self, method: str, path: str, body=None, status=200):
        actual, value = self.request(method, path, body)
        if actual != status:
            raise AssertionError(f"{method} {path}: expected {status}, got {actual}: {value}")
        return value

    def detail(self, run_id: str):
        return self.expect("GET", f"/runs/{run_id}")

    def close(self) -> None:
        terminate(self.process)


def main() -> None:
    load_dotenv(ROOT / ".env")
    if any(not os.environ.get(key) for key in MODEL_KEYS):
        raise SystemExit("Missing real provider configuration; no request was made")
    smoke_model = os.environ.get("ANCHOR_RECOVERY_SMOKE_MODEL", os.environ["ANCHOR_MODEL_NAME"])
    smoke_wire_api = os.environ.get(
        "ANCHOR_RECOVERY_SMOKE_WIRE_API",
        os.environ.get("ANCHOR_MODEL_WIRE_API", "chat"),
    )
    executable = ROOT / "rust/target/debug/anchor-runner-host"
    fixture_executable = ROOT / "rust/target/debug/examples/http_fixture"
    if not executable.is_file():
        raise SystemExit("Build the Rust host first: cd rust && cargo build -p anchor-runner-host")
    if not fixture_executable.is_file():
        raise SystemExit("Build the local MCP fixture first: cd rust && cargo build -p anchor-mcp-host --example http_fixture")

    output_root = ROOT / ".local"
    output_root.mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-recovery-", dir=output_root))
    bundle = proof / "bundle"
    plugin_dir = bundle / "plugins/fixture"
    plugin_dir.mkdir(parents=True)
    token = "recovery-" + uuid.uuid4().hex
    command = lambda script: shlex.join(["sh", "-c", script])
    resource = b'{"name":"Local recovery acceptance fixture","mcpServers":{"fixture":{}}}'
    (plugin_dir / "plugin.json").write_bytes(resource)
    plugin_digest = hashlib.sha256(b"plugin.json" + hashlib.sha256(resource).digest()).hexdigest()
    graph = {
        "objective": "Resume the same Graph Run after an interrupted mutating tool and verify its effect once.",
        "entry": "producer",
        "agents": {
            "worker": {
                "model": smoke_model,
                "network": True,
                "instructions": (
                    "Read /in/producer/source.txt using anchor_run. Search attached MCP tools for fixture_suffix, "
                    "then call the exact returned tool using anchor_mcp__call_tool with its server_id, tool_name, "
                    "and arguments={\"text\": <exact source text>}. "
                    "Write only the returned structured text field into /workspace/effect.txt as one line. "
                    "For that single write, call anchor_run once with a shell command that appends the line "
                    "and then runs sleep 90, so an operator can inspect the result while the command is active. "
                    "Do not repeat this mutating command. The operator may supply the result of that exact call "
                    "after checking the workspace. After receiving its result, return a concise summary and "
                    "route=verify. Do not guess or compute the suffix yourself."
                ),
            }
        },
        "ops": {
            "producer": {"run": command(f"printf %s {shlex.quote(token)} > source.txt")},
            "verify": {
                "run": command(
                    f'test "$(cat /in/work/effect.txt)" = {shlex.quote(token + "-checked")} '
                    "&& cp /in/work/effect.txt verified.txt"
                )
            }
        },
        "nodes": [
            {"id": "producer", "op": "producer"},
            {"id": "work", "agent": "worker", "plugins": ["fixture"]},
            {"id": "verify", "op": "verify"},
        ],
        "edges": [{"from": "producer", "to": "work"}, {"from": "work", "to": "verify"}],
    }
    (bundle / "graph.json").write_text(json.dumps(graph), encoding="utf-8")
    (bundle / "manifest.json").write_text(
        json.dumps({"format": 1, "graph": "graph.json", "plugins": [
            {"id": "fixture", "digest": plugin_digest, "resources": ["plugin.json"],
             "mcp_servers": ["fixture"]}
        ]}),
        encoding="utf-8",
    )

    with socket.socket() as reserved:
        reserved.bind(("127.0.0.1", 0))
        port = reserved.getsockname()[1]
    env = {key: os.environ[key] for key in MODEL_KEYS}
    env.update({
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "ANCHOR_MODEL_NAME": smoke_model,
        "ANCHOR_MODEL_WIRE_API": smoke_wire_api,
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle),
        "ANCHOR_RUNNER_CATALOG_ROOT": str(proof),
        "ANCHOR_RUNNER_GRAPH_NAME": "recovery",
        "ANCHOR_RUNNER_STATE_ROOT": str(proof / "state"),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "work"),
        "ANCHOR_RUNNER_LISTEN": f"127.0.0.1:{port}",
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,bash,cat,cp,test,sleep,printf",
    })
    fixture = subprocess.Popen(
        [str(fixture_executable), str(proof / "mcp-calls.jsonl"), "--append-evidence"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env={"PATH": os.environ.get("PATH", "/usr/bin:/bin")},
    )
    if not select.select([fixture.stdout], [], [], 15)[0]:
        terminate(fixture)
        raise RuntimeError("Local Rust MCP fixture did not become ready")
    endpoint = fixture.stdout.readline().strip()
    if not endpoint.startswith("http://127.0.0.1:"):
        terminate(fixture)
        raise RuntimeError("Local MCP fixture returned an invalid endpoint")
    env["ANCHOR_RUST_MCP_SERVERS"] = json.dumps({"fixture": {
        "transport": "http", "endpoint": endpoint, "allowed_tools": ["fixture_suffix"]
    }})
    api = Api(env, proof, port)
    try:
        api.start()
        run_id = api.expect("POST", "/trigger", {"graph": "recovery"}, 202)["run"]

        def effect_workspace():
            record_path = proof / "state/runs" / f"{run_id}.json"
            if not record_path.exists():
                return None
            record = json.loads(record_path.read_text(encoding="utf-8"))
            if record.get("status") in {"failed", "completed", "aborted"}:
                raise RuntimeError(
                    "Graph Run reached a terminal state before the sandbox marker appeared"
                )
            cursor = record.get("cursor")
            if not cursor or cursor["node_id"] != "work":
                return None
            key = cursor["key"]
            identity = ":".join(str(key[name]) for name in (
                "run_id", "graph_digest", "node_id", "invocation"
            ))
            workspace = proof / "work" / run_id / hashlib.sha256(identity.encode()).hexdigest()
            marker = workspace / "effect.txt"
            if marker.is_file() and marker.read_text(encoding="utf-8") == token + "-checked\n":
                return workspace
            return None

        workspace = until(effect_workspace, 240, "the one-time sandbox side effect")
        before = api.detail(run_id)
        assert before["state"]["status"] == "running", before["state"]
        before_record = json.loads((proof / "state/runs" / f"{run_id}.json").read_text())
        original_key = before_record["cursor"]["key"]
        durable_identity = ":".join(str(original_key[name]) for name in (
            "run_id", "graph_digest", "node_id", "invocation"
        ))
        stem = "np1-" + hashlib.sha256(durable_identity.encode()).hexdigest()
        io_id_file = proof / "state/io-harness/store" / f"{stem}.run"

        # The marker is written only after io-harness has started the mutating
        # tool call. Kill before its 30-second Anchor tool timeout can return.
        assert api.process is not None
        api.process.kill()
        api.process.wait(timeout=10)
        api.process = None
        api.start()

        inactive = api.detail(run_id)
        assert inactive["state"]["status"] == "running", inactive["state"]
        assert inactive["active"] is False, inactive
        api.expect("POST", f"/runs/{run_id}/resume", status=202)

        waiting = until(
            lambda: (
                detail if (
                    (detail := api.detail(run_id))["state"]["status"] == "waiting_recovery"
                    and detail["active"] is False
                )
                else None
            ),
            45,
            "the same invocation to expose its idle unresolved Harness attempt",
        )
        attempts = waiting["state"]["recovery"]
        assert len(attempts) == 1, attempts
        pending = attempts[0]
        assert pending["key"]["node_id"] == "work", pending
        assert pending["key"]["invocation"] == original_key["invocation"], pending
        assert pending["attempt"]["tool"] == "anchor_run", pending
        mcp_calls = [json.loads(line) for line in (proof / "mcp-calls.jsonl").read_text().splitlines()]
        assert len(mcp_calls) == 1 and mcp_calls[0]["input"] == token, mcp_calls
        assert io_id_file.is_file(), "resumed NodePort must persist the original Harness run id"
        harness_run_id = int(io_id_file.read_text().strip())

        api.expect("POST", f"/runs/{run_id}/recovery", {
            "node_id": pending["key"]["node_id"],
            "invocation": pending["key"]["invocation"],
            "attempt_id": pending["attempt"]["attempt_id"],
            "decision": "completed",
            "observation": f"I inspected {token}-checked in the original Agent workspace; the command wrote it once.",
        }, 202)
        until(
            lambda: (
                detail if (detail := api.detail(run_id))["state"]["status"] == "completed"
                else None
            ),
            240,
            "the same Graph Run to complete after its recovery decision",
        )
        after_record = json.loads((proof / "state/runs" / f"{run_id}.json").read_text())
        assert after_record["run_id"] == run_id
        assert after_record["graph_digest"] == original_key["graph_digest"]
        assert after_record["recovery_submissions"][-1]["decision"]["decision"] == "completed"
        assert after_record["recovery_submissions"][-1]["key"]["invocation"] == original_key["invocation"]
        assert (workspace / "effect.txt").read_text(encoding="utf-8").splitlines() == [token + "-checked"]
        mcp_calls_after = [json.loads(line) for line in (proof / "mcp-calls.jsonl").read_text().splitlines()]
        assert len(mcp_calls_after) == 1, mcp_calls_after
        verify = after_record["results"]["verify"][-1]
        artifact = proof / "state/artifacts" / verify["commit"]["id"] / "files/verified.txt"
        assert artifact.read_text(encoding="utf-8").splitlines() == [token + "-checked"]

        evidence = {
            "status": "passed",
            "provider": "real configured provider; credentials omitted",
            "run": run_id,
            "harness_run_id": harness_run_id,
            "recovery": "completed",
            "same_graph_digest": True,
            "same_invocation": original_key["invocation"],
            "attempt_id": pending["attempt"]["attempt_id"],
            "effect_lines": len((workspace / "effect.txt").read_text(encoding="utf-8").splitlines()),
            "mcp_calls": len(mcp_calls_after),
            "verified_artifact": str(artifact.relative_to(proof)),
            "host_restarts": 1,
        }
        (proof / "evidence.json").write_text(json.dumps(evidence, indent=2), encoding="utf-8")
        print(json.dumps({"proof": str(proof), **evidence}, indent=2), flush=True)
    except Exception as error:
        (proof / "acceptance-failure.json").write_text(json.dumps({
            "status": "failed",
            "error_type": type(error).__name__,
            "reason": str(error),
        }, indent=2), encoding="utf-8")
        raise
    finally:
        api.close()
        terminate(fixture)


if __name__ == "__main__":
    main()
