"""Opt-in real-model acceptance: Rust Op -> Agent + HTTP MCP -> Op.

The Python script only prepares fixtures and checks evidence. Runtime execution,
MCP server, persistence and Sandbox are Rust binaries. Loads local .env without
printing credentials. Run after cargo build --workspace --bins --examples in rust/.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import select
import shlex
import sqlite3
import struct
import subprocess
import tempfile
import uuid

from anchor.runtime.secrets import load_dotenv


ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    load_dotenv(ROOT / ".env")
    required = ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")
    if any(not os.environ.get(name) for name in required):
        raise SystemExit("Missing ANCHOR_MODEL_API_KEY/URL/NAME; no model request made")
    output_root = ROOT / ".local"
    output_root.mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-multinode-", dir=output_root))
    fixture_tools = int(os.environ.get("ANCHOR_RUST_MCP_FIXTURE_TOOLS", "0"))
    fixture_command = [str(ROOT / "rust/target/debug/examples/http_fixture"), str(proof / "mcp-call.json")]
    if fixture_tools:
        fixture_command.extend(["--large-tools", str(fixture_tools)])
    fixture = subprocess.Popen(
        fixture_command,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        env={"PATH": os.environ.get("PATH", "/usr/bin:/bin")},
    )
    try:
        if not select.select([fixture.stdout], [], [], 10)[0]:
            raise RuntimeError("MCP fixture failed to start")
        endpoint = fixture.stdout.readline().strip()
        if not endpoint.startswith("http://127.0.0.1:"):
            raise RuntimeError("invalid local fixture endpoint")
        bundle = proof / "bundle"
        plugin = bundle / "plugins/fixture"
        plugin.mkdir(parents=True)
        resource = json.dumps({
            "name": "Local acceptance fixture",
            "mcpServers": {"fixture": {"type": "http", "url": endpoint}},
        }).encode()
        (plugin / "plugin.json").write_bytes(resource)
        digest = hashlib.sha256(b"plugin.json" + hashlib.sha256(resource).digest()).hexdigest()
        token = "evidence-" + uuid.uuid4().hex
        command = lambda script: shlex.join(["sh", "-c", script])
        graph = {
            "objective": "Read the producer file, transform via fixture_suffix MCP, write a report, verify it.",
            "entry": "producer",
            "agents": {"worker": {
                "model": os.environ["ANCHOR_MODEL_NAME"], "network": True,
                "wall_time_limit_seconds": 120,
                "instructions": (
                    "Use anchor_run to read /in/producer/source.txt. Call the attached MCP tool "
                    "fixture-fixture_fixture_suffix with text=<exact source text>. "
                    "Use anchor_run with command=[\"sh\",\"-c\",\"printf '%s' '<returned structured text field>' > /workspace/report.txt\"] "
                    "to write ONLY the returned structured text field into /workspace/report.txt "
                    "(no newline, no quotes, no Markdown); do not use bash, tee, write_file, exec, or shell. "
                    "Do not guess the text or compute the suffix yourself. "
                    "Read back report.txt, then return JSON summary and route=verify."
                ),
            }},
            "ops": {
                "produce": {"run": command(f"printf %s {shlex.quote(token)} > source.txt")},
                "verify": {"run": command(
                    f'test "$(cat /in/worker/report.txt)" = {shlex.quote(token + "-checked")} '
                    "&& cp /in/worker/report.txt verified.txt"
                )},
            },
            "nodes": [
                {"id": "producer", "op": "produce"},
                {"id": "worker", "agent": "worker", "plugins": ["fixture"]},
                {"id": "verify", "op": "verify"},
            ],
            "edges": [{"from": "producer", "to": "worker"}, {"from": "worker", "to": "verify"}],
        }
        (bundle / "graph.json").write_text(json.dumps(graph))
        (bundle / "manifest.json").write_text(json.dumps({"format": 1, "graph": "graph.json", "plugins": [
            {"id": "fixture", "digest": digest, "resources": ["plugin.json"], "mcp_servers": ["fixture"]}
        ]}))
        env = {name: os.environ[name] for name in required}
        env.update({
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "ANCHOR_MODEL_WIRE_API": os.environ.get("ANCHOR_MODEL_WIRE_API", "responses"),
            "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle),
            "ANCHOR_RUNNER_STATE_ROOT": str(proof / "state"),
            "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "work"),
            "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,cp,printf,test",
        })
        request = json.dumps({"op": "start_bundle", "version": 1, "request_id": "acceptance", "run_id": "native-multinode", "input": {}}).encode()
        result = subprocess.run([str(ROOT / "rust/target/debug/anchor-runner-host")],
                                input=struct.pack(">I", len(request)) + request, capture_output=True, env=env, timeout=150, check=True)
        size = struct.unpack(">I", result.stdout[:4])[0]
        response = json.loads(result.stdout[4:4 + size])
        (proof / "response.json").write_text(json.dumps(response, indent=2))
        if response.get("status") != "completed":
            raise RuntimeError(f"Graph did not complete; inspect {proof / 'response.json'}")
        record = json.loads((proof / "state/runs/native-multinode.json").read_text())
        commit = record["results"]["verify"][0]["commit"]["id"]
        final = proof / "state/artifacts" / commit / "files/verified.txt"
        assert final.read_text() == token + "-checked"
        mcp_call = json.loads((proof / "mcp-call.json").read_text())
        assert mcp_call["input"] == token
        stores = list((proof / "state/io-harness/store").glob("*.sqlite3"))
        assert len(stores) == 1, f"expected one AgentNode store, found {stores}"
        with sqlite3.connect(stores[0]) as store:
            calls = [
                call
                for (serialized,) in store.execute("SELECT calls FROM step_turns ORDER BY step")
                for call in json.loads(serialized)
                if call["name"].startswith("fixture-fixture_")
            ]
            provider_calls = store.execute(
                "SELECT step, model, prompt_tokens, completion_tokens, total_tokens "
                "FROM provider_calls ORDER BY id"
            ).fetchall()
        mcp_tool_sequence = [call["name"] for call in calls]
        assert mcp_tool_sequence == ["fixture-fixture_fixture_suffix"], mcp_tool_sequence
        assert calls[0]["arguments"] == {"text": token}
        evidence = {"status": "passed", "run": record["run_id"], "nodes": sorted(record["results"], key=lambda node: record["results"][node][0]["sequence"]),
                    "model_requests": record["results"]["worker"][0]["completion"]["model_requests"],
                    "mcp_tool_sequence": mcp_tool_sequence,
                    "mcp_selected_tool": {"server_id": "fixture-fixture", "tool_name": "fixture_suffix"},
                    "mcp_inventory_tools": fixture_tools + 1,
                    "provider_wire": env["ANCHOR_MODEL_WIRE_API"],
                    "provider_model_observed": sorted({row[1] for row in provider_calls if row[1]}),
                    "provider_prompt_tokens": [row[2] for row in provider_calls],
                    "provider_prompt_tokens_total": sum(row[2] or 0 for row in provider_calls),
                    "mcp": "local Rust fixture over real HTTP", "provider": "real configured model",
                    "verified_artifact": str(final.relative_to(proof))}
        (proof / "evidence.json").write_text(json.dumps(evidence, indent=2))
        print(json.dumps({"evidence": str(proof / "evidence.json"), **evidence}, indent=2))
    finally:
        fixture.terminate()
        try:
            fixture.wait(timeout=5)
        except subprocess.TimeoutExpired:
            fixture.kill()
            fixture.wait()


if __name__ == "__main__":
    main()
