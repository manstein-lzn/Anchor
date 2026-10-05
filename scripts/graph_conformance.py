#!/usr/bin/env python3
"""Generate and check Python Graph parser goldens for Rust conformance work.

From the repository root, run ``./.venv/bin/python scripts/graph_conformance.py`` to
check the committed baseline, or add ``--update`` to regenerate it from the named
authoring examples. The fixture retains each complete authoring object (including
editor-only fields) alongside the Python parser's expanded graph snapshot.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from anchor.simple import graph as graph_module  # noqa: E402

OUTPUT = ROOT / "tests/fixtures/graph-conformance/python-v1.json"
PARSER_SOURCE = ROOT / "src/anchor/simple/graph.py"

# These examples deliberately cover independent authoring features while keeping
# each comparison useful as a small, reviewable Rust from_authoring case.
CASE_NAMES = (
    "academic-simple",       # ordinary agent graph and inferred entry
    "revise-loop",           # feedback edge and per-node ceilings
    "parallel-audit",        # paired fanout/join region
    "survey-modular",        # nested graph expansion with a feedback loop
    "plugin-research",       # AgentNode Plugin binding
    "deep-academic-research",  # authoring-only layout plus Plugin/call-free feedback graph
    "call-report",           # Op.call authoring and its static target
    "call-worker",           # call target graph, bundled beside its caller
)

def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _canonical(value: Any) -> Any:
    """Sort object keys recursively while preserving all author-declared array order."""
    if isinstance(value, dict):
        return {key: _canonical(value[key]) for key in sorted(value)}
    if isinstance(value, list):
        return [_canonical(item) for item in value]
    return value


def _pointer_part(value: str) -> str:
    return value.replace("~", "~0").replace("/", "~1")


def _field_paths(value: Any, path: str = "") -> dict[str, list[str]]:
    paths: dict[str, list[str]] = {}

    def visit(item: Any, current: str) -> None:
        if isinstance(item, dict):
            for key, child in item.items():
                child_path = f"{current}/{_pointer_part(key)}"
                paths.setdefault(key, []).append(child_path)
                visit(child, child_path)
        elif isinstance(item, list):
            for index, child in enumerate(item):
                visit(child, f"{current}/{index}")

    visit(value, path)
    return {key: sorted(found) for key, found in sorted(paths.items())}


def _summary(graph: graph_module.Graph, expanded: dict[str, Any]) -> dict[str, Any]:
    ordered_edges = graph_module.edges(graph)
    feedback = graph_module.back_edges(graph)
    nodes = []
    for node_id, node in graph.nodes.items():
        nodes.append({
            "id": node_id,
            "kind": "op" if node.op else "agent",
            **({"op": node.op} if node.op else {"agent": node.agent}),
            "reads": list(graph.reads(node_id)),
            "writes": list(graph.writes(node_id)),
            "plugins": list(node.plugins),
            "with": node.with_,
            "max_rounds": graph.ceiling(node_id),
        })

    calls = []
    for node_id, node in graph.nodes.items():
        if not node.op:
            continue
        op = graph.ops[node.op]
        if op.call is not None:
            calls.append({"node": node_id, "op": node.op, "call": _canonical(op.call)})

    regions = graph.parallel_regions()
    return {
        "objective": graph.objective,
        "input": _canonical(graph.input),
        "entry": graph.entry(),
        "node_order": list(graph.nodes),
        "nodes": nodes,
        "edges": [{"from": source, "to": target} for source, target in ordered_edges],
        "feedback_edges": [
            {"from": source, "to": target}
            for source, target in ordered_edges if (source, target) in feedback
        ],
        "max_rounds": _canonical(graph.max_rounds),
        "module_rounds": _canonical(graph.module_rounds),
        "parallel_regions": [
            {"fanout": region.fanout, "join": region.join,
             "branches": [list(branch) for branch in region.branches]}
            for region in regions.values()
        ],
        "plugins": [
            {"node": node.id, "plugins": list(node.plugins)}
            for node in graph.nodes.values() if node.plugins
        ],
        "calls": calls,
        "expanded_graph_keys": sorted(expanded),
    }


def _call_targets(raw: dict[str, Any], sources: dict[str, tuple[str, bytes]]) -> dict[str, Any]:
    targets: dict[str, Any] = {}
    for op_name, op_spec in (raw.get("ops") or {}).items():
        call = op_spec.get("call")
        if not isinstance(call, dict):
            continue
        target = call.get("graph")
        if not isinstance(target, str):
            continue
        source = sources.get(target)
        if source is None:
            continue
        relative_path, raw_bytes = source
        targets[target] = {"source_path": relative_path, "source_sha256": _sha256(raw_bytes),
                           "op": op_name}
    return targets


def generate() -> dict[str, Any]:
    sources: dict[str, tuple[str, bytes]] = {}
    for name in CASE_NAMES:
        path = ROOT / "examples/graphs" / f"{name}.json"
        if not path.is_file():
            raise ValueError(f"missing Graph conformance authoring input: {path.relative_to(ROOT)}")
        sources[name] = (path.relative_to(ROOT).as_posix(), path.read_bytes())

    cases = []
    for name in CASE_NAMES:
        relative_path, raw_bytes = sources[name]
        try:
            authoring = json.loads(raw_bytes)
            parsed = graph_module.parse(authoring)
        except (OSError, ValueError, TypeError) as exc:
            raise ValueError(f"{relative_path}: Python Graph parser failed: {type(exc).__name__}: {exc}") from exc
        expanded = graph_module.to_dict(parsed)
        fields = _field_paths(authoring)
        cases.append({
            "id": name,
            "source_path": relative_path,
            "source_sha256": _sha256(raw_bytes),
            "authoring_graph": _canonical(authoring),
            "authoring_field_paths": fields,
            "call_targets": _call_targets(authoring, sources),
            "expanded_graph": _canonical(expanded),
            "summary": _summary(parsed, expanded),
        })

    parser_bytes = PARSER_SOURCE.read_bytes()
    return {
        "format": 1,
        "provenance": {
            "parser": "anchor.simple.graph.parse + anchor.simple.graph.to_dict",
            "parser_source_path": PARSER_SOURCE.relative_to(ROOT).as_posix(),
            "parser_source_sha256": _sha256(parser_bytes),
            "python_implementation": sys.implementation.name,
            "python_version": f"{sys.version_info.major}.{sys.version_info.minor}",
        },
        "cases": cases,
    }


def _first_difference(expected: Any, actual: Any, path: str = "") -> tuple[str, Any, Any] | None:
    if type(expected) is not type(actual):
        return path or "/", expected, actual
    if isinstance(expected, dict):
        expected_keys, actual_keys = set(expected), set(actual)
        if expected_keys != actual_keys:
            key = sorted(expected_keys ^ actual_keys)[0]
            return f"{path}/{_pointer_part(key)}", expected.get(key, "<missing>"), actual.get(key, "<missing>")
        for key in sorted(expected):
            difference = _first_difference(expected[key], actual[key], f"{path}/{_pointer_part(key)}")
            if difference:
                return difference
        return None
    if isinstance(expected, list):
        if len(expected) != len(actual):
            return f"{path}/length", len(expected), len(actual)
        for index, (old, new) in enumerate(zip(expected, actual, strict=True)):
            difference = _first_difference(old, new, f"{path}/{index}")
            if difference:
                return difference
        return None
    if expected != actual:
        return path or "/", expected, actual
    return None


def _display(value: Any, limit: int = 240) -> str:
    rendered = json.dumps(value, ensure_ascii=False, sort_keys=True)
    return rendered if len(rendered) <= limit else rendered[:limit - 3] + "..."


def check_baseline(expected: dict[str, Any], actual: dict[str, Any]) -> str | None:
    difference = _first_difference(expected, actual)
    if difference is None:
        return None
    path, old, new = difference
    return (f"Python Graph conformance baseline differs at {path}\n"
            f"  fixture: {_display(old)}\n"
            f"  current: {_display(new)}\n"
            "Review the parser/input change, then regenerate with "
            "`./.venv/bin/python scripts/graph_conformance.py --update`.")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--update", action="store_true", help="write a fresh Python baseline")
    mode.add_argument("--check", action="store_true", help="compare against the committed baseline (default)")
    args = parser.parse_args(argv)

    try:
        current = generate()
    except (OSError, ValueError, TypeError) as exc:
        print(f"Graph conformance generation failed: {exc}", file=sys.stderr)
        return 2

    if args.update:
        OUTPUT.parent.mkdir(parents=True, exist_ok=True)
        OUTPUT.write_text(json.dumps(current, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(f"wrote {OUTPUT.relative_to(ROOT)} ({len(current['cases'])} Python parser cases)")
        return 0

    if not OUTPUT.is_file():
        print(f"Python Graph baseline is missing: {OUTPUT.relative_to(ROOT)}; run with --update",
              file=sys.stderr)
        return 2
    try:
        expected = json.loads(OUTPUT.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError, UnicodeError) as exc:
        print(f"cannot read {OUTPUT.relative_to(ROOT)}: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 2
    difference = check_baseline(expected, current)
    if difference:
        print(difference, file=sys.stderr)
        return 1
    print(f"Python Graph conformance baseline matches ({len(current['cases'])} cases)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
