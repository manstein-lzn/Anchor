"""Runtime-only secret resolution.

Secrets are referenced by name in configuration.  They are never represented
in graph definitions, events, API payloads, or model response objects.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Mapping, Protocol


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

