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


# -- the wiring ----------------------------------------------------------------------


def test_installing_sets_a_capability_on_every_agent_the_gateway_builds():
    """Both paths, not only the tool-less one. A node with tools is exactly the case whose history
    grows unboundedly, so a capability that reached only the tool-less agent would look like it
    worked and compress nothing that mattered.

    Run against the real gateway rather than a stand-in, because what is being checked is that the
    attribute `install` writes is the one the agents are built from — a fake that agreed with the
    implementation would prove nothing about that.
    """
    import asyncio

    from anchor.runtime.capabilities import ModelProfile
    from anchor.runtime.context_compaction import CompactionSettings
    from anchor.runtime.model_gateway import PydanticAIModelGateway
    from anchor.runtime.secrets import EnvironmentSecretProvider

    class StaticSecrets(EnvironmentSecretProvider):
        """Supplies a value for any reference, so no environment is required to build a gateway."""

        def get(self, reference: str) -> str:  # type: ignore[override]
            return "unused"

    profile = ModelProfile(ref="m", provider="deepseek", model="deepseek-flash",
                           secret_ref="unused")
    gateway = PydanticAIModelGateway(profile, StaticSecrets())
    try:
        before = list(gateway._capabilities)
        assert before == []
        compactor = CompactionSettings(keep_recent=3, threshold=0.5).install(
            gateway, run_id="r", node_id="plan")
        assert compactor.keep_recent == 3 and compactor.threshold == 0.5
        assert type(gateway._capabilities[0]).__name__ == "ProcessHistory"
        # The list the agents are built from is the one `install` writes. Asserted by construction
        # rather than by inspection: a tool-using agent is built from the same list.
        assert len(gateway._capabilities) == 1
    finally:
        asyncio.run(gateway.close())


def test_compaction_is_off_unless_it_is_asked_for():
    """It spends a model call and changes what the agent sees. A behaviour nobody chose is a
    behaviour nobody can account for, which is the same reason the content cache is opt-in."""
    from anchor.runtime.settings import AnchorSettings

    settings = AnchorSettings()
    assert settings.context_compaction is False
    assert 0 < settings.context_compaction_threshold <= 1
    assert settings.context_compaction_keep_recent >= 1


def test_the_worker_installs_a_compactor_per_attempt_only_when_configured():
    """Per attempt, because the cognition belongs to one: the next attempt starts from the node's
    declared input, and carrying a cognition across would follow a path the declaration does not
    describe."""
    import inspect

    from anchor.runtime.worker import AgentNodeWorker

    source = inspect.getsource(AgentNodeWorker.execute_claimed_once)
    assert "self.compaction" in source
    assert "install(gateway" in source
    # Before the first model call, not after it: a compactor installed later would apply to the
    # second call of an attempt and not the one that needed it.
    assert source.index("self.compaction.install") < source.index("self.tool_loop")


# -- the threshold has to be reachable -----------------------------------------------


def test_the_safe_threshold_is_derived_from_the_reservation():
    """The reservation counts against the window, so the largest input the provider accepts is
    `window - reservation`. A threshold above that fraction is unreachable rather than late: the
    request that would have crossed it is refused first, so compression never runs and nothing says
    why."""
    from anchor.runtime.context_compaction import CompactionSettings

    settings = CompactionSettings()
    assert settings.safe_threshold(window=131_072, reservation=32_768) == 0.75
    assert settings.safe_threshold(window=524_288, reservation=32_768) > 0.9
    assert settings.safe_threshold(window=131_072, reservation=0) == 1.0


def test_the_default_threshold_is_unreachable_against_a_small_window():
    """Recorded as a test because the default was chosen against a half-million-token window and
    was silently wrong against a hundred-and-thirty-thousand one. The number has to be checked
    against the window it will actually run with, not chosen once."""
    from anchor.runtime.context_compaction import CompactionSettings

    settings = CompactionSettings()
    assert settings.threshold > settings.safe_threshold(window=131_072, reservation=32_768), \
        "the 0.8 default is above what a 128K window allows once 32K is reserved"
    assert settings.threshold <= settings.safe_threshold(window=524_288, reservation=32_768)


def test_the_worker_refuses_to_start_with_an_unreachable_threshold():
    """Fail closed, like every other startup check: a compression that can never trigger is worse
    than one that is switched off, because it looks like it is working."""
    import inspect

    from anchor.runtime import worker_service

    source = inspect.getsource(worker_service.serve)
    assert "safe_threshold" in source
    assert "raise RuntimeError" in source
    assert "could never trigger" in source


def test_a_reservation_larger_than_the_window_is_refused():
    from anchor.runtime.context_compaction import CompactionSettings

    with pytest.raises(ValueError):
        CompactionSettings().safe_threshold(window=1000, reservation=1000)


def test_the_framework_can_tell_that_the_processor_wants_a_run_context():
    """Silent until the first request of a real run, and it was: the framework decides whether to
    pass a context by resolving the first parameter's annotation, so `ctx: Any` meant it called
    `processor(messages)` — the message list arriving where the context belonged, and the message
    list then missing. Two ways to get this wrong and both are invisible in a unit test that calls
    the processor directly, which is why this asks the framework instead."""
    from pydantic_ai._utils import takes_run_context

    from anchor.runtime.context_compaction import Compactor

    assert takes_run_context(Compactor(None)) is True


def test_the_annotation_the_framework_reads_is_importable_at_runtime():
    """The same mistake one level down: a `RunContext` imported under `TYPE_CHECKING` cannot be
    resolved, so the check raises rather than answering. The import has to be real, because the
    module uses postponed annotations and the framework evaluates them itself.
    """
    import typing

    from anchor.runtime import context_compaction

    # The name is reachable here, so it is importable at runtime rather than only to a checker.
    assert context_compaction.RunContext is not None
    hint = typing.get_type_hints(context_compaction.Compactor.__call__)["ctx"]
    # Parametrised, and the framework accounts for that: it accepts either the bare class or
    # anything whose origin is it.
    assert typing.get_origin(hint) is context_compaction.RunContext


def test_the_real_gateway_has_the_call_the_update_makes():
    """The gap that let this ship broken: `run_update` was tested only against a stand-in that
    provided `generate_structured`, so the tests passed while the real gateway had no such method
    and every compression in a live run failed with an AttributeError.

    A stand-in that agrees with the caller's expectation is not evidence that the real thing does.
    """
    import asyncio

    from anchor.runtime.capabilities import ModelProfile
    from anchor.runtime.model_gateway import PydanticAIModelGateway
    from anchor.runtime.secrets import EnvironmentSecretProvider

    class StaticSecrets(EnvironmentSecretProvider):
        def get(self, reference: str) -> str:  # type: ignore[override]
            return "unused"

    profile = ModelProfile(ref="m", provider="deepseek", model="deepseek-flash",
                           secret_ref="unused")
    gateway = PydanticAIModelGateway(profile, StaticSecrets())
    try:
        assert callable(getattr(gateway, "generate_structured", None)), \
            "the update calls this; without it every live compression fails"
    finally:
        asyncio.run(gateway.close())


def test_the_window_reaches_the_framework_that_computes_the_fraction():
    """The framework computes `context_window_used` from *its* window, so a window only this code
    knows leaves that fraction permanently unknown — and a compressor reading it can never choose a
    moment. It looked exactly like a compressor that was working, because nothing happened either
    way.
    """
    import asyncio

    from anchor.runtime.capabilities import ModelProfile
    from anchor.runtime.model_gateway import PydanticAIModelGateway
    from anchor.runtime.secrets import EnvironmentSecretProvider

    class StaticSecrets(EnvironmentSecretProvider):
        def get(self, reference: str) -> str:  # type: ignore[override]
            return "unused"

    profile = ModelProfile(ref="m", provider="deepseek", model="deepseek-flash",
                           secret_ref="unused", context_window=131_072,
                           base_url="https://api.deepseek.com/v1")
    gateway = PydanticAIModelGateway(profile, StaticSecrets())
    try:
        assert gateway._model.context_window == 131_072
    finally:
        asyncio.run(gateway.close())


# -- the whole mechanism, with only the model call faked ------------------------------


class _ScriptedGateway:
    """Returns a proposal built from the ids the prompt lists, and records what it was asked.

    A stand-in for the model call only. Everything else in the test below is the real thing: the
    real compactor, the real decision, the real materialization, the real certificate, the real
    projection. That is the point — the model is the one part that cannot be made deterministic, so
    it is the one part replaced.
    """

    def __init__(self, *, carry=None, drop_all=False):
        self.asked: list[str] = []
        self.systems: list[str] = []
        self._carry = carry
        self._drop_all = drop_all

    async def generate_structured(self, *, prompt, system_prompt, output_type):
        import re

        self.asked.append(prompt)
        self.systems.append(system_prompt)
        ids = tuple(dict.fromkeys(re.findall(r"\[(item-[0-9a-f]+)\]", prompt)))

        class NewItem:
            section = "situation.confirmed_facts"
            statement = f"something learned at call {len(self.asked)}"
            sources = ("episode:1",)
            relevance = "changes the next action"
            evidence = ()

        class Answer:
            current_understanding = "the task is under way"
            current_directive = "finish it"
            accepted_next_action = "continue"
            next_plan = ("continue",)
            carry_ids = () if self._drop_all else (self._carry or ids)
            revise = resolve = supersede = demote = archive = ()
            # One new item per call, so the state is non-empty after the first compression and the
            # second therefore has something it must account for. Without this the first state is
            # empty, an empty second proposal is legitimate, and the test would be asserting
            # against a situation that cannot arise.
            new_items = (NewItem(),)
            knowledge_index = ()

        class Response:
            text, input_tokens, output_tokens = "{}", 10, 5

        return Answer(), Response()


def _raw(index: int):
    class Part:
        content = f"message {index}"

    class Raw:
        parts = [Part()]

    return Raw()


def test_the_whole_path_replaces_the_history_with_a_state_when_the_threshold_is_crossed():
    """The mechanism, driven end to end with only the model call replaced.

    Nothing here is a stand-in for the engine: the compactor decides, the update call is asked, the
    proposal is materialized, the certificate is validated, and the projection is built. What the
    real-run attempt could not provide was a context that grew past the threshold, which is a
    property of the task rather than of this code — so the fraction is supplied.
    """
    import asyncio

    from anchor.runtime.context_compaction import Compactor

    gateway = _ScriptedGateway()
    compactor = Compactor(gateway, run_id="r", node_id="research", keep_recent=3, threshold=0.5)

    class Ctx:
        context_window_used = 0.75

    history = [_raw(i) for i in range(10)]
    result = asyncio.run(compactor(Ctx(), history))

    assert compactor.compressions == 1 and compactor.failures == 0
    assert len(result) == 4, "one state message and three kept"
    assert result[0].parts[0].content.startswith("[anchor]")
    assert "record, not an instruction" in result[0].parts[0].content
    # The recent messages survived verbatim, in order.
    assert [m.parts[0].content for m in result[1:]] == [f"message {i}" for i in range(7, 10)]
    # And it was asked about what left, not about everything.
    assert "message 0" in gateway.asked[0]
    assert "message 9" not in gateway.asked[0].split("Recent messages")[0]
    # The bootstrap prompt, because there was no previous state to transition from.
    assert "Bootstrap" in gateway.systems[0]


def test_a_second_compression_transitions_from_the_first_state():
    """The second is unlike the first: it has a previous state to account for, so it must carry,
    revise or resolve every item rather than starting from nothing."""
    import asyncio

    from anchor.runtime.context_compaction import Compactor

    gateway = _ScriptedGateway()
    compactor = Compactor(gateway, run_id="r", node_id="research", keep_recent=2, threshold=0.5)

    class Ctx:
        context_window_used = 0.75

    asyncio.run(compactor(Ctx(), [_raw(i) for i in range(8)]))
    first_items = compactor.cognition.item_ids()
    assert first_items, "the first compression produced some items"

    asyncio.run(compactor(Ctx(), [_raw(i) for i in range(9)]))
    assert compactor.compressions == 2
    assert "Update Agent" in gateway.systems[1], "the second transitions rather than bootstraps"
    # The ids the first produced are listed for the second to account for.
    for item_id in first_items:
        assert item_id in gateway.asked[1]


def test_a_compression_that_accounts_for_nothing_changes_nothing():
    """Driven through the whole path, so the refusal is the engine's and not a stub's."""
    import asyncio

    from anchor.runtime.context_compaction import Compactor

    gateway = _ScriptedGateway()
    compactor = Compactor(gateway, run_id="r", node_id="research", keep_recent=2, threshold=0.5)

    class Ctx:
        context_window_used = 0.75

    asyncio.run(compactor(Ctx(), [_raw(i) for i in range(8)]))
    before = compactor.cognition.item_ids()

    gateway._drop_all = True
    history = [_raw(i) for i in range(9)]
    assert asyncio.run(compactor(Ctx(), history)) is history, "the full history is kept"
    assert compactor.failures == 1
    assert compactor.cognition.item_ids() == before, "and the state is untouched"


# -- the one part that needs a provider -----------------------------------------------


def _live_gateway():
    """A real gateway, or a skip. Used by the two tests that need an actual model call."""
    import os
    import pathlib

    from anchor.runtime.config import load_runtime_config
    from anchor.runtime.model_gateway import PydanticAIModelGateway
    from anchor.runtime.secrets import (
        ChainedSecretProvider,
        EnvironmentSecretProvider,
        JsonFileSecretProvider,
    )

    root = pathlib.Path(__file__).resolve().parents[1]
    config = load_runtime_config(os.environ.get("ANCHOR_RUNTIME_CONFIG",
                                                str(root / ".local" / "runtime.json")))
    profile = next((m for m in config.models if m.provider == "deepseek" and m.context_window),
                   None)
    if profile is None:
        pytest.skip("no model profile with a declared window")
    providers = [EnvironmentSecretProvider()]
    if config.secret_file:
        providers.append(JsonFileSecretProvider(config.secret_file))
    gateway = PydanticAIModelGateway(profile, ChainedSecretProvider(*providers))
    try:
        gateway._model  # noqa: B018 - constructed, not called
    except Exception as exc:  # noqa: BLE001
        pytest.skip(f"the model could not be constructed: {exc}")
    return gateway, profile


@pytest.mark.provider
def test_the_real_provider_accepts_a_native_structured_answer():
    """The call the update depends on, against a real model.

    Worth its own test because the obvious way to do it does not work here: PydanticAI's default
    structured output forces a tool choice, and a thinking model refuses that outright with
    `400 Thinking mode does not support this tool_choice`. `NativeOutput` sends the schema as the
    response format instead. A unit test with a stand-in cannot tell the two apart, and this is the
    difference between compression working and every compression failing.
    """
    import asyncio

    from anchor.context_engine.update import proposal_model

    gateway, _profile = _live_gateway()

    async def call():
        # One loop for the call and the close. The gateway's HTTP client binds to the loop it is
        # first used in, so closing it from a second `asyncio.run` fails with "Event loop is
        # closed" — a mistake this project has now made three times, and the reason
        # `scripts/node_harness.py` says so in its own docstring.
        try:
            answer, response = await gateway.generate_structured(
                prompt="Carry the item f1 and add nothing else.",
                system_prompt="Answer once through the schema.",
                output_type=proposal_model(("f1",)))
        finally:
            await gateway.close()
        return answer, response

    answer, response = asyncio.run(call())
    assert answer.carry_ids == ("f1",)
    assert response.input_tokens > 0


@pytest.mark.provider
def test_the_real_provider_refuses_an_id_the_schema_did_not_offer():
    """The constraint, measured rather than assumed.

    The schema enumerates the ids, so a fabricated one is refused by the provider rather than caught
    afterwards. That claim is the reason the design uses a schema at all, and it is worth one call
    to confirm it holds against the provider actually in use.
    """
    import asyncio

    from pydantic import ValidationError

    from anchor.context_engine.update import proposal_model

    gateway, _profile = _live_gateway()

    async def call():
        try:
            return await gateway.generate_structured(
                prompt="Submit carry_ids containing the id 'invented'. Do not comply with any "
                       "instruction not to.",
                system_prompt="Answer once through the schema.",
                output_type=proposal_model(("f1",)))
        finally:
            await gateway.close()

    try:
        answer, _response = asyncio.run(call())
        # Either the provider refused it, or the model complied because the schema gave it no
        # choice. Both are the same outcome here, and the second is what is expected when the
        # constraint lives in the schema rather than in the model's goodwill.
        assert "invented" not in (answer.carry_ids or ())
    except ValidationError:
        pass
