"""Regression evidence must distinguish real execution from missing coverage."""

import importlib.util
import json
from pathlib import Path
import sys

import pytest


SCRIPTS = Path(__file__).parents[1] / "scripts"
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location("rust_low_cost_regression", SCRIPTS / "rust_low_cost_regression.py")
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)
sys.path.pop(0)


def save(path: Path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value))


def attempt(state: Path, sequence: int, status: str, usage=None):
    root = state / "io-harness/store/invocation.recordings" / f"{sequence:020}"
    save(root / "outcome.json", {"status": status})
    if status == "succeeded":
        save(root / "recording.json", {"exchanges": [{"response": {"usage": usage}}]})
    return root


def test_usage_preserves_unreported_tokens_and_failed_attempts(tmp_path):
    state = tmp_path / "state"
    attempt(state, 1, "succeeded", {"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13})
    attempt(state, 2, "failed")
    attempt(state, 3, "succeeded")
    result = smoke.collect_usage(state)
    assert result["provider_attempts"] == 3
    assert result["usage_reported_attempts"] == 1
    assert result["reported_tokens"]["total_tokens"] == 13
    assert not result["usage_complete"]
    assert [item["status"] for item in result["attempts"]] == ["succeeded", "failed", "succeeded"]


def test_absent_usage_is_not_zero_cost(tmp_path):
    assert smoke.collect_usage(tmp_path)["reported_tokens"]["total_tokens"] is None
    attempt(tmp_path, 1, "succeeded")
    result = smoke.collect_usage(tmp_path)
    assert result["provider_attempts"] == 1
    assert result["reported_tokens"]["total_tokens"] is None
    assert not result["usage_complete"]


@pytest.mark.parametrize("case", smoke.CASES)
def test_live_bundle_keeps_short_real_agent_graph_unchanged(tmp_path, case):
    source = smoke.FIXTURES / f"{case}.json"
    original = source.read_bytes()
    graph, digest = smoke.prepare_bundle(case, tmp_path)
    assert (tmp_path / "bundle/graph.json").read_bytes() == original
    assert smoke.file_sha256(source) == digest
    assert all(agent["model"] == "models.regression" and not agent["network"]
               and agent["wall_time_limit_seconds"] == 60 for agent in graph["agents"].values())
    manifest = json.loads((tmp_path / "bundle/manifest.json").read_text())
    if case == "serial":
        assert manifest["plugins"][0]["id"] == "regression"
        assert manifest["plugins"][0]["mcp_servers"] == []
        assert "skills/check/SKILL.md" in manifest["plugins"][0]["resources"]
    else:
        assert manifest["plugins"] == []


def test_live_host_does_not_inherit_production_or_unrelated_secrets(tmp_path, monkeypatch):
    for name in smoke.MODEL_KEYS:
        monkeypatch.setenv(name, "configured")
    monkeypatch.setenv("ANCHOR_RUNNER_STATE_ROOT", "/production/state")
    monkeypatch.setenv("ANCHOR_RUNNER_ALLOWED_COMMANDS", "curl,python3")
    monkeypatch.setenv("ANCHOR_MODEL_ALIASES", '{"models.business":"unrelated"}')
    monkeypatch.setenv("WECOM_SECRET", "not-for-test")
    env = smoke.host_environment(tmp_path, "serial", 12345)
    assert "WECOM_SECRET" not in env
    assert env["ANCHOR_RUNNER_STATE_ROOT"] == str(tmp_path / "state")
    assert env["ANCHOR_RUNNER_ALLOWED_COMMANDS"] == "sh,cat,printf,cp,git,cmp"
    assert json.loads(env["ANCHOR_MODEL_ALIASES"]) == {"models.regression": "configured"}


def test_all_mode_missing_provider_is_failure_without_fixture_or_model_calls(monkeypatch):
    from anchor.runtime import secrets

    monkeypatch.setattr(secrets, "load_dotenv", lambda _path: None)
    for name in smoke.MODEL_KEYS:
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setattr(sys, "argv", ["rust_low_cost_regression.py", "--mode", "all"])
    with pytest.raises(SystemExit) as error:
        smoke.main()
    assert error.value.code == 2


def test_file_checks_require_workspace_and_immutable_artifact_bytes(tmp_path):
    key = {"run_id": "run", "graph_digest": "graph", "node_id": "worker", "invocation": 1}
    result = {"key": key, "commit": {"id": "artifact"}, "completion": {"route": None}}
    record = {"run_id": "run", "results": {"worker": [result]}}
    artifact = tmp_path / "state/artifacts/artifact"
    save(artifact / "manifest.json", {"key": key, "completion": result["completion"], "files": {
        "report.txt": {"sha256": smoke.hashlib.sha256(b"expected").hexdigest(), "bytes": 8},
    }})
    (artifact / "files").mkdir()
    (artifact / "files/report.txt").write_bytes(b"expected")
    workspace = tmp_path / "work/run" / smoke.invocation_digest(result)
    workspace.mkdir(parents=True)
    (workspace / "report.txt").write_bytes(b"expected")
    assert smoke.check_file(tmp_path, record, "worker", "report.txt", b"expected")["bytes"] == 8
    (workspace / "report.txt").write_bytes(b"wrong")
    with pytest.raises(smoke.SmokeFailure, match="Workspace bytes differ"):
        smoke.check_file(tmp_path, record, "worker", "report.txt", b"expected")
    (artifact / "files/report.txt").write_bytes(b"wrong")
    with pytest.raises(smoke.SmokeFailure, match="Artifact bytes differ"):
        smoke.check_file(tmp_path, record, "worker", "report.txt", b"expected")


def test_history_checks_reject_text_claims_without_actual_tools(tmp_path):
    graph = {"nodes": [{"id": "worker", "agent": "worker"}]}
    record = {"results": {"worker": [{"key": {"invocation": 1}}]}}
    detail = {"traces": {'["worker",1]': [{"role": "assistant", "text": "I wrote the files successfully"}]}}
    with pytest.raises(smoke.SmokeFailure, match="No real tool call"):
        smoke.check_histories(tmp_path, graph, record, detail)


def test_fixture_runner_rejects_success_without_scenario_evidence(tmp_path, monkeypatch):
    class Result:
        returncode = 0

    monkeypatch.setattr(smoke.subprocess, "run", lambda *_args, **_kwargs: Result())
    result = smoke.run_fixture(tmp_path, tmp_path / "target")
    assert result["status"] == "failed"
    assert result["scenario_evidence"] == 0


def test_fixture_runner_builds_native_tools_and_includes_composition(tmp_path, monkeypatch):
    commands = []

    class Result:
        returncode = 0

    def run(command, **_kwargs):
        commands.append(command)
        return Result()

    monkeypatch.setattr(smoke.subprocess, "run", run)
    smoke.run_fixture(tmp_path, tmp_path / "target")
    assert len(commands) == 2
    assert commands[0][1] == "build"
    assert "anchor-wecom-tools" in commands[0]
    assert "anchor-docmost-tools" in commands[0]
    assert "runtime_contract" in commands[1]
    assert commands[1][commands[1].index("--features") + 1] == "legacy-regression"
    assert "native_plugins" in commands[1]


def test_fixture_build_failure_does_not_use_stale_native_binaries(tmp_path, monkeypatch):
    commands = []

    class Result:
        returncode = 101

    def run(command, **_kwargs):
        commands.append(command)
        return Result()

    monkeypatch.setattr(smoke.subprocess, "run", run)
    result = smoke.run_fixture(tmp_path, tmp_path / "target")
    assert result["status"] == "failed"
    assert result["build_exit_code"] == 101
    assert len(commands) == 1
