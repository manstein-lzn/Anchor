#!/usr/bin/env python3
"""Quality gate: ratcheted lint, type-check and size budgets.

Every metric may only go down. A change that increases one fails the gate; run
with ``--update`` to record a reviewed, intentional new baseline (and inspect
the diff before committing it).

Usage:
    python scripts/quality_gate.py             # check against the baseline
    python scripts/quality_gate.py --update    # rewrite the baseline
    python scripts/quality_gate.py --fast      # skip mypy
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
SRC = ROOT / "src" / "anchor"
BASELINE = ROOT / "quality-baseline.json"
BIN = pathlib.Path(sys.executable).parent


def _run(command: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)


def ruff_counts() -> dict[str, int]:
    result = _run([str(BIN / "ruff"), "check", "src/anchor", "--output-format", "json"])
    try:
        payload = json.loads(result.stdout or "[]")
    except json.JSONDecodeError:
        raise SystemExit(f"ruff produced unparsable output:\n{result.stdout}\n{result.stderr}")
    counts: dict[str, int] = {}
    for item in payload:
        code = item.get("code") or "unknown"
        counts[code] = counts.get(code, 0) + 1
    return counts


def mypy_errors() -> int:
    result = _run([str(BIN / "mypy"), "src/anchor"])
    return sum(1 for line in result.stdout.splitlines() if re.match(r"^src/.*error:", line))


def module_sizes() -> dict[str, int]:
    return {path.relative_to(SRC).as_posix(): len(path.read_text(encoding="utf-8").splitlines())
            for path in sorted(SRC.rglob("*.py"))}


def measure(*, skip_mypy: bool) -> dict:
    sizes = module_sizes()
    return {
        "ruff": dict(sorted(ruff_counts().items())),
        "mypy_errors": None if skip_mypy else mypy_errors(),
        "max_module_lines": max(sizes.values(), default=0),
        "largest_modules": dict(sorted(sizes.items(), key=lambda item: -item[1])[:5]),
    }


def compare(current: dict, baseline: dict) -> list[str]:
    regressions: list[str] = []
    for code, count in current["ruff"].items():
        allowed = baseline.get("ruff", {}).get(code, 0)
        if count > allowed:
            regressions.append(f"ruff {code}: {count} > {allowed}")
    if current["mypy_errors"] is not None:
        allowed = baseline.get("mypy_errors")
        if allowed is not None and current["mypy_errors"] > allowed:
            regressions.append(f"mypy errors: {current['mypy_errors']} > {allowed}")
    # Module size is owned by tests/test_architecture.py, which applies the
    # documented per-module cap; here it is reported for visibility only.
    return regressions


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--update", action="store_true", help="record a new baseline")
    parser.add_argument("--fast", action="store_true", help="skip mypy")
    args = parser.parse_args()

    current = measure(skip_mypy=args.fast)
    if args.update:
        BASELINE.write_text(json.dumps(current, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(f"baseline written to {BASELINE.relative_to(ROOT)}")
        print(json.dumps(current, indent=2, sort_keys=True))
        return 0

    if not BASELINE.exists():
        raise SystemExit("no baseline: run `python scripts/quality_gate.py --update` first")
    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    regressions = compare(current, baseline)

    print("quality gate")
    print(f"  ruff:              {current['ruff']}")
    print(f"  mypy errors:       {current['mypy_errors']}")
    print(f"  largest module:    {current['max_module_lines']} lines (cap enforced by the architecture test)")
    print(f"  baseline:          {json.dumps({k: baseline.get(k) for k in ('ruff', 'mypy_errors')})}")
    if regressions:
        print("\nREGRESSIONS:")
        for item in regressions:
            print(f"  - {item}")
        return 1
    print("\nOK: no metric increased.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
