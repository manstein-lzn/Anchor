"""Versioned surfaces declare a version, and the policy document covers them all.

A protocol without a version number has its breaking changes arrive downstream as bugs. This
checks the other half of that: that every surface the policy document names really does carry a
marker, and that nothing carrying a marker is missing from the document. Either gap leaves a
surface whose changes nobody has agreed how to describe.
"""

from __future__ import annotations

import ast
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DOC = ROOT / "docs" / "PROTOCOL_VERSIONS.md"

#: Surfaces the document names, and where each marker lives.
DECLARED = {
    "IR_VERSION": "src/anchor/domain/ir.py",
    "BUNDLE_VERSION": "src/anchor/domain/bundle.py",
    "EVALUATOR_VERSION": "src/anchor/domain/conditions.py",
    "ROUTING_EVALUATOR_VERSION": "src/anchor/domain/propagation.py",
    "RECORDING_FORMAT": "src/anchor/runtime/model_recording.py",
    "CACHE_FORMAT": "src/anchor/runtime/content_cache.py",
}


def test_the_document_names_every_surface_this_test_knows_about():
    text = DOC.read_text(encoding="utf-8")
    missing = [name for name in DECLARED if name not in text]
    assert not missing, (f"PROTOCOL_VERSIONS.md does not cover {missing}; a surface whose "
                         f"versioning is undocumented has none")


def test_every_declared_surface_actually_carries_a_marker():
    """The document would otherwise describe intent rather than code."""
    for name, relative in DECLARED.items():
        source = (ROOT / relative).read_text(encoding="utf-8")
        assert re.search(rf"^{name}\s*=", source, re.MULTILINE), \
            f"{relative} does not declare {name}"


def test_the_admission_message_pins_its_schema_version():
    """A literal type is what makes "only 1 is legal" true rather than intended: a producer
    that starts emitting 2 fails at construction instead of at a consumer somewhere."""
    tree = ast.parse((ROOT / "src/anchor/domain/admission.py").read_text(encoding="utf-8"))
    found = False
    for node in ast.walk(tree):
        if isinstance(node, ast.AnnAssign) and getattr(node.target, "id", "") == "schema_version":
            annotation = ast.unparse(node.annotation)
            assert "Literal[1]" in annotation, annotation
            found = True
    assert found, "RunDispatch no longer declares schema_version"


def test_nothing_carrying_a_marker_is_absent_from_the_document():
    """The other direction: a new versioned surface must be described, not just created."""
    text = DOC.read_text(encoding="utf-8")
    markers: set[str] = set()
    for path in sorted((ROOT / "src/anchor").rglob("*.py")):
        for match in re.finditer(r"^([A-Z][A-Z0-9_]*VERSION|FORMAT)\s*=", path.read_text(
                encoding="utf-8"), re.MULTILINE):
            markers.add(match.group(1))
    # The document may mention a marker under a different heading; it only has to appear.
    undocumented = sorted(name for name in markers if name not in text)
    assert not undocumented, (f"these versioned surfaces exist and are not in "
                              f"PROTOCOL_VERSIONS.md: {undocumented}")


def test_the_api_has_no_version_prefix_and_the_document_says_why():
    """An undocumented absence is indistinguishable from an oversight.

    The decision is that the only consumer lives in this repository, so a version prefix would
    mean two sets of endpoints to maintain for nobody. The document has to carry that reasoning,
    because the day a second consumer appears is the day the reasoning stops holding.
    """
    text = DOC.read_text(encoding="utf-8")
    assert "HTTP API" in text and "无版本前缀" in text
    assert "anchor.client" in text, "the document must name the single consumer it relies on"


def test_the_status_vocabulary_is_the_one_the_document_defines():
    """Four words whose difference is whether the thing can be trusted.

    `Completed` and `Not yet implemented` are the ones this replaces: they do not distinguish
    "code exists" from "code exists and is verified", which is the only distinction that matters
    to a reader deciding whether to rely on something.
    """
    banned = ("Completed", "Not yet implemented", "Work in progress", "TBD")
    offenders: list[str] = []
    for path in sorted(ROOT.glob("*.md")) + sorted((ROOT / "docs").glob("*.md")):
        if path.name in {"DEVELOPMENT_PLAN.md", "COGNITION_ARCHITECTURE.md",
                         "COGNITION_ARCHITECTURE_REVIEW.md", "AGENT_ARCHITECTURE_RESEARCH_BRIEF.md",
                         "COGNITION_REVIEW_BRIEF.md", "deep-research-report.md",
                         "PROTOCOL_VERSIONS.md", "EXECUTION_POLICY_PLAN.md"}:
            continue  # historical records and the policy that defines the vocabulary
        text = path.read_text(encoding="utf-8")
        for word in banned:
            if word in text:
                offenders.append(f"{path.relative_to(ROOT)}: {word}")
    assert not offenders, (f"documents spell status with words the vocabulary replaced: "
                           f"{offenders}")
