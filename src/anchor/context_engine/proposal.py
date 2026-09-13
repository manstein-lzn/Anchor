"""What the model proposes, and how the engine turns it into cognition.

The model does not submit cognition. It submits *operations* — carry these, replace that, drop
that one and keep a pointer, add these — and the engine applies them. That division is the whole
design, and it is what makes two of the harder questions here answerable.

**Nothing carried is paraphrased.** A carried item travels as an id, so its statement is byte-for-
byte the statement that was already there. The obvious failure of a summary — that rewriting a
fact changes it — cannot happen to an item nobody rewrote. Only items the model actually revises
or adds get new text, and those are visible as such.

**The certificate is derived, not submitted.** The model says "carry `fact-1`" and the engine
writes the disposition. A model asked for both its new state and its account of what it did can
disagree with itself, and the disagreement is undetectable after the fact; a model asked only for
operations cannot, because there is nothing to disagree with.

**An id cannot be invented.** The schema constrains every `item_id` to an enum of the ids that
actually exist, so a fabricated id is refused before the engine sees it rather than caught
afterwards. `validate_transition` in `cognition.py` still checks coverage, because a model can
omit an operation even when it cannot invent one.

What remains a judgement is the text of revised and new items, and the reasons given for dropping
something. A certificate makes those auditable, not correct. This module does not pretend
otherwise either.
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass, field

from anchor.context_engine.cognition import (
    ITEM_GROUPS,
    Disposition,
    Cognition,
    CognitionItem,
    ItemDisposition,
    KnowledgeReference,
    TransitionCertificate,
)

#: The section names a new item may claim, as `section.group`. A closed set, so a model cannot
#: invent a place to put something where nothing would ever read it.
SECTIONS: tuple[str, ...] = tuple(f"{section}.{group}" for section, group in ITEM_GROUPS)


@dataclass(frozen=True)
class NewItem:
    """An item the model is adding, or the replacement for one it is revising."""

    section: str
    statement: str
    sources: tuple[str, ...]
    relevance: str
    evidence: tuple[str, ...] = ()


@dataclass(frozen=True)
class SourcedOperation:
    """Dropping an item, with a reason and where the reason came from."""

    item_id: str
    reason: str
    sources: tuple[str, ...]


@dataclass(frozen=True)
class ReplacementOperation:
    """Replacing an item with new text. The one place a carried statement may change."""

    item_id: str
    reason: str
    replacement: NewItem


@dataclass(frozen=True)
class DemotionOperation:
    """Dropping an item from the active set while keeping it reachable."""

    item_id: str
    reason: str
    sources: tuple[str, ...]
    reference: str


@dataclass(frozen=True)
class Proposal:
    """Everything the model is allowed to say about the transition.

    The field names are the operation kinds, so the schema the model is offered and the dataclass
    the engine reads cannot drift apart: there is one list per way an item can be disposed of.
    """

    current_understanding: str = ""
    current_directive: str = ""
    accepted_next_action: str = ""
    next_plan: tuple[str, ...] = ()
    carry_ids: tuple[str, ...] = ()
    revise: tuple[ReplacementOperation, ...] = ()
    resolve: tuple[SourcedOperation, ...] = ()
    supersede: tuple[ReplacementOperation, ...] = ()
    demote: tuple[DemotionOperation, ...] = ()
    archive: tuple[SourcedOperation, ...] = ()
    new_items: tuple[NewItem, ...] = ()
    knowledge_index: tuple[KnowledgeReference, ...] = ()
    #: Recorded so a reader can tell a proposal from an empty one.
    model_ref: str | None = None


@dataclass(frozen=True)
class Materialized:
    """The result: the next cognition, and the account of how it got there."""

    cognition: Cognition
    certificate: TransitionCertificate
    #: Newly assigned ids, by the statement that produced them, so a caller can see what a
    #: replacement actually became.
    assigned_ids: dict[str, str] = field(default_factory=dict)


def item_id(*, run_id: str, node_id: str, occurrence: int, statement: str) -> str:
    """A stable id for a new item.

    Derived rather than random, so re-materializing the same proposal produces the same ids. A
    random id would make a retried compression a different state, and the certificate's whole
    purpose is that the same transition is the same transition.
    """
    payload = json.dumps({"run": run_id, "node": node_id, "n": occurrence,
                          "statement": statement}, sort_keys=True, ensure_ascii=False)
    return "item-" + hashlib.sha256(payload.encode("utf-8")).hexdigest()[:16]


class AmbiguousProposal(ValueError):
    """One item disposed of by two operations, so there is no single state to materialize.

    Refused rather than resolved: picking an order would make the resulting state depend on the
    order this file happens to apply lists in, and a caller could not tell which operation won. A
    proposal that says both "carry d1" and "revise d1" has not decided, and the decision belongs
    to whoever wrote it.
    """

    code = "ambiguous_proposal"


def materialize(previous: Cognition, proposal: Proposal, *, run_id: str = "",
                node_id: str = "") -> Materialized:
    """Apply a proposal to a cognition, and derive the certificate from what was applied.

    Every previous item ends up in exactly one of the operation lists, and the certificate is
    built from those lists rather than from a separate claim. An item that appears in no list
    gets no disposition, which is what `validate_transition` reports as coverage — so the two
    halves of this design fail in the same place when they fail.
    """
    _refuse_double_disposition(proposal)
    by_id = {item.id: item for item in previous.items()}
    next_items: dict[str, list[CognitionItem]] = {key: [] for key in SECTIONS}
    dispositions: list[ItemDisposition] = []
    assigned: dict[str, str] = {}
    occurrence = 0

    def place(section: str, statement: str, sources: tuple[str, ...], relevance: str,
              evidence: tuple[str, ...]) -> str:
        nonlocal occurrence
        occurrence += 1
        new_id = item_id(run_id=run_id, node_id=node_id, occurrence=occurrence,
                         statement=statement)
        assigned[statement] = new_id
        next_items[section].append(CognitionItem(id=new_id, statement=statement,
                                                sources=sources, relevance=relevance,
                                                evidence=evidence))
        return new_id

    for item_id_ in proposal.carry_ids:
        item = by_id.get(item_id_)
        if item is None:
            # Not placed, and not disposed of either — so it shows up as a missing disposition
            # rather than as a quiet carry of something that does not exist.
            continue
        dispositions.append(ItemDisposition(item_id=item_id_, disposition="carry"))
        next_items[_section_of(previous, item_id_)].append(item)

    # Separate loops per operation kind rather than one loop over heterogeneous tuples. The shared
    # version read the same way but forced the type checker to widen every list to whatever the
    # first one was, which is how a genuine mismatch would have been hidden rather than reported.
    replacements_by_kind: tuple[tuple[Disposition, tuple[ReplacementOperation, ...]], ...] = (
        ("revise", proposal.revise), ("supersede", proposal.supersede))
    for kind, replacements in replacements_by_kind:
        for replacement_op in replacements:
            replacement_id = place(replacement_op.replacement.section,
                                   replacement_op.replacement.statement,
                                   replacement_op.replacement.sources,
                                   replacement_op.replacement.relevance,
                                   replacement_op.replacement.evidence)
            dispositions.append(ItemDisposition(
                item_id=replacement_op.item_id, disposition=kind,
                reason=replacement_op.reason,
                # The replacement carries its own sources, which is where the disposition's
                # citation comes from: the operation is only as well-sourced as what it put in.
                sources=replacement_op.replacement.sources, replacement_id=replacement_id))

    dropped_by_kind: tuple[tuple[Disposition, tuple[SourcedOperation, ...]], ...] = (
        ("resolve", proposal.resolve), ("archive", proposal.archive))
    for kind, dropped in dropped_by_kind:
        for dropped_item in dropped:
            dispositions.append(ItemDisposition(
                item_id=dropped_item.item_id, disposition=kind, reason=dropped_item.reason,
                sources=dropped_item.sources))

    for demoted in proposal.demote:
        dispositions.append(ItemDisposition(
            item_id=demoted.item_id, disposition="demote", reason=demoted.reason,
            sources=demoted.sources, reference=demoted.reference))

    for new_item in proposal.new_items:
        place(new_item.section, new_item.statement, new_item.sources, new_item.relevance,
              new_item.evidence)

    cognition = Cognition(
        situation={"current_understanding": proposal.current_understanding,
                   "confirmed_facts": next_items["situation.confirmed_facts"],
                   "active_hypotheses": next_items["situation.active_hypotheses"],
                   "unresolved_conflicts": next_items["situation.unresolved_conflicts"],
                   "blockers": next_items["situation.blockers"]},
        experience={"decisions": next_items["experience.decisions"],
                    "failed_paths": next_items["experience.failed_paths"]},
        intent={"current_directive": proposal.current_directive,
                "accepted_next_action": proposal.accepted_next_action,
                "next_plan": list(proposal.next_plan),
                "open_questions": next_items["intent.open_questions"]},
        verification={"model_ref": proposal.model_ref},
        knowledge_index=proposal.knowledge_index)
    return Materialized(cognition=cognition,
                        certificate=TransitionCertificate(tuple(dispositions)),
                        assigned_ids=assigned)


def _refuse_double_disposition(proposal: Proposal) -> None:
    """Every item appears in at most one operation list.

    Checked here rather than left to `validate_transition` because the state has to be built
    before it can be validated, and building it from an ambiguous proposal means silently
    choosing. Reporting a duplicate after choosing which one counted is too late to be useful.
    """
    seen: dict[str, list[str]] = {}
    for kind, ids_ in (
        ("carry", proposal.carry_ids),
        ("revise", tuple(o.item_id for o in proposal.revise)),
        ("resolve", tuple(o.item_id for o in proposal.resolve)),
        ("supersede", tuple(o.item_id for o in proposal.supersede)),
        ("demote", tuple(o.item_id for o in proposal.demote)),
        ("archive", tuple(o.item_id for o in proposal.archive)),
    ):
        for item_id_ in ids_:
            seen.setdefault(item_id_, []).append(kind)
    doubled = {item: kinds for item, kinds in seen.items() if len(kinds) > 1}
    if doubled:
        raise AmbiguousProposal(
            f"these items are disposed of more than once, so the resulting state is not "
            f"determined: {doubled}")


def _section_of(cognition: Cognition, item_id: str) -> str:
    """Which `section.group` holds this id, so a carried item goes back where it came from."""
    for section, group in ITEM_GROUPS:
        for item in getattr(cognition, section).get(group) or []:
            if item.id == item_id:
                return f"{section}.{group}"
    return SECTIONS[0]


def proposal_schema(active_ids: tuple[str, ...]) -> dict[str, object]:
    """The JSON schema the model is offered, with the ids it is allowed to name.

    `active_ids` becomes an enum rather than a free string, which is the mechanism that makes an
    invented id impossible instead of merely detected. The archived project did the same and the
    reason is worth restating: a check that runs after the model has answered is a check on a
    model that has already had the chance to be wrong.
    """
    if active_ids:
        id_schema: dict[str, object] = {"type": "string", "enum": list(active_ids)}
    else:
        id_schema = {"type": "string", "minLength": 1}
    sourced = {
        "type": "object",
        "properties": {"item_id": id_schema, "reason": {"type": "string", "minLength": 1},
                       "sources": {"type": "array", "minItems": 1,
                                   "items": {"type": "string", "minLength": 1}}},
        "required": ["item_id", "reason", "sources"],
        "additionalProperties": False,
    }
    replacement = {
        "type": "object",
        "properties": {"item_id": id_schema, "reason": {"type": "string", "minLength": 1},
                       "replacement": _new_item_schema()},
        "required": ["item_id", "reason", "replacement"],
        "additionalProperties": False,
    }
    demote = {
        "type": "object",
        "properties": {"item_id": id_schema, "reason": {"type": "string", "minLength": 1},
                       "sources": {"type": "array", "minItems": 1,
                                   "items": {"type": "string", "minLength": 1}},
                       "reference": {"type": "string", "minLength": 1}},
        "required": ["item_id", "reason", "sources", "reference"],
        "additionalProperties": False,
    }
    return {
        "type": "object",
        "properties": {
            "current_understanding": {"type": "string", "minLength": 1},
            "current_directive": {"type": "string", "minLength": 1},
            "accepted_next_action": {"type": "string", "minLength": 1},
            "next_plan": {"type": "array", "minItems": 1,
                          "items": {"type": "string", "minLength": 1}},
            "carry_ids": {"type": "array", "items": id_schema},
            "revise": {"type": "array", "items": replacement},
            "resolve": {"type": "array", "items": sourced},
            "supersede": {"type": "array", "items": replacement},
            "demote": {"type": "array", "items": demote},
            "archive": {"type": "array", "items": sourced},
            "new_items": {"type": "array", "items": _new_item_schema()},
            "knowledge_index": {
                "type": "array",
                "items": {"type": "object",
                          "properties": {"cue": {"type": "string", "minLength": 1},
                                         "locator": {"type": "string", "minLength": 1},
                                         "source": {"type": "string", "minLength": 1}},
                          "required": ["cue", "locator", "source"],
                          "additionalProperties": False}},
        },
        "required": ["current_understanding", "current_directive", "accepted_next_action",
                     "next_plan", "carry_ids", "revise", "resolve", "supersede", "demote",
                     "archive", "new_items", "knowledge_index"],
        "additionalProperties": False,
    }


def _new_item_schema() -> dict[str, object]:
    return {
        "type": "object",
        "properties": {
            "section": {"type": "string", "enum": list(SECTIONS)},
            "statement": {"type": "string", "minLength": 1},
            "sources": {"type": "array", "minItems": 1,
                        "items": {"type": "string", "minLength": 1}},
            "relevance": {"type": "string", "minLength": 1},
            "evidence": {"type": "array", "items": {"type": "string", "minLength": 1}},
        },
        "required": ["section", "statement", "sources", "relevance"],
        "additionalProperties": False,
    }
