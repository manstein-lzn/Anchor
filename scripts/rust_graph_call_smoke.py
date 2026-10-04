"""Opt-in real-provider acceptance for the Rust ``Op.call`` R7 closed loop.

The Graph under test is a parent -> child -> parent loop:

* parent ``publish`` (``Op.run``) writes a request file,
* parent ``invoke`` (``Op.call`` wait) hands the file to a child Agent through
  ``files``, maps a value into the child Run through ``input_map``, and selects
  the returned file through ``result``,
* child ``worker`` (Agent + Bubblewrap + local Streamable HTTP MCP) reads the
  handed file, transforms it with the fixture tool, and writes the result,
* parent ``verify`` (``Op.run``) checks the returned ``result/`` file.

The Python script only prepares fixtures and inspects evidence. Runtime
execution, the MCP server, immutable files, persistence and Sandbox are Rust
binaries. Credentials are read from local ``.env`` and never printed.

Build once with ``cargo build --workspace --bins --examples`` inside ``rust/``,
then run::

    ./.venv/bin/python scripts/rust_graph_call_smoke.py

``ANCHOR_MODEL_WIRE_API`` selects the provider wire (``chat`` or ``responses``)
and the observed value is recorded in ``evidence.json``. When credentials are
missing the script exits without making a model request and without writing
passing evidence. When the Rust Host rejects the call contract it writes
``blocked`` evidence naming the gap instead of fabricating success.
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
import sys
import tempfile
import uuid

from anchor.runtime.secrets import load_dotenv
from anchor.simple import graph as graph_module


ROOT = Path(__file__).resolve().parents[1]
PROOF_SCHEMA = 1


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


def _command(script: str) -> str:
    return shlex.join(["sh", "-c", script])


def _graphs(token: str, subject: str) -> tuple[dict, dict]:
    """The parent/child bundles, authored in the product Graph shape."""
    child = {
        "objective": "Transform the handed request file through the fixture MCP tool.",
        "input": {},
        "entry": "worker",
        "agents": {
            "worker": {
                "model": os.environ["ANCHOR_MODEL_NAME"],
                "network": True,
                "wall_time_limit_seconds": 120,
                "instructions": (
                    "Use anchor_run with command=[\"cat\",\"/in/call/request.txt\"] to read the "
                    "request file handed to this child Run. Call the attached MCP tool "
                    "fixture-fixture_fixture_suffix with text=<the exact request file contents>. "
                    "Use anchor_run with command=[\"sh\",\"-c\",\"printf '%s' '<returned structured "
                    "text field>' > /workspace/report.txt\"] to write ONLY the returned structured "
                    "text field into /workspace/report.txt (no newline, no quotes, no Markdown). "
                    "Do not invent the suffix or transform the text yourself. The mapped 'subject' "
                    "value in the Input JSON below comes from the parent's input_map; include it "
                    "verbatim in your JSON summary. Read back report.txt, then return JSON with the "
                    "exact file contents and that subject."
                ),
            }
        },
        "ops": {},
        "nodes": [{"id": "worker", "agent": "worker", "plugins": ["fixture"]}],
        "edges": [],
    }
    parent = {
        "objective": (
            "Publish a request file, wait for the child Graph transformation, then verify "
            "the returned result file."
        ),
        "input": {},
        "entry": "publish",
        "agents": {},
        "ops": {
            "produce": {"run": _command(f"printf %s {shlex.quote(token)} > request.txt")},
            "invoke": {
                "call": {
                    "graph": "child",
                    "mode": "wait",
                    "input_map": {"subject": "/topic"},
                    "files": [{"node": "publish", "path": "request.txt", "as": "request.txt"}],
                    "result": {"node": "worker", "files": ["report.txt"]},
                }
            },
            "verify": {
                "run": _command(
                    f'test "$(cat /in/invoke/result/report.txt)" = {shlex.quote(token + "-checked")} '
                    "&& cp /in/invoke/result/report.txt verified.txt"
                )
            },
        },
        "nodes": [
            {"id": "publish", "op": "produce"},
            {"id": "invoke", "op": "invoke"},
            {"id": "verify", "op": "verify"},
        ],
        "edges": [{"from": "publish", "to": "invoke"}, {"from": "invoke", "to": "verify"}],
    }
    return parent, child


def _validate_fixture_contract(parent: dict, child: dict, token: str, subject: str) -> dict:
    """Provider-free check that the bundles express the frozen product contract."""
    parent_graph = graph_module.parse(parent)
    child_graph = graph_module.parse(child)
    if "worker" not in child_graph.nodes:
        raise RuntimeError("child Graph does not declare the requested result node")
    call = parent_graph.ops["invoke"].call or {}
    return {
        "parent_nodes": list(parent_graph.nodes),
        "child_nodes": list(child_graph.nodes),
        "input_map": dict(call.get("input_map", {})),
        "files": [dict(item) for item in call.get("files", [])],
        "result": dict(call.get("result", {})),
        "mode": call.get("mode"),
        "token": token,
        "mapped_subject": subject,
    }


def _blocked(proof: Path, reason: str, run_id: str, coverage: dict, wire: str, token: str) -> None:
    evidence = {
        "schema": PROOF_SCHEMA,
        "status": "blocked",
        "stage": "host_admission",
        "reason": reason,
        "parent_run": run_id,
        "child_run": None,
        "model_requests": 0,
        "provider_wire": wire,
        "token": token,
        "coverage": coverage,
        "note": (
            "Runtime contract gap in Op.call host integration; no model request was made and "
            "no passing evidence was fabricated."
        ),
    }
    (proof / "evidence.json").write_text(json.dumps(evidence, indent=2))
    print(json.dumps({"proof": str(proof), **evidence}, indent=2))


def _host_env(proof: Path, bundle: Path, catalog: Path, wire: str) -> dict:
    env = {name: os.environ[name] for name in
           ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")}
    env.update(
        {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "ANCHOR_MODEL_WIRE_API": wire,
            "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle),
            "ANCHOR_RUNNER_CATALOG_ROOT": str(catalog),
            "ANCHOR_RUNNER_STATE_ROOT": str(proof / "state"),
            "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "work"),
            "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,cp,printf,test",
        }
    )
    return env


def _collect_evidence(
    proof: Path, parent_run_id: str, coverage: dict, wire: str, token: str, subject: str
) -> dict:
    """Assert the parent -> child -> parent loop and build passing evidence."""
    parent_record = json.loads((proof / "state/runs" / f"{parent_run_id}.json").read_text())
    calls = list(parent_record.get("graph_calls", {}).values())
    if len(calls) != 1 or calls[0].get("mode") != "wait":
        raise RuntimeError("parent Run does not contain exactly one wait call")
    if calls[0].get("status") != "completed":
        raise RuntimeError(f"parent wait call status is {calls[0].get('status')!r}")
    child_id = calls[0].get("child_run_id")
    if not child_id:
        raise RuntimeError("parent wait call has no durable child Run identity")
    child_record = json.loads((proof / "state/runs" / f"{child_id}.json").read_text())
    if child_record.get("status") != "completed":
        raise RuntimeError(f"child Run status is {child_record.get('status')!r}")
    if child_record.get("input") != {"subject": subject}:
        raise RuntimeError(
            f"input_map did not materialize into the child Run: {child_record.get('input')!r}"
        )

    invoke_result = parent_record["results"]["invoke"][0]
    returned = (
        proof / "state/artifacts" / invoke_result["commit"]["id"] / "files/result/report.txt"
    )
    if not returned.is_file():
        raise RuntimeError("parent invoke node has no returned result/report.txt file")
    if returned.read_text() != token + "-checked":
        raise RuntimeError("returned result file does not carry the MCP-transformed text")
    verified = proof / "state/artifacts" / parent_record["results"]["verify"][0]["commit"]["id"]
    if (verified / "files/verified.txt").read_text() != token + "-checked":
        raise RuntimeError("parent verify node did not accept the returned result file")
    fixture_call = json.loads((proof / "mcp-call.json").read_text())
    if (
        fixture_call.get("input") != token
        or fixture_call.get("output", {}).get("text") != token + "-checked"
    ):
        raise RuntimeError("local MCP fixture did not record the exact child call")

    stores = list((proof / "state/io-harness/store").glob("*.sqlite3"))
    if len(stores) != 1:
        raise RuntimeError(f"expected one child Agent store, found {len(stores)}")
    with sqlite3.connect(stores[0]) as store:
        tool_calls = [
            call
            for (serialized,) in store.execute("SELECT calls FROM step_turns ORDER BY step")
            for call in json.loads(serialized)
            if call["name"].startswith("fixture-fixture_")
        ]
        provider_calls = store.execute(
            "SELECT model,prompt_tokens,completion_tokens,total_tokens FROM provider_calls ORDER BY id"
        ).fetchall()
    names = [call["name"] for call in tool_calls]
    if names != ["fixture-fixture_fixture_suffix"]:
        raise RuntimeError(f"child did not call the Plugin-prefixed MCP tool: {names}")
    if tool_calls[0]["arguments"] != {"text": token}:
        raise RuntimeError("child Agent called a different MCP server/tool/argument")

    handed = parent_record["results"]["publish"][0]
    handed_file = proof / "state/artifacts" / handed["commit"]["id"] / "files/request.txt"
    return {
        "schema": PROOF_SCHEMA,
        "status": "passed",
        "parent_run": parent_record["run_id"],
        "child_run": child_id,
        "parent_status": parent_record["status"],
        "child_status": child_record["status"],
        "mode": calls[0]["mode"],
        "coverage": coverage,
        "handoff": {
            "summary": calls[0].get("output"),
            "child_input": child_record["input"],
            "handed_file": {
                "parent_node": "publish",
                "path": "request.txt",
                "child_mount": "/in/call/request.txt",
                "sha256": hashlib.sha256(handed_file.read_bytes()).hexdigest(),
            },
            "returned_file": {
                "child_node": "worker",
                "path": "report.txt",
                "parent_mount": "result/report.txt",
                "sha256": hashlib.sha256(returned.read_bytes()).hexdigest(),
            },
        },
        "token": token,
        "provider_wire": wire,
        "provider_model_observed": sorted({row[0] for row in provider_calls if row[0]}),
        "provider_prompt_tokens": [row[1] for row in provider_calls],
        "provider_completion_tokens": [row[2] for row in provider_calls],
        "provider_tokens_total": sum(row[3] or 0 for row in provider_calls),
        "mcp_tool_sequence": names,
        "mcp_selected_tool": {"server_id": "fixture-fixture", "tool_name": "fixture_suffix"},
        "mcp_inventory_tools": 1,
        "fixture": "local Rust Streamable HTTP MCP under Bubblewrap",
        "provider": "real configured endpoint; use observed model metadata for compatibility claims",
    }


def main() -> None:
    load_dotenv(ROOT / ".env")
    required = ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")
    if any(not os.environ.get(name) for name in required):
        raise SystemExit("Missing ANCHOR_MODEL_API_KEY/URL/NAME; no model request made")
    wire = os.environ.get("ANCHOR_MODEL_WIRE_API", "responses")
    proof_root = ROOT / ".local"
    proof_root.mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-graph-call-", dir=proof_root))
    catalog = proof / "catalog"
    catalog.mkdir()
    token = "req-" + uuid.uuid4().hex
    subject = "topic-" + uuid.uuid4().hex
    parent_run_id = "rust-op-call-parent"
    parent, child = _graphs(token, subject)
    coverage = _validate_fixture_contract(parent, child, token, subject)
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
        plugin_resource = json.dumps({
            "name": "Local child Graph fixture",
            "mcpServers": {"fixture": {"type": "http", "url": endpoint}},
        }).encode()
        _bundle(catalog / "child", child, plugin_resource)
        env = _host_env(proof, bundle, catalog, wire)
        request = {
            "op": "start_bundle",
            "version": 1,
            "request_id": "rust-op-call-acceptance",
            "run_id": parent_run_id,
            "input": {"topic": subject},
        }
        framed = json.dumps(request).encode()
        result = subprocess.run(
            [str(ROOT / "rust/target/debug/anchor-runner-host")],
            input=struct.pack(">I", len(framed)) + framed,
            capture_output=True,
            env=env,
            timeout=300,
            check=True,
        )
        response = _decode_frame(result.stdout)
        (proof / "response.json").write_text(json.dumps(response, indent=2))
        if response.get("kind") != "run":
            _blocked(
                proof,
                str(response.get("reason") or response.get("kind")),
                parent_run_id,
                coverage,
                wire,
                token,
            )
            raise SystemExit(2)
        if response.get("status") != "completed":
            _blocked(
                proof,
                f"parent Run status {response.get('status')!r}",
                parent_run_id,
                coverage,
                wire,
                token,
            )
            raise SystemExit(2)
        evidence = _collect_evidence(proof, parent_run_id, coverage, wire, token, subject)
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
    sys.exit(main())
