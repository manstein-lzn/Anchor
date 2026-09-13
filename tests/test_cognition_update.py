"""One compression: what the model is allowed to say, and what happens to the answer.

Every compression scheme costs a model call. The question this file is about is what that call is
allowed to produce, and the answer is operations rather than a new state — so that carried facts
travel by id and are never rewritten, and so that the engine rather than the model decides what
the state becomes.

The schema is the mechanism. An enumerated id is one the model cannot write, which is a different
kind of guarantee from one this code notices afterwards.
"""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from anchor.context_engine.cognition import Cognition, CognitionItem
from anchor.context_engine.update import (
    BOOTSTRAP_SYSTEM,
    UPDATE_SYSTEM,
    Episode,
    UpdateRejected,
    proposal_model,
    run_update,
    to_proposal,
)


def item(id_: str, statement: str = "") -> CognitionItem:
    return CognitionItem(id=id_, statement=statement or f"{id_} holds",
                         sources=("episode:1",), relevance="changes the next action")


def previous() -> Cognition:
    return Cognition(
        situation={"confirmed_facts": [item("f1", "the API is available")],
                   "active_hypotheses": [], "unresolved_conflicts": [], "blockers": []},
        experience={"decisions": [item("d1", "keep the adapter narrow")], "failed_paths": []},
        intent={"open_questions": []})


class Gateway:
    """Returns a prepared answer, and records what it was asked."""

    def __init__(self, answer):
        self.answer = answer
        self.prompt = self.system = self.output_type = None

    async def generate_structured(self, *, prompt, system_prompt, output_type):
        self.prompt, self.system, self.output_type = prompt, system_prompt, output_type

        class Response:
            text = "{}"
            input_tokens, output_tokens = 100, 50

        return self.answer, Response()


def answer(**overrides) -> object:
    """A submission shaped the way the schema requires, with nothing left empty by accident."""
    base = dict(current_understanding="the adapter is ready", current_directive="finish it",
                accepted_next_action="run the focused test", next_plan=("run it",),
                carry_ids=("f1", "d1"), revise=(), resolve=(), supersede=(), demote=(),
                archive=(), new_items=(), knowledge_index=())
    base.update(overrides)

    class Answer:
        pass

    value = Answer()
    for key, item_value in base.items():
        setattr(value, key, item_value)
    return value


# -- the schema is the mechanism -----------------------------------------------------


def test_every_operation_can_only_name_an_id_that_exists():
    """This is what makes a fabricated id impossible rather than merely detected. A check that runs
    after the model has answered is a check on a model that has already had the chance to be wrong.
    """
    schema = proposal_model(("f1", "d1")).model_json_schema()
    for operation in ("SourcedOperation", "ReplacementOperation", "DemotionOperation"):
        assert schema["$defs"][operation]["properties"]["item_id"]["enum"] == ["f1", "d1"]
    assert schema["properties"]["carry_ids"]["items"]["enum"] == ["f1", "d1"]


def test_a_new_item_can_only_claim_a_declared_section():
    """A closed set, so a model cannot put something where nothing would ever read it."""
    schema = proposal_model(("f1",)).model_json_schema()
    sections = schema["$defs"]["NewItemModel"]["properties"]["section"]["enum"]
    assert "experience.failed_paths" in sections
    assert len(sections) == 7


def test_the_schema_requires_every_operation_list():
    """Required lists make an omission a schema violation rather than a silent empty list."""
    schema = proposal_model(("f1",)).model_json_schema()
    assert set(schema["required"]) == set(schema["properties"])
    for kind in ("revise", "resolve", "supersede", "demote", "archive", "new_items",
                 "knowledge_index"):
        assert kind in schema["required"]


def test_an_empty_previous_state_leaves_the_ids_open():
    """Bootstrap has no ids to enumerate, and an empty enum would make every proposal invalid."""
    schema = proposal_model(()).model_json_schema()
    assert "enum" not in schema["properties"]["carry_ids"]["items"]


def test_the_model_is_actually_validated_against_that_schema():
    """The schema is only a mechanism if something enforces it before the engine sees an answer."""
    model = proposal_model(("f1",))
    with pytest.raises(ValidationError):
        model.model_validate({"current_understanding": "x", "current_directive": "y",
                              "accepted_next_action": "z", "next_plan": ["s"],
                              "carry_ids": ["invented"], "revise": [], "resolve": [],
                              "supersede": [], "demote": [], "archive": [], "new_items": [],
                              "knowledge_index": []})


# -- the prompt ----------------------------------------------------------------------


def test_the_prompt_says_this_is_a_transition_not_a_summary():
    """The distinction the whole design turns on, and the first thing the archived project's own
    prompt says. A model told to summarize will summarize."""
    assert "not conversation summarization" in UPDATE_SYSTEM
    assert "not a summary" in BOOTSTRAP_SYSTEM


def test_the_prompt_lists_the_ids_it_must_account_for():
    """The schema enumerates them too, but a model that can see which ones it must account for is
    less likely to omit one — and the omission is what the certificate exists to catch."""
    gateway = Gateway(answer())
    import asyncio

    asyncio.run(run_update(gateway, previous(), Episode(leaving="the messages")))
    assert "[f1]" in gateway.prompt and "[d1]" in gateway.prompt
    assert "the API is available" in gateway.prompt, "the statement is shown, not only the id"
    assert "the messages" in gateway.prompt


# -- the update ----------------------------------------------------------------------


def test_a_carried_item_survives_a_compression_byte_for_byte():
    """The property that answers "how is the text not distorted": the model chose to carry it, so
    nothing rewrote it."""
    import asyncio

    outcome = asyncio.run(run_update(Gateway(answer()), previous(),
                                     Episode(leaving="everything else")))
    carried = next(i for i in outcome.materialized.cognition.items() if i.id == "f1")
    assert carried.statement == "the API is available"


def test_the_account_of_what_happened_comes_from_what_was_applied():
    import asyncio

    outcome = asyncio.run(run_update(Gateway(answer()), previous(), Episode(leaving="x")))
    assert [(d.item_id, d.disposition) for d in outcome.materialized.certificate.dispositions] == \
        [("f1", "carry"), ("d1", "carry")]


def test_an_answer_that_accounts_for_nothing_is_rejected():
    """Reachable even though the ids are enumerated, because the schema requires every list and
    still permits every one of them to be empty. Coverage is what catches the omission."""
    import asyncio

    with pytest.raises(UpdateRejected) as caught:
        asyncio.run(run_update(Gateway(answer(carry_ids=())), previous(), Episode(leaving="x")))
    assert "coverage_incomplete" in str(caught.value)
    assert caught.value.problems[0].code == "coverage_incomplete"


def test_the_outcome_reports_the_ids_the_model_was_allowed_to_name():
    """Kept so a rejection can be read against the enum the model was given rather than against
    whatever exists now."""
    import asyncio

    outcome = asyncio.run(run_update(Gateway(answer()), previous(), Episode(leaving="x")))
    assert outcome.active_ids == ("d1", "f1")
    assert outcome.input_tokens == 100 and outcome.output_tokens == 50


def test_a_revision_replaces_the_statement_and_the_old_one_is_gone():
    import asyncio

    class Replacement:
        section = "experience.decisions"
        statement = "keep the interface narrow"
        sources = ("episode:2",)
        relevance = "changes the next action"
        evidence = ()

    class Revise:
        item_id = "d1"
        reason = "the user narrowed it further"
        replacement = Replacement()

    outcome = asyncio.run(run_update(Gateway(answer(carry_ids=("f1",), revise=(Revise(),))),
                                     previous(), Episode(leaving="x")))
    statements = {i.statement for i in outcome.materialized.cognition.items()}
    assert "keep the interface narrow" in statements
    assert "keep the adapter narrow" not in statements


def test_proposal_reading_is_total_over_the_validated_shape():
    """Every field the schema requires is read, so a new one cannot be silently dropped on the way
    from the answer into the engine's own type."""
    proposal = to_proposal(answer(carry_ids=("f1",)))
    assert proposal.current_understanding == "the adapter is ready"
    assert proposal.carry_ids == ("f1",)
    assert proposal.next_plan == ("run it",)
