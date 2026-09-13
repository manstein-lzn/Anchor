"""The one model call that compresses, and what it is allowed to say.

Every compression scheme costs a model call; the only question is what that call produces. The
industry answer is a summary, and this produces operations instead, for reasons that are stated
where they matter rather than repeated here: a summary rewrites facts, and a proposal carries them
by id.

The prompts below are adapted from the archived project's `anchor.update-proposal.v3` and
`anchor.bootstrap-proposal.v2`. What changed in the adaptation, and why:

- Locators are content references rather than `checkpoint:<version>:item:<id>`. The archived
  project pointed at its own version numbers; a graph points at content, so a reader can confirm
  the bytes are still there instead of trusting that a version still resolves.
- Items may cite `evidence`, which the archived schema had no field for because a session had no
  operation ledger to cite. It is optional so a hypothesis can honestly cite nothing.
- The Contract is not asked for. Acceptance criteria belong to a verifier, which is a declared
  object in a graph, so a proposal should not restate them in prose where nothing can check them.

The submission shape is a schema-constrained answer rather than JSON in a prompt. That is what
makes the id constraint real: the schema is sent, so an enumerated id is one the model *cannot*
violate rather than one this code notices afterwards.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Literal

from pydantic import BaseModel, Field, create_model

from anchor.context_engine.cognition import Cognition
from anchor.context_engine.proposal import (
    SECTIONS,
    DemotionOperation,
    Materialized,
    NewItem,
    Proposal,
    ReplacementOperation,
    SourcedOperation,
    materialize,
)

UPDATE_SYSTEM = """You are the Anchor Update Agent.

This is state transition, not conversation summarization. Given one previous cognition, the
messages about to leave the context, and the recent messages that will stay, decide what the
cognition becomes so that correct future action is still possible.

You do not write the new state. You submit operations, and the engine applies them:
carry_ids keeps an item exactly as it is, revise and supersede replace one, resolve and archive
drop one, demote drops one but keeps a pointer, new_items adds.

Rules:
- Preserve the goal, the acceptance criteria and the constraints. Never change them silently.
- Keep Situation, Experience and Intent distinct. Preserve causes and retry conditions, not
  chronology.
- Apply newer explicit corrections. Keep hypotheses and unresolved conflicts explicit until
  evidence resolves them.
- A tool output can support an observed fact; your confidence cannot. A failed or interrupted tool
  call is not success.
- Keep an item active only when forgetting it could cause a wrong decision, a constraint
  violation, a repeated failure or expense, or the loss of the next action.
- Every previous item must appear in exactly one operation. The item ids are enumerated in the
  schema; do not invent one, and do not name one twice.
- Non-carry operations need a reason and a source. A demotion needs a reference that resolves, and
  that reference must also appear in the knowledge index.
- Cite evidence where you have it: an operation id for something observed, a verifier reference
  for a criterion, a content reference for stored detail. Cite nothing rather than something that
  does not support the item.
- current_directive, accepted_next_action and the goal are three different things.

Answer once, through the schema. Do not return prose."""

#: Used when there is no previous cognition. The first compression is an update from an empty
#: state rather than a separate mechanism: `materialize` already handles a previous state with no
#: items, and the ids an operation may name fall back from an enumeration to a plain string. The
#: archived project kept a separate Bootstrap because it also established a Contract; a graph has
#: no such object to establish, since acceptance criteria are a verifier.
BOOTSTRAP_SYSTEM = """You are the Anchor Bootstrap Agent.

There is no previous cognition, so there is nothing to carry, revise or resolve. Build the initial
state from the messages you are given alone.

Rules:
- Do not invent a goal, a constraint, an acceptance criterion, a decision or a fact that the
  messages do not support. Unknown requirements belong in open_questions.
- This is not a summary and not a planning conversation. It is the first cognition.
- Preserve uncertainty explicitly rather than resolving it by assertion.
- Every item needs a statement, a source and a relevance.
- Cite evidence where you have it, and cite nothing rather than something that does not support
  the item.

Answer once, through the schema. Do not return prose."""


@dataclass(frozen=True)
class UpdateOutcome:
    """What one compression produced, and what it cost."""

    materialized: Materialized
    response_text: str
    input_tokens: int = 0
    output_tokens: int = 0
    #: The ids the model was allowed to name, kept so a failure can be read against the enum it
    #: was given rather than against the ids that exist now.
    active_ids: tuple[str, ...] = ()


class NewItemModel(BaseModel):
    """An item being added, or the replacement for one being revised.

    `section` is a `Literal` over the declared sections, so a model cannot put something where
    nothing would ever read it. `evidence` is optional, so a hypothesis can honestly cite nothing
    rather than being pushed to cite something that does not support it.
    """

    section: Literal[SECTIONS]  # type: ignore[valid-type]
    statement: str = Field(min_length=1)
    sources: tuple[str, ...] = Field(min_length=1)
    relevance: str = Field(min_length=1)
    evidence: tuple[str, ...] = ()


class KnowledgeReferenceModel(BaseModel):
    """Where an item went. The locator is a content reference, which resolves or does not."""

    id: str = Field(min_length=1)
    cue: str = Field(min_length=1)
    locator: str = Field(min_length=1)
    source: str = Field(min_length=1)


def proposal_model(active_ids: tuple[str, ...]) -> type[BaseModel]:
    """A submission schema whose item ids are the ones that exist.

    Built per call because `Literal` has to be built from the ids at hand, and that is the mechanism
    the design leans on: an enumerated id is one the model cannot write, rather than one this code
    notices afterwards. It is also why the proposal type cannot be a module-level model.

    The two field models are defined above rather than inline because `create_model` resolves the
    types it is handed at call time, so a forward reference to a class defined later would silently
    become a string the provider never sees.
    """
    id_type: Any = Literal[active_ids] if active_ids else str
    sourced = create_model("SourcedOperation", item_id=(id_type, ...), reason=(str, ...),
                           sources=(tuple[str, ...], ...))
    replacement = create_model("ReplacementOperation", item_id=(id_type, ...), reason=(str, ...),
                               replacement=(NewItemModel, ...))
    demote = create_model("DemotionOperation", item_id=(id_type, ...), reason=(str, ...),
                          sources=(tuple[str, ...], ...), reference=(str, ...))
    # The element types are names of classes created two lines up, so mypy reads them as variables
    # and refuses them as types. The `type: ignore` is deliberate and narrow: these are real types
    # at runtime, and replacing them with `Any` to satisfy the checker silently removes the nested
    # models from the generated schema — which removes the id enum with them, and that enum is the
    # reason this schema exists at all. A test asserts the nested enums are present.
    return create_model(
        "Proposal",
        current_understanding=(str, Field(min_length=1)),
        current_directive=(str, Field(min_length=1)),
        accepted_next_action=(str, Field(min_length=1)),
        next_plan=(tuple[str, ...], ...),
        carry_ids=(tuple[id_type, ...], ...),
        revise=(tuple[replacement, ...], ...),  # type: ignore[valid-type]
        resolve=(tuple[sourced, ...], ...),  # type: ignore[valid-type]
        supersede=(tuple[replacement, ...], ...),  # type: ignore[valid-type]
        demote=(tuple[demote, ...], ...),  # type: ignore[valid-type]
        archive=(tuple[sourced, ...], ...),  # type: ignore[valid-type]
        new_items=(tuple[NewItemModel, ...], ...),
        knowledge_index=(tuple[KnowledgeReferenceModel, ...], ...),
    )


def to_proposal(answer: Any) -> Proposal:
    """Read a validated submission into the engine's own shape."""

    def new_item(raw: Any) -> NewItem:
        return NewItem(section=raw.section, statement=raw.statement,
                       sources=tuple(raw.sources), relevance=raw.relevance,
                       evidence=tuple(getattr(raw, "evidence", ()) or ()))

    return Proposal(
        current_understanding=answer.current_understanding,
        current_directive=answer.current_directive,
        accepted_next_action=answer.accepted_next_action,
        next_plan=tuple(answer.next_plan),
        carry_ids=tuple(answer.carry_ids),
        revise=tuple(ReplacementOperation(o.item_id, o.reason, new_item(o.replacement))
                     for o in answer.revise),
        resolve=tuple(SourcedOperation(o.item_id, o.reason, tuple(o.sources))
                      for o in answer.resolve),
        supersede=tuple(ReplacementOperation(o.item_id, o.reason, new_item(o.replacement))
                        for o in answer.supersede),
        demote=tuple(DemotionOperation(o.item_id, o.reason, tuple(o.sources), o.reference)
                     for o in answer.demote),
        archive=tuple(SourcedOperation(o.item_id, o.reason, tuple(o.sources))
                      for o in answer.archive),
        new_items=tuple(new_item(i) for i in answer.new_items),
        knowledge_index=tuple(
            _knowledge(i) for i in answer.knowledge_index))


def _knowledge(raw: Any) -> Any:
    from anchor.context_engine.cognition import KnowledgeReference

    return KnowledgeReference(id=raw.id, cue=raw.cue, locator=raw.locator, source=raw.source)


@dataclass(frozen=True)
class Episode:
    """The text that is leaving the context, and the text that stays.

    Passed as text rather than as messages because the prompt is the boundary: what the Update
    sees is what a reader can reconstruct, and a framework message type would make the prompt
    depend on the framework's version.
    """

    leaving: str
    staying: str = ""
    source: str = ""


async def run_update(gateway: Any, previous: Cognition, episode: Episode, *, run_id: str = "",
                     node_id: str = "", model_ref: str | None = None,
                     system_prompt: str | None = None) -> UpdateOutcome:
    """One compression: propose operations, materialize the result, derive the certificate.

    The model's answer is validated against a schema whose ids are enumerated, so a fabricated one
    is refused by the provider rather than caught here. Everything after that is deterministic:
    the operations are applied, and the certificate comes from what was applied.
    """
    from anchor.context_engine.cognition import validate_transition

    active = tuple(sorted(previous.item_ids()))
    prompt = _update_prompt(previous, episode)
    answer, response = await gateway.generate_structured(
        prompt=prompt, system_prompt=system_prompt or UPDATE_SYSTEM,
        output_type=proposal_model(active))
    proposal = to_proposal(answer)
    materialized = materialize(previous, proposal, run_id=run_id, node_id=node_id)
    problems = validate_transition(materialized.certificate, previous, materialized.cognition)
    if problems:
        # Not raised as a ValueError, because a caller reading a log needs to know which of the
        # certificate's rules was broken and for which item, not that "validation failed".
        raise UpdateRejected(problems)
    return UpdateOutcome(materialized=materialized, response_text=response.text,
                         input_tokens=response.input_tokens,
                         output_tokens=response.output_tokens, active_ids=active)


class UpdateRejected(RuntimeError):
    """The engine built a state the certificate does not accept.

    Reachable even though the schema constrains the ids, because a schema can require that every
    operation list is present and still permit every one of them to be empty. Coverage is the rule
    that catches an omission, and it has to run.
    """

    code = "update_rejected"

    def __init__(self, problems: list[Any]) -> None:
        super().__init__("; ".join(f"{p.code}: {p.detail}" for p in problems))
        self.problems = problems


def _update_prompt(previous: Cognition, episode: Episode) -> str:
    """The previous cognition and the two pieces of text, laid out so a reader can check it.

    The ids are written out because the schema enumerates them anyway; a model that can see which
    ids it must account for is less likely to omit one, and the omission is the failure the
    certificate exists to catch.
    """
    lines = ["Previous cognition:", _render(previous), ""]
    if episode.source:
        lines += [f"Recent suffix source: {episode.source}", ""]
    lines += ["Messages leaving the context:", episode.leaving or "(none)"]
    if episode.staying:
        lines += ["", "Recent messages that will remain:", episode.staying]
    return "\n".join(lines)


def _render(cognition: Cognition) -> str:
    from anchor.context_engine.cognition import ITEM_GROUPS

    out = [f"current_understanding: {cognition.situation.get('current_understanding', '')}",
           f"current_directive: {cognition.intent.get('current_directive', '')}",
           f"accepted_next_action: {cognition.intent.get('accepted_next_action', '')}"]
    for section, group in ITEM_GROUPS:
        items = getattr(cognition, section).get(group) or []
        if not items:
            continue
        out.append(f"{section}.{group}:")
        for item in items:
            out.append(f"  [{item.id}] {item.statement}  (sources: {', '.join(item.sources)})")
    if cognition.knowledge_index:
        out.append("knowledge_index:")
        for reference in cognition.knowledge_index:
            out.append(f"  [{reference.id}] {reference.cue} -> {reference.locator}")
    return "\n".join(out)
