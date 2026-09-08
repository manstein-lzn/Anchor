"""Evidence checks and deterministic Markdown delivery for academic graphs."""

from __future__ import annotations

import json
import re

from anchor.domain.context import canonical_json
from pydantic import BaseModel, ConfigDict, Field, ValidationError
from typing import Literal


class ResearchOutput(BaseModel):
    model_config = ConfigDict(strict=True)
    manuscript: str = Field(min_length=1)
    sources: list[dict]
    search_log: list[dict]
    coverage: list[dict]
    unresolved: list[str]


class ReviewOutput(BaseModel):
    model_config = ConfigDict(strict=True)
    verdict: Literal["pass", "revise", "blocked"]
    summary: str
    issues: list[dict]
    strengths: list[str]
    blockers: list[str]


def validate_agent_output(text: str, role: str | None = None) -> dict:
    value = json.loads(text)
    if not isinstance(value, dict):
        raise ValueError("Output must be a JSON object")
    schema = {"researcher": ResearchOutput, "reviewer": ReviewOutput}.get(role)
    if schema:
        try:
            schema.model_validate(value)
        except ValidationError as exc:
            raise ValueError(str(exc)) from exc
    return value


def preflight_review(snapshot: dict, *, store, artifacts, run_id) -> dict | None:
    work = snapshot.get("research")
    request = snapshot.get("request", {})
    if not isinstance(work, dict):
        errors = ["Research output must be a complete JSON object"]
    else:
        errors, _ = validate_manuscript(work, operations=store.list_tool_operations(run_id),
            artifacts=artifacts, minimum_sources=request.get("minimum_sources", 8),
            minimum_reads=request.get("minimum_reads", 3))
    if not errors:
        return None
    return {"verdict": "revise", "summary": "Mechanical preflight failed; scholarly review was not invoked",
            "issues": [{"severity": "major", "location": "research", "problem": error,
                        "required_action": error} for error in errors], "strengths": [], "blockers": [],
            "review_origin": "deterministic_preflight"}


SECTIONS = ("Abstract", "Introduction", "Methods", "Literature Review", "Discussion",
            "Limitations", "Conclusion")


def citation_numbers(text: str) -> set[int]:
    return {int(number) for group in re.findall(r"\[(\d+(?:\s*,\s*\d+)*)\]", text)
            for number in group.split(",")}


def validate_manuscript(work: dict, *, operations, artifacts, minimum_sources: int = 8,
                        minimum_reads: int = 3) -> tuple[list[str], list[dict]]:
    errors: list[str] = []
    manuscript = work.get("manuscript", "")
    if not isinstance(manuscript, str) or not manuscript.startswith("# "):
        return ["Manuscript must be Markdown with a paper title"], []
    for section in SECTIONS:
        if not re.search(r"^## " + re.escape(section) + r"\s*$", manuscript, re.MULTILINE):
            errors.append(f"Missing paper section: {section}")
    if re.search(r"^## References\s*$", manuscript, re.MULTILINE):
        errors.append("Do not write References manually; they are generated from retrieved metadata")
    records = work.get("sources")
    if not isinstance(records, list):
        return errors + ["Research output must contain a sources array"], []
    successful = {item.result_ref: item.tool_ref for item in operations
                  if item.status.value == "succeeded" and item.result_ref}
    canonical_sources = []
    used_ids: set[str] = set()
    used_numbers: set[int] = set()
    read_ids: set[str] = set()
    for source in records:
        if not isinstance(source, dict):
            errors.append("Source entries must be objects")
            continue
        number, identity, evidence_ref = source.get("citation"), source.get("id"), source.get("evidence_ref")
        if (type(number) is not int or number < 1 or not isinstance(identity, str)
                or not isinstance(evidence_ref, str)):
            errors.append("Each source needs a positive citation number, retrieved id and evidence_ref")
            continue
        if identity in used_ids or number in used_numbers:
            errors.append(f"Duplicate source identity or citation number: {number}")
            continue
        used_ids.add(identity)
        used_numbers.add(number)
        if successful.get(evidence_ref) != "scholarly.search":
            errors.append(f"Citation [{number}] has no successful scholarly search in this Run")
            continue
        try:
            evidence = json.loads(artifacts.get_text(evidence_ref))
            observed = next((item for item in evidence["papers"] if item["id"] == identity), None)
            if observed is None:
                raise ValueError("source id not present in search results")
            canonical = {**observed, "citation": number, "evidence_ref": evidence_ref,
                         "retrieved_at": evidence["retrieved_at"], "read_ref": None}
            read_ref = source.get("read_ref")
            if read_ref:
                if not isinstance(read_ref, str) or successful.get(read_ref) != "scholarly.read":
                    raise ValueError("read_ref is not a successful document read in this Run")
                reading = json.loads(artifacts.get_text(read_ref))
                allowed_urls = {observed.get("url"), *observed.get("fulltext_urls", [])}
                if reading.get("requested_url") not in allowed_urls:
                    raise ValueError("document read does not match the retrieved source URLs")
                canonical["read_ref"] = read_ref
                canonical["read_truncated"] = reading.get("truncated", False)
                read_ids.add(identity)
            canonical_sources.append(canonical)
        except (OSError, ValueError, KeyError, TypeError) as exc:
            errors.append(f"Citation [{number}] evidence rejected: {exc}")
    citations = citation_numbers(manuscript)
    verified_numbers = {source["citation"] for source in canonical_sources}
    if citations - verified_numbers:
        errors.append("Citations without verified sources: " + str(sorted(citations - verified_numbers)))
    if verified_numbers - citations:
        errors.append("Sources not cited in manuscript: " + str(sorted(verified_numbers - citations)))
    if len(citations & verified_numbers) < minimum_sources:
        errors.append(f"Need at least {minimum_sources} distinct cited and retrieved sources")
    if len(read_ids) < minimum_reads:
        errors.append(f"Need at least {minimum_reads} source documents read beyond the search listing")
    return errors, sorted(canonical_sources, key=lambda source: source["citation"])


def evaluate_review(snapshot: dict, *, store, artifacts, run_id, node_id: str) -> dict:
    review, work = snapshot.get("review"), snapshot.get("research")
    if not isinstance(review, dict):
        review = {"verdict": "revise", "summary": "Review output must be a JSON object"}
    malformed = not isinstance(work, dict)
    verdict = review.get("verdict")
    if not isinstance(verdict, str) or verdict not in {"pass", "revise", "blocked"}:
        raise ValueError("Academic review verdict must be pass, revise, or blocked")
    request = snapshot.get("request", {})
    minimum_sources = request.get("minimum_sources", 8)
    minimum_reads = request.get("minimum_reads", 3)
    if (type(minimum_sources) is not int or minimum_sources < 1
            or type(minimum_reads) is not int or not 0 <= minimum_reads <= minimum_sources):
        raise ValueError("Invalid academic source requirements")
    errors, sources = validate_manuscript(
        work if not malformed else {}, operations=store.list_tool_operations(run_id), artifacts=artifacts,
        minimum_sources=minimum_sources, minimum_reads=minimum_reads,
    )
    if errors and verdict == "pass":
        verdict = "revise"
    prior_nodes = [n for n in store.list_node_runs(run_id)
                   if n.node_id == node_id and n.status.value == "completed" and n.output_ref]
    run = store.get_run(run_id)
    graph = store.get_graph_version(run.graph_version_id)
    max_rounds = graph.definition.metadata.get("max_rounds") if graph else None
    if max_rounds is not None:
        max_rounds = int(max_rounds)
        if max_rounds < 1:
            raise ValueError("max_rounds must be a positive integer when set")
    if verdict == "revise" and max_rounds is not None and len(prior_nodes) + 1 >= max_rounds:
        # Explicit operator-set max_rounds still caps revision loops. This is
        # not a hidden default; absent a configured value, the supervisor and
        # watchdog diagnose repeated cycles instead of terminating silently.
        verdict = "blocked"
        errors.append(f"Revision budget exhausted after {max_rounds} rounds; draft is not approved")
    elif verdict == "revise" and len(prior_nodes) > 0:
        prior = json.loads(artifacts.get_text(max(prior_nodes, key=lambda n: n.attempt).output_ref))
        if canonical_json(prior.get("research", {})) == canonical_json(work):
            # Repeated identical research without verified progress: record
            # evidence and surface to supervisor/watchdog for diagnosis,
            # instead of silently blocking the run.
            errors.append("A completed cycle repeated the same research; operator input or diagnosis is required")
            review["verdict"] = "revise"
            review.setdefault("mechanical_issues", []).append(errors[-1])
    return {**snapshot, "review": {**review, "verdict": verdict, "mechanical_issues": errors},
            "verified_sources": sources}


def render_paper(snapshot: dict, *, run_id) -> str:
    if snapshot.get("review", {}).get("verdict") != "pass":
        raise ValueError("Only an approved academic review can publish a manuscript")
    text = snapshot["research"]["manuscript"].rstrip()
    references = []
    provenance = []
    for source in snapshot["verified_sources"]:
        authors = ", ".join(source.get("authors", [])) or "Author unavailable"
        label = "Preprint" if source.get("publication_type") == "preprint" else source.get("venue", "")
        references.append(f"[{source['citation']}] {authors} ({source.get('year') or 'n.d.'}). "
                          f"{source['title']}. {label}. <{source['url']}>")
        level = "document retrieved" if source.get("read_ref") else source.get("evidence_level", "metadata_only")
        provenance.append(f"- [{source['citation']}] {source['retrieved_at']}; {level}; "
                          f"search evidence: `{source['evidence_ref']}`" +
                          (f"; reading evidence: `{source['read_ref']}`" if source.get("read_ref") else ""))
    return (text + "\n\n## References\n\n" + "\n\n".join(references)
            + "\n\n## Retrieval Evidence\n\nRun: `" + str(run_id) + "`\n\n"
            + "\n".join(provenance) + "\n")


# ---------------------------------------------------------------------------
# Behavior registration
#
# The kernel never imports this module. The composition root (service
# entrypoints) registers these behaviors under stable references, so academic
# policy stays a plugin instead of branching inside the generic worker.
# ---------------------------------------------------------------------------

class AcademicAgentBehavior:
    """Preflight, schema and structured-output policy for one academic role."""

    def __init__(self, role: str | None) -> None:
        self.role = role

    def preflight(self, snapshot: dict, *, store, artifacts, run_id) -> dict | None:
        if self.role != "reviewer":
            return None
        return preflight_review(snapshot, store=store, artifacts=artifacts, run_id=run_id)

    def validate_output(self, text: str) -> None:
        validate_agent_output(text, self.role)

    def execute_control(self, snapshot: dict, *, store, artifacts, run_id, node_id) -> dict | str:
        raise ValueError(f"agent behavior {self.role!r} cannot execute control node {node_id!r}")


class ReviewGateBehavior:
    """Deterministic evidence gate that runs before an expensive reviewer call."""

    def preflight(self, snapshot: dict, *, store, artifacts, run_id) -> dict | None:
        return None

    def validate_output(self, text: str) -> None:
        return None

    def execute_control(self, snapshot: dict, *, store, artifacts, run_id, node_id) -> dict:
        return evaluate_review(snapshot, store=store, artifacts=artifacts,
                               run_id=run_id, node_id=node_id)


class MarkdownReportBehavior:
    """Deterministic Markdown delivery from verified sources."""

    def preflight(self, snapshot: dict, *, store, artifacts, run_id) -> dict | None:
        return None

    def validate_output(self, text: str) -> None:
        return None

    def execute_control(self, snapshot: dict, *, store, artifacts, run_id, node_id) -> str:
        return render_paper(snapshot["approved"], run_id=run_id)


ACADEMIC_BEHAVIORS = {
    "academic.planner": AcademicAgentBehavior("planner"),
    "academic.researcher": AcademicAgentBehavior("researcher"),
    "academic.reviewer": AcademicAgentBehavior("reviewer"),
    "academic.review_gate": ReviewGateBehavior(),
    "academic.report": MarkdownReportBehavior(),
}


def register_academic_behaviors(registry) -> None:
    """Composition-root hook: attach academic policy to the generic registry."""
    for ref, behavior in ACADEMIC_BEHAVIORS.items():
        registry.register(ref, behavior)
