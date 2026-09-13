"""Cognition, and the certificate that makes a transition auditable.

This is the deterministic half of context compression: given a *proposal* to replace some
cognition with new cognition, decide whether the proposal is well-formed. It never calls a model
and never decides whether the new cognition is *right* — only whether nothing was lost without
saying so, and whether the evidence it cites exists.

The design comes from the archived project's `anchor.transition.v1`, whose rule is the whole
point:

    every previous active item gets exactly one disposition,
    non-carry dispositions carry a reason and a source,
    demote carries a recoverable reference,
    and no item may disappear silently.

What a graph makes possible beyond that original: the *evidence* can be checked, not just the
coverage. An archived session had no operation ledger, no verifiers and no content addressing, so
it could only confirm that an item had been disposed of. Here a disposition that cites evidence
can have that evidence resolved — a failed path must name an operation that really failed, an
accepted criterion must name a verifier that exists, and a locator must resolve to content that
is really there. That is a strictly larger check, and it is why this is not a port.

What neither can do is judge the prose. `current_understanding`, `active_hypotheses`,
`unresolved_conflicts`, `blockers` and `decisions` are judgements, and a certificate makes the
*disposition* of a judgement auditable rather than its content correct. Nothing here pretends
otherwise.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Literal, Protocol

from anchor.domain.content import ContentRefError, parse as parse_content_ref

#: Our own schema, distinct from the archived project's line, because the item shape differs: an
#: item may carry evidence here, and there is a verification section the original had no use for.
COGNITION_SCHEMA = "anchor.node-cognition.v1"
TRANSITION_SCHEMA = "anchor.transition.v2"

Disposition = Literal["carry", "revise", "resolve", "supersede", "demote", "archive"]

#: Which dispositions must carry what. `carry` is the only one that may be silent, and only
#: because the item is still present in the submitted cognition under the same id.
REQUIRES_REFERENCE: frozenset[str] = frozenset({"demote"})
REQUIRES_REPLACEMENT: frozenset[str] = frozenset({"revise", "supersede"})

#: The sections whose items the certificate must account for, in the order a reader expects.
ITEM_GROUPS: tuple[tuple[str, str], ...] = (
    ("situation", "confirmed_facts"),
    ("situation", "active_hypotheses"),
    ("situation", "unresolved_conflicts"),
    ("situation", "blockers"),
    ("experience", "decisions"),
    ("experience", "failed_paths"),
    ("intent", "open_questions"),
)


@dataclass(frozen=True)
class CognitionItem:
    """One thing the task currently holds to be true, assumed, or prevented by.

    `evidence` is what a graph adds: references that can be resolved rather than prose that has
    to be believed. An empty tuple is legitimate — a hypothesis has no evidence yet, and saying
    so is more honest than citing something that does not support it.
    """

    id: str
    statement: str
    sources: tuple[str, ...]
    relevance: str
    evidence: tuple[str, ...] = ()


@dataclass(frozen=True)
class KnowledgeReference:
    """Where an item went when it left the active set.

    The locator is content-addressed rather than an internal pointer, so a reader can confirm the
    content is still there instead of trusting that a version number still resolves.
    """

    id: str
    cue: str
    locator: str
    source: str


@dataclass(frozen=True)
class Cognition:
    """The state a node's context is projected from, replacing a summary of what happened."""

    situation: dict[str, Any] = field(default_factory=dict)
    experience: dict[str, Any] = field(default_factory=dict)
    intent: dict[str, Any] = field(default_factory=dict)
    verification: dict[str, Any] = field(default_factory=dict)
    knowledge_index: tuple[KnowledgeReference, ...] = ()
    schema: str = COGNITION_SCHEMA

    def items(self) -> list[CognitionItem]:
        """Every active item, across every section that has them."""
        found: list[CognitionItem] = []
        for section, group in ITEM_GROUPS:
            found.extend(getattr(self, section).get(group) or [])
        return found

    def item_ids(self) -> set[str]:
        return {item.id for item in self.items()}


@dataclass(frozen=True)
class ItemDisposition:
    """What happened to one previously active item."""

    item_id: str
    disposition: Disposition
    reason: str = ""
    sources: tuple[str, ...] = ()
    reference: str | None = None
    replacement_id: str | None = None


@dataclass(frozen=True)
class TransitionCertificate:
    dispositions: tuple[ItemDisposition, ...] = ()
    schema: str = TRANSITION_SCHEMA


@dataclass(frozen=True)
class Problem:
    """One reason a proposal is not acceptable. All of them are reported, not the first."""

    code: str
    detail: str
    item_id: str | None = None


class EvidenceResolver(Protocol):
    """What the graph can answer that the archived project could not.

    Each method answers a factual question about a reference. Implementations that cannot answer
    return ``None``, which is reported as unresolved rather than treated as a pass — an
    unverifiable claim is not a verified one.
    """

    def operation_failed(self, operation_id: str) -> bool | None:
        """Did this operation exist and finish as a failure?"""
        ...

    def operation_succeeded(self, operation_id: str) -> bool | None:
        """Did this operation exist and finish successfully?"""
        ...

    def artifact_exists(self, ref: str) -> bool | None:
        """Does this content-addressed reference resolve?"""
        ...

    def verifier_exists(self, ref: str) -> bool | None:
        """Is this verifier declared?"""
        ...


def validate_transition(certificate: TransitionCertificate, previous: Cognition,
                        next_cognition: Cognition, *,
                        resolver: EvidenceResolver | None = None) -> list[Problem]:
    """Every reason the certificate does not account for the previous cognition.

    Coverage is checked as an exact set equality in both directions, which is what the archived
    project did and worth keeping: a missing entry means an item vanished, and an extra entry
    means an id was invented. Either is a silent lie about what was in the state, and neither can
    be detected any other way once the proposal has replaced it.

    The submitted cognition is required rather than optional. A caller always has it, and making
    it optional created a state where three checks silently did not run — which is the failure
    this module refuses elsewhere. The resolver is optional because it depends on having a store,
    and its absence is reported rather than assumed harmless.
    """
    problems: list[Problem] = []
    previous_ids = previous.item_ids()
    covered = [entry.item_id for entry in certificate.dispositions]
    covered_set = set(covered)

    missing = sorted(previous_ids - covered_set)
    if missing:
        problems.append(Problem(
            "coverage_incomplete",
            f"these previous items have no disposition and would disappear silently: {missing}"))
    invented = sorted(covered_set - previous_ids)
    if invented:
        problems.append(Problem(
            "unknown_previous_item",
            f"these ids are not in the previous cognition: {invented}"))

    duplicated = sorted({item for item in covered if covered.count(item) > 1})
    if duplicated:
        problems.append(Problem(
            "duplicate_disposition",
            f"these items are disposed of more than once: {duplicated}"))

    for entry in certificate.dispositions:
        problems.extend(_validate_entry(entry, next_cognition))
    problems.extend(_validate_evidence(certificate, next_cognition, resolver))
    return problems


def _validate_entry(entry: ItemDisposition, next_cognition: Cognition) -> list[Problem]:
    problems: list[Problem] = []
    if entry.disposition != "carry" and not entry.reason:
        problems.append(Problem(
            "missing_reason",
            f"{entry.disposition} must say why it is not a carry", item_id=entry.item_id))
    if entry.disposition != "carry" and not entry.sources:
        problems.append(Problem(
            "missing_source",
            f"{entry.disposition} must cite where it came from", item_id=entry.item_id))
    if entry.disposition in REQUIRES_REFERENCE and not entry.reference:
        # The rule that makes demotion safe: an item may leave the active set only if a reader
        # can still reach it. Without this, "demote" is another word for "delete".
        problems.append(Problem(
            "missing_reference",
            "demote must carry a recoverable reference", item_id=entry.item_id))
    if entry.disposition in REQUIRES_REPLACEMENT and not entry.replacement_id:
        problems.append(Problem(
            "missing_replacement",
            f"{entry.disposition} must name what replaces it", item_id=entry.item_id))
    if entry.disposition == "carry":
        # A carry is a claim that the item is still there. If it is not, the certificate says the
        # item survived while the cognition says it did not, and the disagreement has to surface
        # here rather than as a missing item three compressions later.
        if entry.item_id not in next_cognition.item_ids():
            problems.append(Problem(
                "carry_not_present",
                "carried item is absent from the submitted cognition", item_id=entry.item_id))
    if entry.replacement_id and entry.replacement_id not in next_cognition.item_ids():
        problems.append(Problem(
            "replacement_not_present",
            "replacement does not name an item in the submitted cognition",
            item_id=entry.item_id))
    return problems


def _validate_evidence(certificate: TransitionCertificate, next_cognition: Cognition,
                       resolver: EvidenceResolver | None) -> list[Problem]:
    """What a graph can check and an archived session could not.

    A disposition's reason is prose and cannot be checked. The evidence it cites can be, and the
    difference matters: without this, "this path already failed" is an assertion the state makes
    about itself, and the archived project's own review named exactly that as the thing to watch.

    Split in two because the two halves check different things: a demotion's reference has to be
    recoverable whether or not a resolver exists, and cited evidence can only be checked with one.
    """
    problems = _validate_demotion_references(certificate, next_cognition)
    if resolver is None:
        # Reported only when there was something to check. A cognition full of judgments and
        # citing nothing has no evidence to verify, and reporting otherwise would train a reader
        # to ignore the code. When there *is* evidence and no resolver, silence would read as
        # having verified it.
        if _has_citable_evidence(certificate, next_cognition):
            problems.append(Problem(
                "evidence_unchecked",
                "no resolver was supplied, so cited evidence was not verified"))
        return problems
    problems.extend(_validate_cited_evidence(next_cognition, resolver))
    problems.extend(_validate_demotion_reachability(certificate, resolver))
    return problems


def _validate_demotion_references(certificate: TransitionCertificate,
                                  next_cognition: Cognition) -> list[Problem]:
    """Ported rule: a demotion has to appear in the index, or the certificate is the only place
    the reference was ever written down."""
    locators = {reference.locator for reference in next_cognition.knowledge_index}
    return [Problem("demotion_missing_from_index",
                    "demoted item's reference is not in the knowledge index",
                    item_id=entry.item_id)
            for entry in certificate.dispositions
            if entry.disposition == "demote" and entry.reference not in locators]


def _has_citable_evidence(certificate: TransitionCertificate,
                          next_cognition: Cognition) -> bool:
    """Whether anything was cited that a resolver could have checked."""
    if any(item.evidence for item in next_cognition.items()):
        return True
    if next_cognition.knowledge_index:
        return True
    return any(entry.reference for entry in certificate.dispositions)


def _validate_cited_evidence(next_cognition: Cognition,
                             resolver: EvidenceResolver) -> list[Problem]:
    """Every reference an item cites, interpreted by its scheme."""
    problems: list[Problem] = []
    for item in next_cognition.items():
        for reference in item.evidence:
            verdict = _resolve(reference, resolver)
            if verdict is None:
                problems.append(Problem("evidence_unresolved",
                                        f"{reference!r} could not be resolved", item_id=item.id))
            elif verdict is False:
                problems.append(Problem("evidence_contradicts",
                                        f"{reference!r} does not support this item",
                                        item_id=item.id))
    for entry in next_cognition.knowledge_index:
        verdict = _resolve_content(entry.locator, resolver)
        if verdict is None:
            problems.append(Problem("locator_unresolved",
                                    f"{entry.locator!r} could not be resolved"))
        elif verdict is False:
            problems.append(Problem("locator_missing", f"{entry.locator!r} does not exist"))
    return problems


def _validate_demotion_reachability(certificate: TransitionCertificate,
                                    resolver: EvidenceResolver) -> list[Problem]:
    """A demoted item must still be reachable, which is what makes demotion not a deletion."""
    problems: list[Problem] = []
    for entry in certificate.dispositions:
        if entry.reference is None:
            continue
        if _resolve_content(entry.reference, resolver) is False:
            problems.append(Problem("reference_missing",
                                    f"demotion reference {entry.reference!r} does not exist",
                                    item_id=entry.item_id))
    return problems


#: Schemes for the evidence a graph can check. A content reference is not matched here: it is
#: parsed through the boundary type, so the prefix lives in one place rather than two.
_SCHEMES = (("failed-operation:", "failed-operation"), ("operation:", "operation"),
            ("verifier:", "verifier"))


def _resolve(reference: str, resolver: EvidenceResolver) -> bool | None:
    """Interpret one evidence reference, or report that we cannot.

    Anything that is not one of the schemes above is handed to the content-reference boundary,
    which owns that syntax. Matching an artifact prefix here would put it in a second place, and
    the architecture test that forbids that is right to.
    """
    for prefix, kind in _SCHEMES:
        if reference.startswith(prefix):
            name = reference[len(prefix):]
            if kind == "operation":
                return resolver.operation_succeeded(name)
            if kind == "failed-operation":
                return resolver.operation_failed(name)
            return resolver.verifier_exists(name)
    try:
        parsed = parse_content_ref(reference)
    except ContentRefError:
        return None
    if parsed.is_artifact:
        return resolver.artifact_exists(reference)
    # A workspace revision is a content reference too, and there is no resolver method for it, so
    # this is unresolved rather than assumed fine.
    return None


def _resolve_content(reference: str, resolver: EvidenceResolver) -> bool | None:
    """Resolve a content reference specifically, for places that can only hold one."""
    try:
        parsed = parse_content_ref(reference)
    except ContentRefError:
        return None
    return resolver.artifact_exists(reference) if parsed.is_artifact else None
