"""Every invariant names a test, and the test is there.

A table in a document rots the moment somebody renames a test, and the document is the thing a
reader trusts. So the mapping is parsed out of `docs/INVARIANTS.md` and each named test is
looked up: a rename breaks this, which is the point. The invariant that has no test is the one
nobody notices is gone.

The parse is deliberately strict. A table row that means to bind a test and does not name one is
read as an unbound invariant rather than silently skipped, because "the row was there" is exactly
how a document comes to claim coverage it does not have.
"""

from __future__ import annotations

import ast
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
DOC = ROOT / "docs" / "INVARIANTS.md"

#: `| **I1** | ... | `file.py::test_name` |`
ROW = re.compile(r"^\|\s*\*\*(I\d+)\*\*\s*\|.*?\|\s*`([^`]+)`\s*\|\s*$", re.MULTILINE)
NAME = re.compile(r"^([A-Za-z_][\w.]*)\.py::([A-Za-z_]\w*)$")


def bindings() -> dict[str, str]:
    return {invariant: reference for invariant, reference in ROW.findall(
        DOC.read_text(encoding="utf-8"))}


def defined_tests() -> set[str]:
    """Every test this suite defines, as ``file.py::name``."""
    found: set[str] = set()
    for path in sorted((ROOT / "tests").glob("test_*.py")):
        tree = ast.parse(path.read_text(encoding="utf-8"))
        for node in tree.body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) \
                    and node.name.startswith("test_"):
                found.add(f"{path.name}::{node.name}")
    return found


def test_the_document_binds_every_invariant():
    """I1 through I9, all nine, and none of them quietly dropped."""
    bound = bindings()
    assert set(bound) == {f"I{number}" for number in range(1, 10)}, \
        f"unbound or unknown invariants: {sorted(bound)}"


def test_every_bound_reference_is_well_formed():
    for invariant, reference in bindings().items():
        assert NAME.match(reference), \
            f"{invariant} does not name a test as file.py::name: {reference!r}"


def test_every_named_test_exists():
    """The check that keeps the table honest. Renaming a test fails here rather than leaving a
    document that claims coverage which no longer runs."""
    defined = defined_tests()
    missing = {invariant: reference for invariant, reference in bindings().items()
               if reference not in defined}
    assert not missing, f"INVARIANTS.md names tests that do not exist: {missing}"


@pytest.mark.parametrize("invariant", [f"I{number}" for number in range(1, 10)])
def test_no_invariant_binds_the_same_test_as_another(invariant):
    """Two invariants pointing at one test would mean one of them is not really covered.

    Not fatal in principle — one test can exercise two guarantees — but it is a signal worth
    answering, so it is refused here unless the document says why.
    """
    bound = bindings()
    same = [other for other, reference in bound.items()
            if other != invariant and reference == bound[invariant]]
    assert not same, (f"{invariant} and {same} both name {bound[invariant]}; if that is "
                      f"intentional, say so in the document rather than leaving the reader to "
                      f"guess whether one of them is covered")
