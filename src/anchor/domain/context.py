"""Canonical JSON helpers for durable execution context."""

from __future__ import annotations

import hashlib
import json
from typing import Any, Mapping


def canonical_json(value: Any) -> str:
    """Serialize JSON-compatible context deterministically for hashing."""
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False)


def input_hash(snapshot: Mapping[str, Any]) -> str:
    return hashlib.sha256(canonical_json(snapshot).encode("utf-8")).hexdigest()
