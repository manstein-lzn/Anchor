from pathlib import Path

import pytest

from anchor.runtime.config import load_runtime_config


def test_runtime_config_loads_profile_without_secret(tmp_path: Path):
    path = tmp_path / "runtime.json"
    path.write_text(
        '{"secret_file":"/tmp/auth.json","models":[{"ref":"models.codex","provider":"rightcode",'
        '"model":"gpt-6-astra","base_url":"https://api.a6api.com/v1",'
        '"wire_api":"responses","secret_ref":"OPENAI_API_KEY"}],"agents":[{"ref":"agents.a",'
        '"model_ref":"models.codex","instructions":"be concise"}],"tools":[],"verifiers":['
        '{"ref":"verifiers.evidence","version":"v2","adapter":"model","model_ref":"models.codex"}]}',
        encoding="utf-8",
    )
    config = load_runtime_config(path)
    assert config.models[0].model == "gpt-6-astra"
    assert config.models[0].secret_ref == "OPENAI_API_KEY"
    assert config.agents[0].ref == "agents.a"
    assert config.verifiers[0].ref == "verifiers.evidence"
    assert config.verifiers[0].version == "v2"


def test_runtime_config_missing_file_fails_closed(tmp_path: Path):
    with pytest.raises(RuntimeError, match="not found"):
        load_runtime_config(tmp_path / "missing.json")


def test_settings_reads_typed_env_and_fails_fast_on_database(monkeypatch):
    from anchor.runtime.settings import AnchorSettings

    monkeypatch.setenv("ANCHOR_DATABASE_URL", "sqlite:////tmp/x.sqlite")
    monkeypatch.setenv("ANCHOR_WORKER_INTERVAL", "2.5")
    settings = AnchorSettings()
    assert settings.require_database_url() == "sqlite:////tmp/x.sqlite"
    assert settings.worker_interval == 2.5
    assert settings.worker_id == "anchor-worker"


def test_settings_missing_database_fails_with_historical_message(monkeypatch):
    from anchor.runtime.settings import AnchorSettings

    monkeypatch.delenv("ANCHOR_DATABASE_URL", raising=False)
    with pytest.raises(RuntimeError, match="set ANCHOR_DATABASE_URL explicitly"):
        AnchorSettings().require_database_url()


def test_settings_rejects_malformed_interval(monkeypatch):
    from anchor.runtime.settings import AnchorSettings

    monkeypatch.setenv("ANCHOR_WORKER_INTERVAL", "not-a-number")
    with pytest.raises(Exception):
        AnchorSettings()
