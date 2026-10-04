from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess

from anchor.simple.graph import load

ROOT = Path(__file__).resolve().parents[1]
GRAPH_PATH = ROOT / "examples/rust-rsi/graph.json"


def _proposal(*, path="code/src/main.rs", layer="frozen_source", scope="current_source"):
    citation = {
        "path": path,
        "sha256": "a" * 64,
        "redacted": False,
        "line_basis": "frozen projection, not original source",
        "layer": layer,
        "locator": "line 12",
    }
    return {
        "id": "RSI-test-1",
        "source_snapshot": {"captured_at": "2026-10-04T10:00:00Z"},
        "collection_window": {
            "start": "2026-09-27T10:00:00Z",
            "end_exclusive": "2026-10-04T10:00:00Z",
        },
        "source_layer": layer,
        "claim_scope": scope,
        "evidence": [citation],
    }


def _run_gate(tmp_path: Path, proposal: dict) -> str:
    graph = json.loads(GRAPH_PATH.read_text())
    command = graph["ops"]["gate"]["run"]
    (tmp_path / "analyze").mkdir(parents=True)
    (tmp_path / "review").mkdir()
    (tmp_path / "workspace").mkdir()
    (tmp_path / "bin").mkdir()
    (tmp_path / "analyze/evolution.json").write_text(json.dumps({"proposals": [proposal]}))
    (tmp_path / "review/review.json").write_text('{"passed":true}')
    route = tmp_path / "bin/anchor-route"
    route.write_text("#!/bin/sh\nprintf '%s\\n' \"$2\" > \"$ROUTE_OUTPUT\"\n")
    route.chmod(0o755)
    # Run the exact op command against a temporary /in-shaped fixture.
    command = command.replace("/in/analyze", str(tmp_path / "analyze"))
    command = command.replace("/in/review", str(tmp_path / "review"))
    command = command.replace("/workspace", str(tmp_path / "workspace"))
    env = {**os.environ, "PATH": f"{tmp_path / 'bin'}:{os.environ['PATH']}",
           "ROUTE_OUTPUT": str(tmp_path / "route.txt")}
    subprocess.run(command, shell=True, check=True, env=env, cwd=tmp_path)
    return (tmp_path / "route.txt").read_text().strip()


def test_rust_rsi_has_single_reviewer_and_mechanical_source_gate():
    graph = load(GRAPH_PATH)
    assert graph.routes("review") == ("gate",)
    assert graph.routes("gate") == ("analyze", "publish")
    assert graph.routes("publish") == ()
    assert not graph.max_rounds


def test_rust_rsi_gate_accepts_bound_citation_and_rejects_missing_or_mixed_layer(tmp_path):
    assert shutil.which("jq"), "jq is required by the Rust RSI publish contract"
    valid = _proposal()
    assert _run_gate(tmp_path / "valid", valid) == "publish"

    missing = _proposal()
    del missing["collection_window"]
    assert _run_gate(tmp_path / "missing", missing) == "analyze"

    mixed = _proposal(path="runs/run-1.json")
    assert _run_gate(tmp_path / "mixed", mixed) == "analyze"

    mismatched_snapshot = _proposal()
    mismatched_snapshot["evidence"][0]["sha256"] = "not-a-snapshot-hash"
    assert _run_gate(tmp_path / "snapshot", mismatched_snapshot) == "analyze"

    wrong_capture_time = _proposal()
    wrong_capture_time["source_snapshot"]["captured_at"] = "2026-10-03T10:00:00Z"
    assert _run_gate(tmp_path / "capture-time", wrong_capture_time) == "analyze"

    wrong_scope = _proposal()
    wrong_scope["claim_scope"] = "observed_run_window"
    assert _run_gate(tmp_path / "scope", wrong_scope) == "analyze"

    unsupported_path = _proposal(path="previous/report.md")
    assert _run_gate(tmp_path / "unsupported-path", unsupported_path) == "analyze"

    missing_evidence = _proposal()
    missing_evidence["evidence"] = []
    assert _run_gate(tmp_path / "missing-evidence", missing_evidence) == "analyze"


def test_rust_rsi_gate_accepts_each_supported_source_layer(tmp_path):
    assert shutil.which("jq"), "jq is required by the Rust RSI publish contract"
    supported = [
        ("code/rust/src/main.rs", "frozen_source", "current_source"),
        ("runs/run-1.json", "run_projection", "observed_run_window"),
        ("dependencies/ecosystem-a.json", "external_metadata", "public_metadata_snapshot"),
        ("previous/rsi-report.md", "previous_report", "previous_published_report"),
    ]
    for index, (path, layer, scope) in enumerate(supported):
        proposal = _proposal(path=path, layer=layer, scope=scope)
        assert _run_gate(tmp_path / f"layer-{index}", proposal) == "publish"
