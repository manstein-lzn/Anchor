"""Runtime-only secret resolution.

Secrets are referenced by name in configuration.  They are never represented
in graph definitions, events, API payloads, or model response objects.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Mapping, Protocol


def load_dotenv(path: str | Path | None = None) -> Path | None:
    """Load simple KEY=VALUE lines without overriding explicit environment variables."""
    candidate = Path(path) if path else Path.cwd() / ".env"
    if not candidate.is_file():
        return None
    for line in candidate.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = (part.strip() for part in line.split("=", 1))
        if not key or any(ch not in "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_" for ch in key):
            continue
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        if value and value not in ("[]", "{}"):
            os.environ.setdefault(key, value)
    return candidate


def env_model_profile() -> dict | None:
    """Return the single model configured by .env, when all required values exist."""
    url, key = os.environ.get("ANCHOR_MODEL_URL"), os.environ.get("ANCHOR_MODEL_API_KEY")
    if not url or not key:
        return None
    return {"ref": "models.default", "model": os.environ.get("ANCHOR_MODEL_NAME", "default"),
            "base_url": url, "wire_api": os.environ.get("ANCHOR_MODEL_WIRE_API", "responses"),
            "secret_ref": "MODEL_API_KEY",
            "context_window": int(os.environ.get("ANCHOR_MODEL_CONTEXT_WINDOW", "0") or 0)}


def env_model_profiles() -> dict[str, dict]:
    """Resolve optional model-name aliases on the same operator-configured endpoint."""
    default = env_model_profile()
    if default is None:
        return {}
    aliases = json.loads(os.environ.get("ANCHOR_MODEL_ALIASES", "{}") or "{}")
    if not isinstance(aliases, dict):
        raise ValueError("ANCHOR_MODEL_ALIASES must be a JSON object")
    profiles = {default["ref"]: {**default, "fallback_for_unknown_refs": True}}
    for ref, model in aliases.items():
        if (not isinstance(ref, str) or not ref.startswith("models.") or ref == "models.default"
                or not isinstance(model, str) or not model.strip()):
            raise ValueError("model aliases require a non-default models.* name and a model name")
        profiles[ref] = {**default, "ref": ref, "model": model.strip()}
    return profiles


class SecretProvider(Protocol):
    def get(self, name: str) -> str: ...


class SecretUnavailable(RuntimeError):
    """Raised when a named secret is absent or the source is unsafe."""


class EnvironmentSecretProvider:
    """Resolve names from an allow-listed environment mapping."""

    def __init__(self, values: Mapping[str, str] | None = None, *, prefix: str = "ANCHOR_SECRET_") -> None:
        self._values = values if values is not None else os.environ
        self._prefix = prefix

    def get(self, name: str) -> str:
        if not name or any(ch not in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_" for ch in name):
            raise SecretUnavailable("invalid secret name")
        value = self._values.get(f"{self._prefix}{name}")
        if not value:
            raise SecretUnavailable(f"secret is unavailable: {name}")
        return value


class JsonFileSecretProvider:
    """Read a root-owned-by-user JSON object with strict permission checks."""

    def __init__(self, path: str | Path) -> None:
        self.path = Path(path).expanduser()

    def get(self, name: str) -> str:
        try:
            mode = self.path.stat().st_mode & 0o777
        except OSError as exc:
            raise SecretUnavailable("secret file is unavailable") from exc
        if mode & 0o077:
            raise SecretUnavailable("secret file permissions must be owner-only")
        try:
            payload = json.loads(self.path.read_text(encoding="utf-8"))
            value = payload.get(name)
        except (OSError, ValueError, AttributeError) as exc:
            raise SecretUnavailable("secret file cannot be read") from exc
        if not isinstance(value, str) or not value:
            raise SecretUnavailable(f"secret is unavailable: {name}")
        return value


class ChainedSecretProvider:
    """Try providers in order while preserving a uniform failure surface."""

    def __init__(self, *providers: SecretProvider) -> None:
        self.providers = providers

    def get(self, name: str) -> str:
        for provider in self.providers:
            try:
                return provider.get(name)
            except SecretUnavailable:
                continue
        raise SecretUnavailable(f"secret is unavailable: {name}")
