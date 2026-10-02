#!/usr/bin/env python3
"""Regenerate the provider-free R5 Graph Runner oracle from Anchor's Python runtime.

Run from the repository root with `./.venv/bin/python scripts/generate_r5_python_oracle.py`.
The source JSON describes input graphs and per-invocation node outcomes; this script runs the real
`anchor.simple.run.run` scheduler with only `_config` and `_agent_for` patched, then records the
expanded snapshot and scheduler facts that Rust consumes in graph.rs tests.
"""

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
from pathlib import Path
from types import SimpleNamespace
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from anchor.simple import run as python_runner  # noqa: E402

SOURCE = ROOT / "tests/fixtures/r5-python-oracle-scenarios.json"
OUTPUT = ROOT / "tests/fixtures/r5-python-oracle.json"


class FakeAgent:
    def __init__(self, directory: Path, outcome: dict[str, Any]):
        self.directory = directory
        self.outcome = outcome
        self.env = SimpleNamespace(route=outcome["route"])

    def run(self, task: str, **_kwargs: object) -> dict[str, str]:
        del task
        return self._finish()

    def resume(self) -> dict[str, str]:
        return self._finish()

    def _finish(self) -> dict[str, str]:
        if self.outcome["exit_status"] == "Submitted":
            path = self.directory / "oracle.txt"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("provider-free oracle\n", encoding="utf-8")
            return {"submission": f"oracle:{self.outcome['node']}:{self.outcome['invocation']}",
                    "exit_status": "Submitted"}
        return {"submission": "", "exit_status": self.outcome["exit_status"]}


def mapped_status(state: Any) -> str:
    """Normalize Python RunState words to the equivalent Kernel status vocabulary."""
    if state.status == "finished":
        return "completed"
    if state.status == "stopped" and state.reason == "budget_exhausted":
        return "budget_stopped"
    if state.status == "stopped":
        return "stopped"
    return state.status


def generate(source: dict[str, Any]) -> dict[str, Any]:
    generated: list[dict[str, Any]] = []
    for scenario in source["scenarios"]:
        invocation_counts: dict[str, int] = {}
        expected = {(item["node"], item["invocation"]): item for item in scenario["outcomes"]}
        actual_outcomes: list[dict[str, Any]] = []

        def fake_agent_for(_graph: Any, node_id: str, directory: Path, *_args: Any,
                           _invocation_counts: dict[str, int] = invocation_counts,
                           _expected: dict[tuple[str, int], dict[str, Any]] = expected,
                           _actual_outcomes: list[dict[str, Any]] = actual_outcomes,
                           **_kwargs: Any) -> FakeAgent:
            invocation = _invocation_counts.get(node_id, 0) + 1
            _invocation_counts[node_id] = invocation
            outcome = _expected.get((node_id, invocation))
            if outcome is None:
                raise AssertionError(f"no fixture outcome for {node_id} invocation {invocation}")
            _actual_outcomes.append(outcome)
            return FakeAgent(Path(directory), outcome)

        python_runner._config = lambda _path: ({}, None)
        python_runner._agent_for = fake_agent_for
        with tempfile.TemporaryDirectory(prefix=f"anchor-r5-{scenario['id']}-") as tmp:
            workspace = Path(tmp) / "workspace"
            workspace.mkdir()
            run_input = scenario["run_override"].get("input", {})
            stop_request = (
                (lambda: "paused")
                if scenario.get("control", {}).get("pause_before_dispatch")
                else None
            )
            with contextlib.redirect_stdout(io.StringIO()):
                state = python_runner.run(
                    workspace,
                    config_path=Path(tmp) / "unused-config.json",
                    run_input=run_input,
                    run_id="oracle-run",
                    definition=scenario["graph"],
                    stop_request=stop_request,
                )
            run_dir = workspace / "runs" / "oracle-run"
            snapshot = json.loads((run_dir / "graph.json").read_text(encoding="utf-8"))
            if actual_outcomes != scenario["outcomes"]:
                raise AssertionError(
                    f"{scenario['id']}: Python scheduler consumed {actual_outcomes!r}, "
                    f"expected {scenario['outcomes']!r}"
                )
            if set(expected) != {(item["node"], item["invocation"])
                                 for item in actual_outcomes}:
                raise AssertionError(f"{scenario['id']}: unused per-invocation outcome")
            edge_decisions = []
            for key, decision in state.decided.items():
                source_node, target_node = key.split("|", 1)
                edge_decisions.append({"from": source_node, "to": target_node,
                                       "selected": bool(decision[0])})
            edge_decisions.sort(key=lambda item: (item["from"], item["to"]))
            effective_input = state.input
            expected_input = python_runner.graph_module.merge_input(
                scenario["graph"].get("input", {}), run_input)
            if effective_input != expected_input:
                raise AssertionError(f"{scenario['id']}: run input merge mismatch")
            generated.append({
                "id": scenario["id"],
                "control": scenario.get("control", {}),
                "graph_snapshot": snapshot,
                "run_override": {"input": run_input},
                "effective_input": effective_input,
                "node_outcomes": actual_outcomes,
                "python": {
                    "status": mapped_status(state),
                    "python_status": state.status,
                    "reason": state.reason,
                    "ordered_executed_nodes": state.executed,
                    "skipped_nodes": state.skipped,
                    "passes": state.passes,
                    "ceased": state.ceased,
                    "edge_decisions": edge_decisions,
                    "cursor": ({"exists": state.cursor is not None,
                                "node": state.cursor["node"],
                                "pass": state.cursor["pass"],
                                "invocation": state.cursor["run"]}
                               if state.cursor else {"exists": False}),
                },
            })
    return {
        "format": 1,
        "source": "tests/fixtures/r5-python-oracle-scenarios.json via anchor.simple.graph.parse/to_dict and anchor.simple.run.run",
        "status_mapping": {
            "finished": "completed",
            "stopped with reason=budget_exhausted": "budget_stopped",
            "stopped otherwise": "stopped",
        },
        "scenarios": generated,
    }


def main() -> None:
    source = json.loads(SOURCE.read_text(encoding="utf-8"))
    result = generate(source)
    OUTPUT.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n",
                      encoding="utf-8")
    print(f"wrote {OUTPUT.relative_to(ROOT)} ({len(result['scenarios'])} scenarios)")


if __name__ == "__main__":
    main()
