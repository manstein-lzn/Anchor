"""The model proposes operations; the engine decides what the state becomes.

Two properties are worth more than the rest and are the reason this shape was chosen over asking a
model for its new state directly.

**Nothing carried is paraphrased.** A carried item travels as an id, so its statement is byte-for-
byte what it already was. The classic failure of a summary — that rewriting a fact changes it —
cannot happen to an item nobody rewrote.

**The certificate is derived, not submitted.** A model asked for both its new state and its
account of what it did can contradict itself, and nothing afterwards can tell. A model asked only
for operations has nothing to contradict: the dispositions come from what was actually applied.
"""

from __future__ import annotations

import pytest

from anchor.context_engine.cognition import (
    Cognition,
    CognitionItem,
    KnowledgeReference,
    validate_transition,
)
from anchor.context_engine.proposal import (
    SECTIONS,
    DemotionOperation,
    NewItem,
    Proposal,
    ReplacementOperation,
    SourcedOperation,
    item_id,
    materialize,
    proposal_schema,
)


def item(id_: str, statement: str = "", *, evidence: tuple[str, ...] = ()) -> CognitionItem:
    return CognitionItem(id=id_, statement=statement or f"{id_} holds",
                         sources=("episode:1",), relevance="changes the next action",
                         evidence=evidence)


def previous() -> Cognition:
    return Cognition(
        situation={"confirmed_facts": [item("f1", "the API is available")],
                   "active_hypotheses": [item("h1")], "unresolved_conflicts": [],
                   "blockers": []},
        experience={"decisions": [item("d1", "keep the adapter narrow")], "failed_paths": [item("x1")]},
        intent={"open_questions": []})


def proposal(**overrides) -> Proposal:
    base = dict(current_understanding="the adapter is ready",
                current_directive="finish the adapter",
                accepted_next_action="run the focused test", next_plan=("run the test",),
                carry_ids=("f1", "h1", "d1"),
                archive=(SourcedOperation("x1", "superseded by the new approach", ("episode:2",)),))
    base.update(overrides)
    return Proposal(**base)


def codes(problems) -> list[str]:
    return [problem.code for problem in problems]


class Resolver:
    """A stand-in for the graph's own facts, so a cited reference can be resolved in a test."""

    def __init__(self, *, failed=(), succeeded=(), artifacts=(), verifiers=()):
        self._failed, self._succeeded = set(failed), set(succeeded)
        self._artifacts, self._verifiers = set(artifacts), set(verifiers)

    def operation_failed(self, operation_id):
        return operation_id in self._failed

    def operation_succeeded(self, operation_id):
        return operation_id in self._succeeded

    def artifact_exists(self, ref):
        return ref in self._artifacts

    def verifier_exists(self, ref):
        return ref in self._verifiers


# -- nothing carried is paraphrased -------------------------------------------------


def test_a_carried_item_keeps_its_exact_statement():
    """The property that answers "how is the text not distorted". It is not rewritten, so it is
    not distorted, and no prompt can change that."""
    before = previous()
    result = materialize(before, proposal())
    carried = next(i for i in result.cognition.items() if i.id == "f1")
    original = next(i for i in before.items() if i.id == "f1")
    assert carried is original, "the same object, not a copy that happens to match"
    assert carried.statement == "the API is available"


def test_a_carried_item_returns_to_the_section_it_came_from():
    """Otherwise a decision quietly becomes a fact, which is a category error the reader cannot
    see because both are just items in a list."""
    result = materialize(previous(), proposal())
    assert [i.id for i in result.cognition.experience["decisions"]] == ["d1"]
    assert [i.id for i in result.cognition.situation["confirmed_facts"]] == ["f1"]
    assert [i.id for i in result.cognition.situation["active_hypotheses"]] == ["h1"]


def test_only_revised_and_new_items_get_new_text():
    result = materialize(previous(), proposal(
        carry_ids=("f1", "h1"),                       # d1 is revised, so it is not also carried
        revise=(ReplacementOperation("d1", "the user narrowed it further",
                                     NewItem("experience.decisions", "keep the interface narrow",
                                             ("episode:2",), "changes the next action")),)))
    assert "keep the interface narrow" in {i.statement for i in result.cognition.items()}
    assert "keep the adapter narrow" not in {i.statement for i in result.cognition.items()}


# -- the certificate is derived from what was applied --------------------------------


def test_the_certificate_is_derived_from_the_operations_not_submitted_alongside_them():
    result = materialize(previous(), proposal())
    assert [(d.item_id, d.disposition) for d in result.certificate.dispositions] == \
        [("f1", "carry"), ("h1", "carry"), ("d1", "carry"), ("x1", "archive")]


def test_a_materialized_certificate_validates_against_the_state_it_produced():
    """The two halves of the design have to agree, and this is where they meet: an engine-built
    certificate should survive the same check a model-built one is put through."""
    before = previous()
    result = materialize(before, proposal(
        carry_ids=("f1", "h1"),
        revise=(ReplacementOperation("d1", "narrowed",
                                     NewItem("experience.decisions", "keep it narrow",
                                             ("episode:2",), "changes the next action")),),
        new_items=(NewItem("intent.open_questions", "is the API stable?", ("episode:2",),
                           "blocks the next step"),)))
    assert validate_transition(result.certificate, before, result.cognition) == []


def test_an_item_left_out_of_every_operation_is_reported_as_coverage():
    """The engine does not invent a disposition for an item nobody mentioned. Omitting it has to be
    visible, and the same check catches it whether a model or a caller did the omitting."""
    before = previous()
    result = materialize(before, Proposal(current_understanding="x", current_directive="y",
                                          accepted_next_action="z", next_plan=("s",),
                                          carry_ids=("f1",)))
    problems = codes(validate_transition(result.certificate, before, result.cognition))
    assert "coverage_incomplete" in problems


def test_an_id_that_does_not_exist_is_not_carried():
    """A fabricated id gets no disposition rather than a carry of nothing, so an invented id is
    refused by the same coverage check as an omitted one."""
    before = previous()
    result = materialize(before, proposal(carry_ids=("f1", "h1", "d1", "invented")))
    assert "invented" not in {i.id for i in result.cognition.items()}
    assert validate_transition(result.certificate, before, result.cognition) == []


# -- ids ----------------------------------------------------------------------------


def test_a_new_id_is_derived_so_a_retry_is_the_same_state():
    """A random id would make a retried compression a different state, which defeats the point of
    the certificate: the same transition has to be the same transition."""
    before = previous()
    again = Proposal(current_understanding="x", current_directive="y", accepted_next_action="z",
                     next_plan=("s",), carry_ids=("f1", "h1", "d1"),
                     archive=(SourcedOperation("x1", "done", ("episode:2",)),),
                     new_items=(NewItem("intent.open_questions", "is it stable?", ("episode:2",),
                                        "blocks"),))
    first = materialize(before, again, run_id="r", node_id="n")
    second = materialize(before, again, run_id="r", node_id="n")
    assert first.cognition.item_ids() == second.cognition.item_ids()
    assert first.certificate == second.certificate


def test_ids_differ_between_runs_and_between_nodes():
    """Stable within a transition, distinct across them, or two tasks' items would collide."""
    new = NewItem("intent.open_questions", "same words", ("episode:2",), "relevance")
    a = item_id(run_id="r1", node_id="plan", occurrence=1, statement=new.statement)
    assert a != item_id(run_id="r2", node_id="plan", occurrence=1, statement=new.statement)
    assert a != item_id(run_id="r1", node_id="write", occurrence=1, statement=new.statement)
    assert a == item_id(run_id="r1", node_id="plan", occurrence=1, statement=new.statement)


def test_a_replacement_is_reachable_by_the_id_its_disposition_names():
    result = materialize(previous(), proposal(
        carry_ids=("f1", "h1"),
        revise=(ReplacementOperation("d1", "narrowed",
                                     NewItem("experience.decisions", "keep it narrow",
                                             ("episode:2",), "changes the next action")),)))
    disposition = next(d for d in result.certificate.dispositions if d.item_id == "d1")
    assert disposition.replacement_id in result.cognition.item_ids()


# -- the schema the model is offered -------------------------------------------------


def test_the_schema_constrains_item_ids_to_the_ones_that_exist():
    """An invented id becomes impossible rather than merely detected. A check that runs after the
    model has answered is a check on a model that has already had the chance to be wrong."""
    schema = proposal_schema(("f1", "d1"))
    properties = schema["properties"]
    assert properties["carry_ids"]["items"]["enum"] == ["f1", "d1"]
    assert properties["revise"]["items"]["properties"]["item_id"]["enum"] == ["f1", "d1"]
    assert properties["demote"]["items"]["properties"]["item_id"]["enum"] == ["f1", "d1"]
    assert properties["resolve"]["items"]["properties"]["item_id"]["enum"] == ["f1", "d1"]


def test_the_schema_closes_every_object_and_requires_every_operation_list():
    """A closed object is what makes "an operation we do not know about" impossible; required lists
    are what make an omission a schema violation rather than a silent empty list."""
    schema = proposal_schema(("f1",))
    assert schema["additionalProperties"] is False
    assert set(schema["required"]) == set(schema["properties"])
    for kind in ("revise", "resolve", "supersede", "demote", "archive", "new_items"):
        assert schema["properties"][kind]["items"]["additionalProperties"] is False
    assert schema["properties"]["new_items"]["items"]["properties"]["section"]["enum"] == \
        list(SECTIONS)


def test_the_schema_names_every_section_a_new_item_may_claim():
    """A closed set, so a model cannot put something where nothing would ever read it."""
    assert SECTIONS == ("situation.confirmed_facts", "situation.active_hypotheses",
                        "situation.unresolved_conflicts", "situation.blockers",
                        "experience.decisions", "experience.failed_paths",
                        "intent.open_questions")


def test_an_empty_previous_state_leaves_the_id_schema_open():
    """Bootstrap has no ids to enumerate, and an empty enum would make every proposal invalid."""
    schema = proposal_schema(())
    assert "enum" not in schema["properties"]["carry_ids"]["items"]
    assert schema["properties"]["carry_ids"]["items"]["minLength"] == 1


# -- demotion ------------------------------------------------------------------------


def test_a_demotion_keeps_its_reference_and_the_reference_is_checked():
    locator = "artifact://sha256/" + "a" * 64
    result = materialize(previous(), proposal(
        demote=(DemotionOperation("x1", "useful for audit only", ("episode:2",), locator),),
        carry_ids=("f1", "h1", "d1"), archive=(),     # demoted, not archived
        knowledge_index=(KnowledgeReference(id="ref-x1", cue="a failed approach", locator=locator,
                                            source="episode:2"),)))
    disposition = next(d for d in result.certificate.dispositions if d.item_id == "x1")
    assert disposition.disposition == "demote"
    assert disposition.reference == locator
    assert "x1" not in result.cognition.item_ids(), "a demoted item leaves the active set"
    # The citation satisfies the ported rule, and the reference still cannot be resolved without a
    # resolver, so this reports rather than passing.
    assert codes(validate_transition(result.certificate, previous(), result.cognition)) == \
        ["evidence_unchecked"]
    # With one, and the content present, it passes.
    # the resolver turns the same proposal into a verified one
    present = materialize(previous(), proposal(
        demote=(DemotionOperation("x1", "useful for audit only", ("episode:2",), locator),),
        carry_ids=("f1", "h1", "d1"), archive=(),
        knowledge_index=(KnowledgeReference(id="ref-x1", cue="a failed approach", locator=locator,
                                            source="episode:2"),)))
    assert validate_transition(present.certificate, previous(), present.cognition,
                               resolver=Resolver(artifacts={locator})) == []


def test_a_demotion_whose_reference_is_absent_from_the_index_is_reported():
    """Ported rule. Without it the certificate is the only place the reference was ever written."""
    result = materialize(previous(), proposal(
        archive=(), demote=(DemotionOperation("x1", "audit only", ("episode:2",),
                                              "artifact://sha256/" + "b" * 64),)))
    assert "demotion_missing_from_index" in codes(
        validate_transition(result.certificate, previous(), result.cognition))


@pytest.mark.parametrize("section", SECTIONS)
def test_an_item_can_be_added_to_any_declared_section(section):
    result = materialize(previous(), proposal(
        new_items=(NewItem(section, "something new", ("episode:2",), "relevance"),)))
    assert "something new" in {i.statement for i in result.cognition.items()}
    assert validate_transition(result.certificate, previous(), result.cognition) == []
