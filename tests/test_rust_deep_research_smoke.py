"""Evidence collection must not turn partial deep research into acceptance."""

import importlib.util
import json
from pathlib import Path
import sqlite3


SPEC = importlib.util.spec_from_file_location(
    "rust_deep_research_smoke", Path(__file__).parents[1] / "scripts/rust_deep_research_smoke.py"
)
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)


def store(state: Path) -> Path:
    root = state / "io-harness/store"
    root.mkdir(parents=True)
    path = root / "fixture.sqlite3"
    with sqlite3.connect(path) as db:
        db.executescript("""
            CREATE TABLE provider_calls (id INTEGER, step INTEGER, attempt INTEGER,
                                         model TEXT, finish_reason TEXT, failure TEXT);
            CREATE TABLE step_turns (step INTEGER, calls TEXT);
            CREATE TABLE ledger_observations (id INTEGER, step INTEGER, target TEXT, text TEXT);
        """)
        db.execute("INSERT INTO provider_calls VALUES (1, 1, 0, 'real-model', 'tool_calls', NULL)")
        db.execute("INSERT INTO step_turns VALUES (?, ?)", (1, json.dumps([
            {"name": "anchor_run", "arguments": {"command": ["/tools/scholarly/run", "search"]}},
        ])))
        db.execute("INSERT INTO ledger_observations VALUES (1, 1, 'anchor_run', ?)", (
            '\n[anchor_run]\n{"status":"completed","exit_code":0,"stdout":"{}"}',
        ))
    return path


def test_collect_preserves_partial_run_and_provider_evidence(tmp_path):
    state = tmp_path / "state"
    run = state / "runs/deep-academic-research.json"
    run.parent.mkdir(parents=True)
    record = {"status": "running", "results": {"frame": [{"commit": {"id": "first"}}]}}
    run.write_text(json.dumps(record))
    original = run.read_bytes()
    store(state)
    result = smoke.collect(tmp_path, {"status": "timeout", "timeout_seconds": 900})
    assert result["runtime_closure"] == "not completed"
    assert result["run_status"] == "running"
    assert result["node_passes"] == {"frame": 1}
    assert result["provider_requests"] == 1
    assert result["provider_model_observed"] == ["real-model"]
    assert result["tool_call_counts"] == {"anchor_run": 1}
    assert result["research_quality"] == "not accepted by this mechanical collector"
    assert run.read_bytes() == original
    assert json.loads((tmp_path / "evidence.json").read_text())["status"] == "timeout"


def test_completed_status_without_committed_business_nodes_is_rejected(tmp_path):
    failures = smoke._completed_checks(tmp_path, {"results": {}}, {"provider_calls": [], "tool_calls": []})
    assert failures == ["Missing committed nodes: frame, investigate, challenge, feedback, synthesize, review, review-gate, report"]


def test_trace_uses_native_provider_schema_and_observations(tmp_path):
    state = tmp_path / "state"
    store(state)
    trace = smoke._provider_trace(state)
    assert trace["provider_calls"][0] == {
        "store": str(state / "io-harness/store/fixture.sqlite3"), "id": 1, "step": 1, "attempt": 0,
        "model": "real-model", "finish_reason": "tool_calls", "failure": None,
    }
    assert trace["observations"][0]["step"] == 1
    assert '"status":"completed"' in trace["observations"][0]["text"]


def test_recovery_contract_is_read_only_and_same_run(tmp_path):
    record = {
        "run_id": "deep-academic-research",
        "graph_digest": "digest",
        "status": "running",
        "cursor": {"key": {
            "run_id": "deep-academic-research", "graph_digest": "digest",
            "node_id": "investigate", "invocation": 1,
        }},
    }
    contract = smoke._inspect_recovery_contract(tmp_path, record)
    assert contract["read_only"] is True
    assert contract["resume_invoked"] is False
    assert contract["same_run_id"] is True
    assert contract["same_graph_digest"] is True
    assert "POST /runs/{run}/resume" in contract["http_protocol"]
    assert "must not replay" in contract["unknown_effect_policy"]
