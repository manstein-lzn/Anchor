"""A service must refuse to start against a broken environment, and say which part.

Both failure modes this guards against look healthy from outside: a crash-looping unit that
systemd reports as ``activating``, and a unit that runs and silently does nothing. So the
tests here are mostly about the *diagnosis* — that each way an environment can be wrong is
reported as itself, and that a mistyped path is never reported as a missing migration.
"""

from __future__ import annotations

import json

import pytest

from anchor.runtime.preflight import (
    ENVIRONMENT_EXIT_CODE,
    check_artifacts,
    check_database,
    check_environment,
    check_runtime_config,
    require_environment,
)

from conftest import make_store


def codes(problems) -> list[str]:
    return [problem.code for problem in problems]


def test_a_missing_url_is_not_guessed(tmp_path):
    """It used to fall back to a developer's local file, which meant a misconfigured unit
    wrote somewhere nobody was looking and reported nothing wrong."""
    assert codes(check_database(None)) == ["database_url_missing"]
    assert codes(check_database("")) == ["database_url_missing"]


def test_an_unopenable_database_is_unreachable_not_unmigrated(tmp_path):
    """Telling an operator to run migrations against a path they mistyped sends them the
    wrong way, so the two are separated before the schema is read."""
    problems = check_database(f"sqlite:///{tmp_path}/no/such/dir/x.sqlite")
    assert codes(problems) == ["database_unreachable"]
    assert "reachable" in problems[0].detail


def test_a_reachable_but_empty_database_needs_migrating(tmp_path):
    problems = check_database(f"sqlite:///{tmp_path}/empty.sqlite")
    assert codes(problems) == ["schema_not_migrated"]
    assert "alembic upgrade head" in problems[0].detail


def test_a_migrated_database_passes(tmp_path):
    store = make_store(tmp_path)
    store.close()
    assert check_database(f"sqlite:///{tmp_path}/anchor.sqlite") == []


def test_a_missing_runtime_profile_is_reported_as_missing(tmp_path):
    assert codes(check_runtime_config(str(tmp_path / "absent.json"))) == \
        ["runtime_config_missing"]


def test_a_profile_without_models_is_reported_as_empty(tmp_path):
    """A profile that parses but declares nothing means no node could ever be executed —
    a service that started anyway would sit idle and look healthy."""
    profile = tmp_path / "runtime.json"
    profile.write_text(json.dumps({"models": [], "agents": [], "tools": [],
                                   "verifiers": []}), encoding="utf-8")
    assert codes(check_runtime_config(str(profile))) == ["runtime_config_empty"]


def test_an_invalid_profile_is_reported_as_invalid(tmp_path):
    profile = tmp_path / "runtime.json"
    profile.write_text("{ not json", encoding="utf-8")
    problems = check_runtime_config(str(profile))
    assert codes(problems) == ["runtime_config_invalid"]


def test_a_valid_profile_passes(tmp_path):
    profile = tmp_path / "runtime.json"
    profile.write_text(json.dumps({
        "models": [{"ref": "models.test", "provider": "rightcode", "model": "m",
                    "secret_ref": "K"}],
        "agents": [], "tools": [], "verifiers": []}), encoding="utf-8")
    assert check_runtime_config(str(profile)) == []


def test_an_unwritable_artifact_root_is_reported(tmp_path):
    blocker = tmp_path / "file-not-dir"
    blocker.write_text("x", encoding="utf-8")
    problems = check_artifacts(str(blocker / "artifacts"))
    assert codes(problems) == ["artifact_root_unwritable"]


def test_a_writable_artifact_root_passes_and_is_created(tmp_path):
    root = tmp_path / "deep" / "artifacts"
    assert check_artifacts(str(root)) == []
    assert root.is_dir()


def test_checks_run_in_the_order_an_operator_would_fix_them(tmp_path):
    """Database first: nothing else can be judged until it is readable."""
    problems = check_environment(role="x", database_url=None,
                                 runtime_config=str(tmp_path / "absent.json"),
                                 artifact_root=str(tmp_path / "a"))
    assert codes(problems)[0] == "database_url_missing"


def test_require_environment_exits_two_with_a_structured_reason(capsys, tmp_path):
    with pytest.raises(SystemExit) as caught:
        require_environment(role="agent_worker", database_url=None)
    assert caught.value.code == ENVIRONMENT_EXIT_CODE

    report = json.loads(capsys.readouterr().err.strip())
    assert report["role"] == "agent_worker"
    assert report["status"] == "environment_not_ready"
    assert report["problems"][0]["code"] == "database_url_missing"


def test_require_environment_returns_quietly_when_ready(tmp_path):
    store = make_store(tmp_path)
    store.close()
    require_environment(role="agent_worker",
                        database_url=f"sqlite:///{tmp_path}/anchor.sqlite")
