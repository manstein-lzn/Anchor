from __future__ import annotations

import ast
import hashlib
import importlib.util
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/rust_weekly_acceptance.py"
spec = importlib.util.spec_from_file_location("rust_weekly_acceptance", SCRIPT)
module = importlib.util.module_from_spec(spec)
assert spec.loader
spec.loader.exec_module(module)


def test_acceptance_bundle_keeps_graph_bytes_and_uses_reject_only_docmost_gate(tmp_path):
    bundle, digest = module.make_bundle(tmp_path)
    source = ROOT / "examples/graphs/weekly-work-report.json"
    assert digest == hashlib.sha256(source.read_bytes()).hexdigest()
    assert (bundle / "graph.json").read_bytes() == source.read_bytes()
    plugin = json.loads((bundle / "plugins/docmost/plugin.json").read_text())
    assert plugin["mcpServers"]["docmost"]["command"] == "python3"
    assert "https://" not in (bundle / "plugins/docmost/server.py").read_text()
    assert "SAFE_GATE_REJECTED" in (bundle / "plugins/docmost/server.py").read_text()


def test_acceptance_fixture_has_no_production_credentials_or_endpoint_literals():
    source = SCRIPT.read_text()
    tree = ast.parse(source)
    literals = [node.value for node in ast.walk(tree) if isinstance(node, ast.Constant) and isinstance(node.value, str)]
    joined = "\n".join(literals)
    assert "docmost.cwise.dev" not in joined
    assert "DOCMOST_API_KEY" in joined  # explicit environment deletion is part of the gate
    assert "ANCHOR_CHANNEL_CONTROL_DESCRIPTOR" in joined
    assert "ANCHOR_WECOM_SEND_USERS" in joined


def test_fixture_provider_requires_feedback_and_safe_gate_tool():
    assert module.EXPECTED_NODES == ["collect", "understand", "write", "review", "gate", "publish", "docmost"]
    assert "SAFE_GATE_REJECTED" in module.SAFE_GATE_SERVER
    assert "isError" in module.SAFE_GATE_SERVER
