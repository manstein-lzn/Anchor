"""Evidence checks and deterministic Markdown delivery for academic graphs."""

from __future__ import annotations

import json
import re

from anchor.domain.content import ARTIFACT_PREFIX
from anchor.domain.context import canonical_json
from pydantic import BaseModel, ConfigDict, Field, ValidationError
from typing import Literal


class ResearchOutput(BaseModel):
    """Legacy single-agent output: evidence and manuscript in one response."""

    model_config = ConfigDict(strict=True)
    manuscript: str = Field(min_length=1)
    sources: list[dict]
    search_log: list[dict]
    coverage: list[dict]
    unresolved: list[str]


class GatherOutput(BaseModel):
    """The evidence ledger. It carries no prose; the writer owns the paper."""

    model_config = ConfigDict(strict=True)
    sources: list[dict]
    search_log: list[dict]
    evidence_notes: list[dict]
    coverage: list[dict]
    tensions: list[dict]
    unresolved: list[str]


class WriteOutput(BaseModel):
    """The reader-facing deliverable. It cites the ledger; it never audits it."""

    model_config = ConfigDict(strict=True)
    manuscript: str = Field(min_length=1)
    thesis: str = Field(min_length=1)


class ReviewOutput(BaseModel):
    model_config = ConfigDict(strict=True)
    verdict: Literal["pass", "revise", "blocked"]
    # Where the fix belongs: more evidence, or better writing. This keeps the
    # revision loop pointed at the real deficiency instead of redoing both.
    target: Literal["evidence", "manuscript", "none"] = "none"
    summary: str
    issues: list[dict]
    strengths: list[str]
    blockers: list[str]


def validate_agent_output(text: str, role: str | None = None) -> dict:
    value = json.loads(text)
    if not isinstance(value, dict):
        raise ValueError("Output must be a JSON object")
    schema = {"researcher": ResearchOutput, "gatherer": GatherOutput,
              "writer": WriteOutput, "reviewer": ReviewOutput}.get(role)
    if schema:
        try:
            schema.model_validate(value)
        except ValidationError as exc:
            raise ValueError(str(exc)) from exc
    return value


def preflight_review(snapshot: dict, *, store, artifacts, run_id) -> dict | None:
    work = _extract_work(snapshot)
    request = snapshot.get("request", {})
    if not isinstance(work, dict) or not work.get("manuscript"):
        errors, target = ["Research output must be a complete JSON object"], "manuscript"
    else:
        errors, _, target = validate_manuscript(work, operations=store.list_tool_operations(run_id),
            artifacts=artifacts, minimum_sources=request.get("minimum_sources", 8),
            minimum_reads=request.get("minimum_reads", 3))
    if not errors:
        return None
    return {"verdict": "revise", "target": target,
            "summary": "Mechanical preflight failed; scholarly review was not invoked",
            "issues": [{"severity": "major", "location": "manuscript", "problem": error,
                        "required_action": error} for error in errors], "strengths": [], "blockers": [],
            "review_origin": "deterministic_preflight"}


# The paper is the reader-facing deliverable. Verification is a background
# property of the pipeline, so audit language in the body is a defect, not a
# virtue: it spends the reader's attention on how we know rather than on what we
# know. These checks make "reader-facing" judgeable instead of decorative.
EVIDENCE_TAG = re.compile(r"（\s*(?:全文级|摘要级|题录级)[^）]{0,16}）")
PROCESS_PHRASES = ("本轮", "本次检索", "本文检索到", "本文共执行", "工具预算",
                   "未在本次", "本次未执行")
# Section skeleton. This is not invented: survey and systematic-review papers have
# converged on it. The fixed anchors are conventional; the body must be named by
# the paper's own axes, never by one catch-all "Literature Review" bucket.
# Measured against 1801.04405 (ACM CSUR), 1304.1002 (SLR), 1808.04836 (survey study).
REQUIRED_SECTIONS = ("Abstract", "Introduction", "Survey Methodology", "Conclusion")
VALIDITY_SECTIONS = ("Threats to Validity", "Limitations")
FORBIDDEN_SECTIONS = ("Literature Review",)
MIN_THEMATIC_SECTIONS = 2
MAX_ABSTRACT_CHARS = 1800
MAX_PARAGRAPH_CHARS = 1200
MAX_METHODS_CHARS = 2500


def citation_numbers(text: str) -> set[int]:
    return {int(number) for group in re.findall(r"\[(\d+(?:\s*,\s*\d+)*)\]", text)
            for number in group.split(",")}


def _section_text(manuscript: str, heading: str) -> str:
    match = re.search(r"^## " + re.escape(heading) + r"\s*$", manuscript, re.MULTILINE)
    if not match:
        return ""
    rest = manuscript[match.end():]
    following = re.search(r"^## ", rest, re.MULTILINE)
    return rest[:following.start()] if following else rest


def _headings(manuscript: str) -> list[str]:
    return [heading.strip() for heading in re.findall(r"^## (.+?)\s*$", manuscript, re.MULTILINE)]


def structure_errors(manuscript: str) -> list[str]:
    """Enforce the converged survey skeleton and a thematic, not catch-all, body."""
    headings = _headings(manuscript)
    errors: list[str] = []
    for section in REQUIRED_SECTIONS:
        if section not in headings:
            errors.append(f"Missing required section: {section}")
    if not any(section in headings for section in VALIDITY_SECTIONS):
        errors.append("Missing a validity section: Threats to Validity (or Limitations)")
    for section in FORBIDDEN_SECTIONS:
        if section in headings:
            errors.append(f"Do not use a catch-all {section!r} section; name the body by theme")
    fixed = set(REQUIRED_SECTIONS) | set(VALIDITY_SECTIONS) | set(FORBIDDEN_SECTIONS)
    if len([heading for heading in headings if heading not in fixed]) < MIN_THEMATIC_SECTIONS:
        errors.append(f"The body needs at least {MIN_THEMATIC_SECTIONS} thematic sections, "
                      f"each named by an axis of the field, not one catch-all section")
    positions = {heading: index for index, heading in enumerate(headings)}
    if {"Abstract", "Introduction"} <= set(positions) \
            and positions["Abstract"] > positions["Introduction"]:
        errors.append("Abstract must precede Introduction")
    if {"Survey Methodology", "Introduction"} <= set(positions) \
            and positions["Survey Methodology"] < positions["Introduction"]:
        errors.append("Survey Methodology must follow the Introduction")
    validity = [heading for heading in headings if heading in VALIDITY_SECTIONS]
    if validity and "Conclusion" in positions \
            and positions[validity[0]] > positions["Conclusion"]:
        errors.append("Threats to Validity (or Limitations) must precede the Conclusion")
    abstract = _section_text(manuscript, "Abstract")
    if len(abstract) > MAX_ABSTRACT_CHARS:
        errors.append(f"Abstract must stay under {MAX_ABSTRACT_CHARS} characters; "
                      f"found {len(abstract)}")
    return errors


def craft_errors(manuscript: str) -> list[str]:
    """Reader-facing defects: audit language, walls of text, runaway Methods."""
    errors: list[str] = []
    if EVIDENCE_TAG.search(manuscript):
        errors.append("Remove evidence-level tags from the body; they belong in the appendix")
    for phrase in PROCESS_PHRASES:
        if phrase in manuscript:
            errors.append(f"Remove process language from the body: {phrase!r}")
    if ARTIFACT_PREFIX in manuscript:
        errors.append("Remove evidence hashes from the body; they belong in the appendix")
    blocks = [block.strip() for block in re.split(r"\n\s*\n", manuscript)]
    oversized = [block for block in blocks
                 if not block.startswith(("#", "|")) and len(block) > MAX_PARAGRAPH_CHARS]
    if oversized:
        errors.append(f"Paragraphs must stay under {MAX_PARAGRAPH_CHARS} characters; "
                      f"found {max(len(block) for block in oversized)}")
    methods = _section_text(manuscript, "Survey Methodology") or _section_text(manuscript, "Methods")
    if len(methods) > MAX_METHODS_CHARS:
        errors.append(f"Survey Methodology must stay under {MAX_METHODS_CHARS} characters and leave "
                      f"the search log to the appendix; found {len(methods)}")
    return errors


def _extract_work(snapshot: dict) -> dict:
    """Combine the writer's manuscript with the gatherer's evidence ledger."""
    manuscript, evidence = snapshot.get("manuscript"), snapshot.get("evidence")
    if isinstance(manuscript, dict) and isinstance(evidence, dict):
        return {**evidence, "manuscript": manuscript.get("manuscript", ""),
                "thesis": manuscript.get("thesis", "")}
    legacy = snapshot.get("research")
    return legacy if isinstance(legacy, dict) else {}


def validate_manuscript(work: dict, *, operations, artifacts, minimum_sources: int = 8,
                        minimum_reads: int = 3) -> tuple[list[str], list[dict], str]:
    """Return (errors, verified sources, where the fix belongs)."""
    errors: list[str] = []
    craft: list[str] = []
    manuscript = work.get("manuscript", "")
    if not isinstance(manuscript, str) or not manuscript.startswith("# "):
        return ["Manuscript must be Markdown with a paper title"], [], "manuscript"
    craft.extend(structure_errors(manuscript))
    if re.search(r"^## References\s*$", manuscript, re.MULTILINE):
        craft.append("Do not write References manually; they are generated from retrieved metadata")
    craft.extend(craft_errors(manuscript))
    records = work.get("sources")
    if not isinstance(records, list):
        return craft + ["Research output must contain a sources array"], [], "evidence"
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
    target = "evidence" if errors else ("manuscript" if craft else "none")
    return (errors + craft, sorted(canonical_sources, key=lambda source: source["citation"]),
            target)


def evaluate_review(snapshot: dict, *, store, artifacts, run_id, node_id: str) -> dict:
    review, work = snapshot.get("review"), _extract_work(snapshot)
    if not isinstance(review, dict):
        review = {"verdict": "revise", "target": "manuscript",
                  "summary": "Review output must be a JSON object"}
    malformed = not isinstance(work, dict) or not work.get("manuscript")
    verdict = review.get("verdict")
    if not isinstance(verdict, str) or verdict not in {"pass", "revise", "blocked"}:
        raise ValueError("Academic review verdict must be pass, revise, or blocked")
    request = snapshot.get("request", {})
    minimum_sources = request.get("minimum_sources", 8)
    minimum_reads = request.get("minimum_reads", 3)
    if (type(minimum_sources) is not int or minimum_sources < 1
            or type(minimum_reads) is not int or not 0 <= minimum_reads <= minimum_sources):
        raise ValueError("Invalid academic source requirements")
    errors, sources, deterministic_target = validate_manuscript(
        work if not malformed else {}, operations=store.list_tool_operations(run_id), artifacts=artifacts,
        minimum_sources=minimum_sources, minimum_reads=minimum_reads,
    )
    if errors and verdict == "pass":
        verdict = "revise"
    target = review.get("target")
    if target not in ("evidence", "manuscript", "none"):
        target = "none"
    if errors and target == "none":
        target = deterministic_target
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
        if canonical_json(_extract_work(prior)) == canonical_json(work):
            # Repeated identical research without verified progress: record
            # evidence and surface to supervisor/watchdog for diagnosis,
            # instead of silently blocking the run.
            errors.append("A completed cycle repeated the same research; operator input or diagnosis is required")
            review["verdict"] = "revise"
            review.setdefault("mechanical_issues", []).append(errors[-1])
    return {**snapshot, "review": {**review, "verdict": verdict, "target": target,
                                   "mechanical_issues": errors},
            "verified_sources": sources}


def render_paper(snapshot: dict, *, run_id) -> str:
    if snapshot.get("review", {}).get("verdict") != "pass":
        raise ValueError("Only an approved academic review can publish a manuscript")
    text = _extract_work(snapshot)["manuscript"].rstrip()
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
    "academic.gatherer": AcademicAgentBehavior("gatherer"),
    "academic.writer": AcademicAgentBehavior("writer"),
    "academic.reviewer": AcademicAgentBehavior("reviewer"),
    "academic.review_gate": ReviewGateBehavior(),
    "academic.report": MarkdownReportBehavior(),
}


def register_academic_behaviors(registry) -> None:
    """Composition-root hook: attach academic policy to the generic registry."""
    for ref, behavior in ACADEMIC_BEHAVIORS.items():
        registry.register(ref, behavior)
