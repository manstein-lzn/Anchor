"""Contract checks for the unchanged 18-node RSI graph and native commit refs."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess

import pytest

from scripts.rsi.commit_binding import verify_commit


ROOT = Path(__file__).resolve().parents[1]


def _native_projection(tmp_path: Path) -> tuple[Path, dict]:
    directory = tmp_path / "run-audit"
    (directory / ".git").mkdir(parents=True)
    (directory / "findings.json").write_text('{"domain":"runs"}\n')
    subprocess.run(["git", "init", "--bare", "--quiet", str(directory / ".git")], check=True)
    blob = subprocess.check_output(
        ["git", f"--git-dir={directory / '.git'}", "hash-object", "-w", "--no-filters", str(directory / "findings.json")],
        text=True,
    ).strip()
    tree_input = f"100644 blob {blob}\tfindings.json\0".encode()
    tree = subprocess.check_output(
        ["git", f"--git-dir={directory / '.git'}", "mktree", "-z"], input=tree_input
    ).decode().strip()
    artifact_id = "fs2-" + "a" * 64
    message = (
        f"Anchor Artifact {artifact_id}\n"
        "Artifact-Node: run-audit\n"
        "Artifact-Invocation: 1\n"
        "Manifest-SHA256: " + "b" * 64 + "\n"
    )
    head = subprocess.check_output(
        ["git", f"--git-dir={directory / '.git'}", "commit-tree", tree], input=message.encode()
    ).decode().strip()
    subprocess.run(["git", f"--git-dir={directory / '.git'}", "update-ref", "HEAD", head], check=True)
    return directory, {"id": artifact_id, "node_id": "run-audit", "invocation": 1}


def test_standard_graph_is_the_unchanged_18_node_definition():
    graph = json.loads((ROOT / "examples/graphs/rsi.json").read_text())
    assert len(graph["nodes"]) == 18
    assert graph["entry"] == "collect"
    assert set(graph["agents"]) == {"run-audit", "code-audit", "graph-audit", "plugin-audit",
                                     "dependency-audit", "analyst", "fact-review", "proposal-review"}


def test_native_commit_ref_requires_source_bound_identity(tmp_path):
    directory, expected = _native_projection(tmp_path)
    verify_commit(directory, expected, node="run-audit")

    with pytest.raises(ValueError, match="node identity"):
        verify_commit(directory, {**expected, "node_id": "code-audit"}, node="run-audit")
    with pytest.raises(ValueError, match="exact CommitRef"):
        verify_commit(directory, {"id": expected["id"]}, node="run-audit")
