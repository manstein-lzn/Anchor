"""Local runtime configuration for capability profiles.

Configuration contains references to secrets, never secret values.  The file is
intended for development and can be replaced by an operator-managed provider in
production.
"""

from __future__ import annotations

import json
from pathlib import Path

from pydantic import BaseModel, ConfigDict, Field

from anchor.runtime.capabilities import AgentCapability, ModelProfile, ToolCapability, VerifierCapability


class RuntimeConfig(BaseModel):
    model_config = ConfigDict(extra="forbid")

    models: list[ModelProfile] = Field(default_factory=list)
    agents: list[AgentCapability] = Field(default_factory=list)
    tools: list[ToolCapability] = Field(default_factory=list)
    verifiers: list[VerifierCapability] = Field(default_factory=list)
    secret_file: str | None = None


def load_runtime_config(path: str | Path | None = None) -> RuntimeConfig:
    from anchor.runtime.settings import AnchorSettings

    selected = Path(path or AnchorSettings().runtime_config).expanduser()
    try:
        return RuntimeConfig.model_validate_json(selected.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise RuntimeError(f"runtime config not found: {selected}") from exc
    except (OSError, ValueError) as exc:
        raise RuntimeError(f"runtime config is invalid: {selected}") from exc
