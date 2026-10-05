from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

from scripts.graph_conformance import OUTPUT, _first_difference, generate

ROOT = Path(__file__).resolve().parents[1]


def test_python_graph_conformance_baseline_is_current():
    expected = json.loads(OUTPUT.read_text(encoding="utf-8"))
    actual = generate()
    difference = _first_difference(expected, actual)

    assert difference is None, (
        "Python Graph golden changed at "
        f"{difference[0] if difference else ''}; run "
        "./.venv/bin/python scripts/graph_conformance.py --update after reviewing the change"
    )


def test_python_graph_conformance_cases_expose_migration_features():
    cases = {case["id"]: case for case in generate()["cases"]}

    assert cases["academic-simple"]["summary"]["entry"] == "plan"
    assert cases["revise-loop"]["summary"]["feedback_edges"]
    assert cases["parallel-audit"]["summary"]["parallel_regions"] == [
        {"fanout": "fanout", "join": "join", "branches": [["architecture"], ["recovery"]]}
    ]
    assert "write/draft" in cases["survey-modular"]["summary"]["node_order"]
    assert cases["plugin-research"]["summary"]["plugins"] == [
        {"node": "research", "plugins": ["academic-research"]}
    ]
    assert cases["call-report"]["summary"]["calls"][0]["call"]["graph"] == "call-worker"
    assert cases["call-report"]["call_targets"]["call-worker"]["source_path"] == (
        "examples/graphs/call-worker.json"
    )


def test_layout_is_retained_as_authoring_input_but_not_runtime_snapshot():
    case = next(case for case in generate()["cases"] if case["id"] == "deep-academic-research")

    assert "layout" in case["authoring_graph"]
    assert "/layout" in case["authoring_field_paths"]["layout"]
    assert "layout" not in case["expanded_graph"]
    assert len(case["source_sha256"]) == 64


def test_conformance_difference_report_names_the_changed_graph_path():
    assert _first_difference({"cases": [{"entry": "plan"}]},
                             {"cases": [{"entry": "gather"}]}) == (
                                 "/cases/0/entry", "plan", "gather"
                             )


def test_conformance_cli_check_succeeds_from_repository_root():
    completed = subprocess.run(
        [sys.executable, "scripts/graph_conformance.py", "--check"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )

    assert completed.returncode == 0, completed.stderr or completed.stdout
    assert "8 cases" in completed.stdout
