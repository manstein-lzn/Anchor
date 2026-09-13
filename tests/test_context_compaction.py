"""When to compress, and what the projection looks like — without a framework.

Two of these tests are about refusal rather than action, and they matter more than the rest: an
unknown window fraction must not be acted on, and a history at the threshold with nothing worth
dropping must not be replaced by a state either. Both are cases where compressing would be easy to
do and would make the context worse.
"""

from __future__ import annotations

import pytest

from anchor.context_engine.cognition import Cognition, CognitionItem
from anchor.context_engine.compaction import (
    DEFAULT_THRESHOLD,
    Message,
    decide,
    projected,
    render_cognition,
    render_episode,
)


def history(n: int) -> tuple[Message, ...]:
    return tuple(Message("user" if i % 2 == 0 else "assistant", f"message {i}") for i in range(n))


def cognition() -> Cognition:
    return Cognition(
        situation={"confirmed_facts": [CognitionItem("f1", "the API is available", ("ep",), "r")],
                   "active_hypotheses": [], "unresolved_conflicts": [], "blockers": []},
        experience={"decisions": [CognitionItem("d1", "keep it narrow", ("ep",), "r")],
                    "failed_paths": []},
        intent={"current_directive": "finish the adapter",
                "accepted_next_action": "run the test", "open_questions": []})


# -- when ----------------------------------------------------------------------------


def test_below_the_threshold_nothing_happens():
    decision = decide(messages=history(20), context_window_used=0.5, keep_recent=6)
    assert decision.action == "keep"
    assert "below" in decision.reason


def test_at_the_threshold_it_compresses_and_splits_the_history():
    decision = decide(messages=history(20), context_window_used=DEFAULT_THRESHOLD,
                      keep_recent=6)
    assert decision.compressing
    assert len(decision.staying) == 6
    assert len(decision.leaving) == 14
    assert decision.leaving[-1].text == "message 13"
    assert decision.staying[0].text == "message 14"


def test_an_unknown_fraction_is_not_acted_on():
    """`None` means no response yet, or an unknown window. Choosing a moment on the strength of a
    number nobody has is exactly what this engine refuses elsewhere."""
    decision = decide(messages=history(20), context_window_used=None, keep_recent=6)
    assert decision.action == "keep"
    assert "unknown" in decision.reason
    assert decision.leaving == ()


def test_a_history_with_nothing_to_drop_is_left_alone():
    """Over the threshold with fewer messages than are kept would replace the whole history with a
    state — the case where a summary is most damaging and least useful."""
    decision = decide(messages=history(4), context_window_used=0.99, keep_recent=6)
    assert decision.action == "keep"
    assert str(len(history(4))) in decision.reason


def test_the_threshold_is_a_parameter_rather_than_a_law():
    assert decide(messages=history(20), context_window_used=0.6, keep_recent=6).action == "keep"
    assert decide(messages=history(20), context_window_used=0.6, keep_recent=6,
                  threshold=0.5).compressing


def test_exactly_keep_recent_messages_leaves_nothing_to_compress():
    decision = decide(messages=history(6), context_window_used=0.9, keep_recent=6)
    assert decision.action == "keep"


# -- what leaves ---------------------------------------------------------------------


def test_the_episode_is_everything_that_leaves_in_order():
    decision = decide(messages=history(10), context_window_used=0.9, keep_recent=4)
    episode = render_episode(decision)
    assert episode.index("message 0") < episode.index("message 5")
    assert "message 6" not in episode, "a kept message is not part of what leaves"
    assert "[user]" in episode and "[assistant]" in episode


# -- what the projection says --------------------------------------------------------


def test_the_projection_is_the_state_then_the_recent_messages():
    decision = decide(messages=history(20), context_window_used=0.9, keep_recent=6)
    result = projected(decision, cognition())
    assert len(result) == 7
    assert "the API is available" in result[0].text
    assert [m.text for m in result[1:]] == [f"message {i}" for i in range(14, 20)]


def test_the_state_is_labelled_as_a_record_rather_than_an_instruction():
    """It is derived from what the agent did and read. Arriving with a system prompt's authority
    would let a retrieved document steer the agent, which is the injection the framework's own
    documentation warns about."""
    text = render_cognition(cognition())
    assert "record, not an instruction" in text
    assert "recoverable by reference" in text


def test_items_travel_with_their_ids_so_a_reader_can_ask_for_more():
    """An id is what makes the detail addressable. A projection that described its contents without
    naming them would leave the agent unable to ask for what it needs."""
    text = render_cognition(cognition())
    assert "[id: f1]" in text and "[id: d1]" in text
    assert "the API is available" in text and "keep it narrow" in text


def test_the_directive_and_the_next_action_are_separated():
    """They are different things, and a projection that ran them together would let the agent act
    on the wrong one."""
    text = render_cognition(cognition())
    assert "Current directive: finish the adapter" in text
    assert "Accepted next action: run the test" in text


def test_stored_detail_is_named_by_reference():
    from anchor.context_engine.cognition import KnowledgeReference

    state = cognition()
    with_index = Cognition(situation=state.situation, experience=state.experience,
                           intent=state.intent,
                           knowledge_index=(KnowledgeReference(
                               id="ref-1", cue="a failed approach",
                               locator="artifact://sha256/" + "a" * 64, source="episode:2"),))
    text = render_cognition(with_index)
    assert "Stored detail, by reference:" in text
    assert "a failed approach" in text and "artifact://sha256/" in text


def test_projection_refuses_a_decision_that_is_not_compressing():
    """Returning the history unchanged from a function called `projected` would hide the case where
    nothing was decided, and the caller would not know which happened."""
    decision = decide(messages=history(20), context_window_used=0.1, keep_recent=6)
    with pytest.raises(ValueError):
        projected(decision, cognition())


def test_an_empty_cognition_still_produces_a_readable_projection():
    empty = Cognition(situation={"confirmed_facts": [], "active_hypotheses": [],
                                 "unresolved_conflicts": [], "blockers": []},
                      experience={"decisions": [], "failed_paths": []},
                      intent={"open_questions": []})
    text = render_cognition(empty)
    assert "record, not an instruction" in text
    assert text.strip(), "a projection with nothing to say is still a projection"


# -- the adapter ---------------------------------------------------------------------


def test_the_adapter_compresses_once_and_then_reuses_the_state():
    """"Compress once and reuse" is the design. A compactor that rebuilt the state on every request
    would pay a model call per request and would lose the point of having a state at all."""
    import asyncio

    from anchor.runtime.context_compaction import Compactor

    calls = []

    class Gateway:
        async def generate_structured(self, *, prompt, system_prompt, output_type):
            calls.append(prompt)

            class Answer:
                current_understanding, current_directive = "done", "finish"
                accepted_next_action, next_plan = "run", ("run",)
                carry_ids = tuple(dict.fromkeys(__import__("re").findall(r"\[(\w+)\]", prompt)))
                revise = resolve = supersede = demote = archive = new_items = ()
                knowledge_index = ()

            class Response:
                text, input_tokens, output_tokens = "{}", 1, 1

            return Answer(), Response()

    compactor = Compactor(Gateway(), run_id="r", node_id="plan", keep_recent=2)

    class Ctx:
        context_window_used = 0.9

    first = asyncio.run(compactor(Ctx(), [ _raw_message(i) for i in range(6)]))
    assert compactor.compressions == 1
    assert len(first) == 3, "one state message and two kept"
    assert compactor.cognition is not None
    again = asyncio.run(compactor(Ctx(), [ _raw_message(i) for i in range(8)]))
    assert again[0].parts[0].content.startswith("[anchor]")
    assert compactor.compressions == 2


def _raw_message(index: int):
    class Part:
        content = f"message {index}"

    class Raw:
        parts = [Part()]

    return Raw()


def test_the_adapter_leaves_the_history_alone_below_the_threshold():
    import asyncio

    from anchor.runtime.context_compaction import Compactor

    class Gateway:
        async def generate_structured(self, **kwargs):  # pragma: no cover - must not be reached
            raise AssertionError("the model must not be called below the threshold")

    compactor = Compactor(Gateway(), keep_recent=2)
    original = [_raw_message(i) for i in range(6)]

    class Ctx:
        context_window_used = 0.2

    assert asyncio.run(compactor(Ctx(), original)) is original
    assert compactor.compressions == 0


def test_an_unknown_fraction_never_reaches_the_model():
    import asyncio

    from anchor.runtime.context_compaction import Compactor

    class Gateway:
        async def generate_structured(self, **kwargs):  # pragma: no cover
            raise AssertionError("no moment can be chosen without a window fraction")

    compactor = Compactor(Gateway(), keep_recent=2)

    class Ctx:
        context_window_used = None

    original = [_raw_message(i) for i in range(6)]
    assert asyncio.run(compactor(Ctx(), original)) is original


def test_a_failed_compression_leaves_the_history_alone():
    """The honest response. Sending a state the certificate refused would be worse than sending a
    long history, and the next request can try again."""
    import asyncio

    from anchor.runtime.context_compaction import Compactor

    class Gateway:
        async def generate_structured(self, **kwargs):
            raise RuntimeError("the provider is having a day")

    compactor = Compactor(Gateway(), keep_recent=2)

    class Ctx:
        context_window_used = 0.95

    original = [_raw_message(i) for i in range(6)]
    assert asyncio.run(compactor(Ctx(), original)) is original
    assert compactor.failures == 1 and compactor.compressions == 0


def test_a_rejected_compression_is_distinguished_from_a_failed_one():
    """Both leave the history alone, and they mean different things: one says the engine built a
    state its own rules refuse, the other says the provider was unreachable."""
    import asyncio

    from anchor.context_engine.cognition import CognitionItem
    from anchor.runtime.context_compaction import Compactor

    class Gateway:
        async def generate_structured(self, *, prompt, system_prompt, output_type):
            class Answer:
                current_understanding, current_directive = "x", "y"
                accepted_next_action, next_plan = "z", ("s",)
                carry_ids = ()          # account for nothing
                revise = resolve = supersede = demote = archive = new_items = ()
                knowledge_index = ()

            class Response:
                text, input_tokens, output_tokens = "{}", 1, 1

            return Answer(), Response()

    compactor = Compactor(Gateway(), keep_recent=2)
    compactor.cognition = Cognition(
        situation={"confirmed_facts": [CognitionItem("f1", "x", ("ep",), "r")],
                   "active_hypotheses": [], "unresolved_conflicts": [], "blockers": []},
        experience={"decisions": [], "failed_paths": []}, intent={"open_questions": []})

    class Ctx:
        context_window_used = 0.95

    original = [_raw_message(i) for i in range(6)]
    assert asyncio.run(compactor(Ctx(), original)) is original
    assert compactor.failures == 1
    assert compactor.cognition.item_ids() == {"f1"}, "a rejected compression changes nothing"
