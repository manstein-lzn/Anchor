import json
import os
import stat
from pathlib import Path

import pytest

from anchor.runtime.capabilities import (AgentCapability, CapabilityRegistry, CapabilityRegistryError,
                                          ModelProfile, ToolCapability, VerifierCapability)
from anchor.runtime.secrets import (ChainedSecretProvider, EnvironmentSecretProvider, JsonFileSecretProvider,
                                    SecretUnavailable)


def test_registry_validates_agent_dependencies_without_secrets():
    registry = CapabilityRegistry(
        models=[ModelProfile(ref="models.codex", provider="rightcode", model="gpt-6-astra", secret_ref="CODEX")],
        agents=[AgentCapability(ref="agents.researcher", model_ref="models.codex", tool_refs=["tools.search"])],
        tools=[ToolCapability(ref="tools.search")],
    )
    assert registry.validate_agent("agents.researcher").model_ref == "models.codex"
    assert registry.snapshot()["models"] == ("models.codex",)


def test_registry_rejects_missing_dependencies():
    registry = CapabilityRegistry(models=[ModelProfile(ref="m", provider="x", model="y", secret_ref="s")],
                                  agents=[AgentCapability(ref="a", model_ref="m", tool_refs=["missing"])])
    with pytest.raises(CapabilityRegistryError, match="unknown tool"):
        registry.validate_agent("a")


def test_registry_validates_deterministic_and_model_verifiers():
    model = ModelProfile(ref="m", provider="x", model="y", secret_ref="s")
    deterministic = VerifierCapability(ref="rules", adapter="deterministic", expression="context.ok")
    model_verifier = VerifierCapability(ref="judge", adapter="model", model_ref="m")
    registry = CapabilityRegistry(models=[model], verifiers=[deterministic, model_verifier])
    assert registry.validate_verifier("rules").expression == "context.ok"
    assert registry.validate_verifier("judge").model_ref == "m"
    assert registry.snapshot()["verifiers"] == ("judge", "rules")


def test_verifier_configuration_fails_closed():
    with pytest.raises(ValueError, match="requires expression"):
        VerifierCapability(ref="rules", adapter="deterministic")
    with pytest.raises(ValueError, match="requires model_ref"):
        VerifierCapability(ref="judge", adapter="model")
    registry = CapabilityRegistry(verifiers=[
        VerifierCapability(ref="judge", adapter="model", model_ref="missing"),
    ])
    with pytest.raises(CapabilityRegistryError, match="unknown model"):
        registry.validate_verifier("judge")


def test_environment_provider_and_chain():
    provider = ChainedSecretProvider(EnvironmentSecretProvider({"ANCHOR_SECRET_TEST": "value"}))
    assert provider.get("TEST") == "value"
    with pytest.raises(SecretUnavailable):
        provider.get("MISSING")


def test_json_file_provider_requires_owner_only(tmp_path: Path):
    path = tmp_path / "auth.json"
    path.write_text(json.dumps({"OPENAI_API_KEY": "secret"}), encoding="utf-8")
    path.chmod(stat.S_IRUSR | stat.S_IWUSR)
    assert JsonFileSecretProvider(path).get("OPENAI_API_KEY") == "secret"
    path.chmod(0o644)
    with pytest.raises(SecretUnavailable, match="permissions"):
        JsonFileSecretProvider(path).get("OPENAI_API_KEY")
