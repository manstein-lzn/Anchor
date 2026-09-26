"""Shared test fixtures.

The fault-injection evidence is the one thing here worth sharing: producing it means running a script
that really kills processes at every recovery window, and it takes about fifty seconds. Assertions read
only its semantic fields — verdicts, counters, events, effects — so one cached copy serves every test
that asks about it, and one copy per machine serves every parallel worker.
"""
from __future__ import annotations

import fcntl
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "recovery_windows.py"


def _evidence_path() -> Path:
    """Keyed by the script's contents, so editing it discards the stale copy."""
    digest = hashlib.sha256(SCRIPT.read_bytes()).hexdigest()[:16]
    return Path(tempfile.gettempdir()) / f"anchor-recovery-evidence-{digest}.json"


@pytest.fixture(scope="session")
def killed() -> dict:
    """Every recovery window, run once per machine with real kills."""
    out = _evidence_path()
    with open(out.with_suffix(".lock"), "w", encoding="utf-8") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        if not out.exists():
            root = Path(tempfile.mkdtemp(prefix="anchor-recovery-"))
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--root", str(root), "--json", str(out),
                 "--timeout", "120"],
                capture_output=True, text=True, timeout=1800, cwd=str(ROOT))
            assert out.exists(), f"the fault script produced no evidence:\n{result.stdout}\n{result.stderr}"
        return {item["window"]: item for item in json.loads(out.read_text(encoding="utf-8"))}
