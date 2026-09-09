"""Architecture gates: layering, boundary ownership and anti-pattern ratchets.

These tests encode rules that would otherwise decay into convention. They are
deliberately ratcheted: a count may go down freely, but any increase fails, so a
new violation cannot be added silently. Update an allowlist only with an ADR or
an explicit review.
"""

from __future__ import annotations

import ast
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src" / "anchor"

# Layer index: a module may import its own layer or lower layers, never higher.
LAYERS = {"domain": 0, "state": 1, "runtime": 2, "api": 3}
# Top-level modules that are composition roots and may import anything.
FREE_TOP_LEVEL = {"client.py", "cli.py", "__init__.py"}

# Content-reference prefixes must be parsed in one place, not sprinkled around.
CONTENT_REF_PREFIXES = ("artifact://", "workspace://")
CONTENT_REF_OWNERS = {
    "domain/content.py",      # the boundary type itself
    "runtime/content.py",     # the resolver boundary
    "runtime/workspace.py",   # the workspace backend
    "runtime/artifacts.py",   # the artifact store
    "state/storage.py",       # reference scanning for the storage report
    "runtime/integrity.py",   # evidence readability check
    "runtime/resolution.py",  # predecessor artifact resolution
    "runtime/verifier.py",    # verified-artifact binding
    "api/app.py",             # artifact read endpoint
}

# Stringly-typed node dispatch is a ratchet: new code must use the registry.
NODE_TYPE_DISPATCH_ALLOWED = {
    "api/app.py": 4,
    "state/checkpoints.py": 1,
}

# Module size budget. Anything above the cap must be split before merging.
# api/app.py is the composition root that registers every route; it carries a
# larger documented budget and should be split by router when it grows further.
MODULE_LINE_CAP = 600
MODULE_LINE_EXEMPT = {"api/app.py": 750}


def _modules() -> list[Path]:
    return sorted(SRC.rglob("*.py"))


def _relative(path: Path) -> str:
    return path.relative_to(SRC).as_posix()


def _tree(path: Path) -> ast.Module:
    return ast.parse(path.read_text(encoding="utf-8"))


def _imported_top_level(path: Path) -> set[str]:
    """Top-level ``anchor.<layer>`` names imported by a module."""
    names: set[str] = set()
    for node in ast.walk(_tree(path)):
        if isinstance(node, ast.ImportFrom) and node.module and node.module.startswith("anchor."):
            names.add(node.module.split(".")[1])
        elif isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name.startswith("anchor."):
                    names.add(alias.name.split(".")[1])
    return names


def test_every_module_belongs_to_a_known_layer():
    unknown = {_relative(path).split("/")[0] for path in _modules()} - set(LAYERS) - FREE_TOP_LEVEL
    assert not unknown, f"unexpected top-level modules: {sorted(unknown)}"


def test_dependencies_only_point_downward():
    violations: list[str] = []
    for path in _modules():
        top = _relative(path).split("/")[0]
        if top not in LAYERS:
            continue
        for imported in _imported_top_level(path):
            if imported in LAYERS and LAYERS[imported] > LAYERS[top]:
                violations.append(f"{_relative(path)} imports anchor.{imported}")
    assert not violations, "layer violations: " + "; ".join(violations)


def test_content_reference_prefixes_stay_in_the_boundary():
    violations: list[str] = []
    for path in _modules():
        relative = _relative(path)
        if relative in CONTENT_REF_OWNERS:
            continue
        text = path.read_text(encoding="utf-8")
        for prefix in CONTENT_REF_PREFIXES:
            if prefix in text:
                violations.append(f"{relative} contains {prefix!r}")
    assert not violations, (
        "content references must be parsed through the boundary type: "
        + "; ".join(violations))


def test_node_type_dispatch_is_ratcheted():
    violations: list[str] = []
    for path in _modules():
        relative = _relative(path)
        count = sum(path.read_text(encoding="utf-8").count(marker)
                    for marker in ("type.value ==", "type.value !="))
        allowed = NODE_TYPE_DISPATCH_ALLOWED.get(relative, 0)
        if count > allowed:
            violations.append(f"{relative}: {count} > {allowed}")
    assert not violations, (
        "stringly-typed node dispatch must use the behavior registry instead: "
        + "; ".join(violations))


def test_no_silently_swallowed_exceptions():
    """A broad except that only passes hides integrity failures.

    A *named* exception with ``pass`` is allowed: ``except asyncio.TimeoutError:
    pass`` is a deliberate loop-control idiom, not a swallowed failure.
    """
    violations: list[str] = []
    for path in _modules():
        for node in ast.walk(_tree(path)):
            if not isinstance(node, ast.ExceptHandler):
                continue
            broad = node.type is None or (
                isinstance(node.type, ast.Name)
                and node.type.id in {"Exception", "BaseException"})
            body = [item for item in node.body if not isinstance(item, ast.Expr)]
            if broad and len(body) == 1 and isinstance(body[0], ast.Pass):
                violations.append(f"{_relative(path)}:{node.lineno}")
    assert not violations, "silent broad except/pass at: " + ", ".join(violations)


def test_modules_stay_within_the_size_budget():
    oversized = []
    for path in _modules():
        relative = _relative(path)
        lines = len(path.read_text(encoding="utf-8").splitlines())
        cap = MODULE_LINE_EXEMPT.get(relative, MODULE_LINE_CAP)
        if lines > cap:
            oversized.append(f"{relative}={lines} (cap {cap})")
    assert not oversized, "modules over budget: " + ", ".join(oversized)
