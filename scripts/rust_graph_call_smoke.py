"""Opt-in real-provider acceptance for Rust wait-mode Op.call with child MCP."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import select
import sqlite3
import struct
import subprocess
import tempfile
import uuid

from anchor.runtime.secrets import load_dotenv


ROOT = Path(__file__).resolve().parents[1]


def _bundle(root: Path, graph: dict, plugin_resource: bytes | None = None) -> None:
    root.mkdir(parents=True)
    (root / "graph.json").write_text(json.dumps(graph, ensure_ascii=False, indent=2))
    plugins = []
    if plugin_resource is not None:
        plugin = root / "plugins/fixture"
        plugin.mkdir(parents=True)
        (plugin / "plugin.json").write_bytes(plugin_resource)
        digest = hashlib.sha256(
            b"plugin.json" + hashlib.sha256(plugin_resource).digest()
        ).hexdigest()
        plugins.append({
            "id": "fixture",
            "digest": digest,
            "resources": ["plugin.json"],
            "mcp_servers": ["fixture"],
        })
    (root / "manifest.json").write_text(
        json.dumps({"format": 1, "graph": "graph.json", "plugins": plugins})
    )


def _decode_frame(data: bytes) -> dict:
    if len(data) < 4:
        raise RuntimeError("Rust Host returned no complete response frame")
    size = struct.unpack(">I", data[:4])[0]
    if len(data) < 4 + size:
        raise RuntimeError("Rust Host response frame is truncated")
    return json.loads(data[4 : 4 + size])


def main() -> None:
    load_dotenv(ROOT / ".env")
    required = ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")
    if any(not os.environ.get(name) for name in required):
        raise SystemExit("Missing ANCHOR_MODEL_API_KEY/URL/NAME; no model request made")
    proof_root = ROOT / ".local"
    proof_root.mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-graph-call-", dir=proof_root))
    catalog = proof / "catalog"
    catalog.mkdir()
    plugin_resource = b'{"name":"Local child Graph fixture","mcpServers":{"fixture":{}}}'
    token = "child-evidence-" + uuid.uuid4().hex
    child = {
        "objective": "Use the attached MCP fixture for the requested text transformation.",
        "input": {"text": token},
        "entry": "worker",
        "agents": {
            "worker": {
                "model": os.environ["ANCHOR_MODEL_NAME"],
                "network": True,
                "wall_time_limit_seconds": 120,
                "instructions": (
                    "Read the text field from the Graph input. Search attached MCP tools for "
                    "fixture_suffix, then call exactly the returned tool using the returned "
                    "server_id, tool_name, and arguments={\"text\": input.text}. "
                    "Do not invent the suffix or use any other transformation. "
                    "Return a concise summary containing the exact structured result text."
                ),
            }
        },
        "ops": {},
        "nodes": [{"id": "worker", "agent": "worker", "plugins": ["fixture"]}],
        "edges": [],
    }
    parent = {
        "objective": "Wait for the child Graph that performs the requested MCP transformation.",
        "input": {},
        "entry": "invoke",
        "agents": {},
        "ops": {
            "invoke": {
                "call": {
                    "graph": "child",
                    "mode": "wait",
                    "input": {"text": token},
                }
            }
        },
        "nodes": [{"id": "invoke", "op": "invoke", "plugins": []}],
        "edges": [],
    }
    _bundle(catalog / "child", child, plugin_resource)
    bundle = proof / "parent"
    _bundle(bundle, parent)

    fixture = subprocess.Popen(
        [str(ROOT / "rust/target/debug/examples/http_fixture"), str(proof / "mcp-call.json")],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env={"PATH": os.environ.get("PATH", "/usr/bin:/bin")},
    )
    try:
        if fixture.stdout is None or not select.select([fixture.stdout], [], [], 10)[0]:
            raise RuntimeError("local MCP fixture did not start")
        endpoint = fixture.stdout.readline().strip()
        if not endpoint.startswith("http://127.0.0.1:"):
            raise RuntimeError("local MCP fixture returned an invalid endpoint")
        env = {name: os.environ[name] for name in required}
        env.update(
            {
                "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                "ANCHOR_MODEL_WIRE_API": os.environ.get("ANCHOR_MODEL_WIRE_API", "chat"),
                "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle),
                "ANCHOR_RUNNER_CATALOG_ROOT": str(catalog),
                "ANCHOR_RUNNER_STATE_ROOT": str(proof / "state"),
                "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "work"),
                "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,printf,test",
                "ANCHOR_RUST_MCP_SERVERS": json.dumps(
                    {
                        "fixture": {
                            "transport": "http",
                            "endpoint": endpoint,
                            "allowed_tools": ["fixture_suffix"],
                        }
                    }
                ),
            }
        )
        request = {
            "op": "start_bundle",
            "version": 1,
            "request_id": "rust-op-call-acceptance",
            "run_id": "rust-op-call-parent",
            "input": {},
        }
        framed = json.dumps(request).encode()
        result = subprocess.run(
            [str(ROOT / "rust/target/debug/anchor-runner-host")],
            input=struct.pack(">I", len(framed)) + framed,
            capture_output=True,
            env=env,
            timeout=240,
            check=True,
        )
        response = _decode_frame(result.stdout)
        (proof / "response.json").write_text(json.dumps(response, indent=2))
        if response.get("status") != "completed":
            raise RuntimeError(f"parent/child Graph did not complete; inspect {proof / 'response.json'}")

        parent_record = json.loads(
            (proof / "state/runs/rust-op-call-parent.json").read_text()
        )
        calls = list(parent_record.get("graph_calls", {}).values())
        if len(calls) != 1 or calls[0].get("mode") != "wait":
            raise RuntimeError("parent Run does not contain exactly one completed wait call")
        child_id = calls[0].get("child_run_id")
        if not child_id:
            raise RuntimeError("parent wait call has no durable child Run identity")
        child_record = json.loads((proof / "state/runs" / f"{child_id}.json").read_text())
        if child_record.get("status") != "completed":
            raise RuntimeError(f"child Run status is {child_record.get('status')!r}")
        rendered_child = json.dumps(child_record, ensure_ascii=False)
        if token + "-checked" not in rendered_child:
            raise RuntimeError("child completion does not contain the fixture's exact transformed text")
        fixture_call = json.loads((proof / "mcp-call.json").read_text())
        if fixture_call.get("input") != token or fixture_call.get("output", {}).get("text") != token + "-checked":
            raise RuntimeError("local MCP fixture did not record the exact child call")

        stores = list((proof / "state/io-harness/store").glob("*.sqlite3"))
        if len(stores) != 1:
            raise RuntimeError(f"expected one child Agent store, found {len(stores)}")
        with sqlite3.connect(stores[0]) as store:
            tool_calls = [
                call
                for (serialized,) in store.execute("SELECT calls FROM step_turns ORDER BY step")
                for call in json.loads(serialized)
                if call["name"].startswith("anchor_mcp__")
            ]
            provider_calls = store.execute(
                "SELECT model,prompt_tokens,completion_tokens,total_tokens FROM provider_calls ORDER BY id"
            ).fetchall()
        names = [call["name"] for call in tool_calls]
        if names[:2] != ["anchor_mcp__search_tools", "anchor_mcp__call_tool"]:
            raise RuntimeError(f"child did not search before calling: {names}")
        selected = tool_calls[1]["arguments"]
        if selected != {
            "server_id": "fixture",
            "tool_name": "fixture_suffix",
            "arguments": {"text": token},
        }:
            raise RuntimeError("child Agent called a different MCP server/tool/argument")

        evidence = {
            "status": "passed",
            "parent_run": parent_record["run_id"],
            "child_run": child_id,
            "parent_status": parent_record["status"],
            "child_status": child_record["status"],
            "mode": calls[0]["mode"],
            "tool_sequence": names,
            "selected_tool": {"server_id": selected["server_id"], "tool_name": selected["tool_name"]},
            "inventory_tools": 1,
            "provider_model_observed": sorted({row[0] for row in provider_calls if row[0]}),
            "provider_prompt_tokens": [row[1] for row in provider_calls],
            "provider_prompt_tokens_total": sum(row[1] or 0 for row in provider_calls),
            "fixture": "local Rust Streamable HTTP MCP",
            "provider": "real configured endpoint; use observed model metadata for compatibility claims",
        }
        (proof / "evidence.json").write_text(json.dumps(evidence, indent=2))
        print(json.dumps({"proof": str(proof), **evidence}, indent=2))
    finally:
        fixture.terminate()
        try:
            fixture.wait(timeout=5)
        except subprocess.TimeoutExpired:
            fixture.kill()
            fixture.wait()


if __name__ == "__main__":
    main()
