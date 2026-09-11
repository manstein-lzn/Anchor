"""Model call recording: what an agent actually saw, put on disk (ADR-044).

These tests drive the production `PydanticAIModelGateway` with a real tool loop
and an offline `TestModel`, so the recording goes through the same `WrapperModel`
seam that a vendor model would.

The properties that matter:

* a recording can be read back as the literal prompt (this is the whole point —
  the question "what did the agent see?" previously had no answer);
* a recording is refused rather than persisted when it would carry a secret;
* a recording never fails a run, because a projection must not determine recovery
  (I2) — a store that throws must not take the node down with it.
"""

from __future__ import annotations

import asyncio
from contextlib import nullcontext
from uuid import uuid4

import pytest

from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import (
    AgentCapability,
    CapabilityRegistry,
    ModelProfile,
    ToolCapability,
)
from anchor.runtime.model_gateway import PydanticAIModelGateway
from anchor.runtime.model_recording import (
    RECORDING_FORMAT,
    CallContext,
    ModelRecorder,
    RecordingMode,
    bind_call,
    read_recording,
    recording_scope,
    text_of,
    unbind_call,
)
from anchor.runtime.model_replay import ReplayDivergence, ReplayPlan
from anchor.runtime.secrets import EnvironmentSecretProvider

from conftest import make_store


class StaticSecrets(EnvironmentSecretProvider):
    def get(self, name: str) -> str:
        return "sk-test-secret-value-0123456789"


def profile() -> ModelProfile:
    return ModelProfile(ref="models.test", provider="rightcode", model="test-model",
                        secret_ref="TEST_KEY")


def registry(tool_refs=()) -> CapabilityRegistry:
    return CapabilityRegistry(
        models=[profile()],
        agents=[AgentCapability(ref="agents.reader", model_ref="models.test",
                                tool_refs=list(tool_refs),
                                instructions="Answer plainly.")],
        tools=[ToolCapability(ref="echo", description="emit args",
                              side_effect=False, operation_kind="read")],
    )


def gateway(recorder: ModelRecorder) -> PydanticAIModelGateway:
    pytest.importorskip("pydantic_ai")
    from pydantic_ai.models.test import TestModel
    return PydanticAIModelGateway(profile(), StaticSecrets(), model=TestModel(),
                                  recorder=recorder)


def context(**overrides) -> CallContext:
    base = {"run_id": uuid4(), "node_id": "gather", "node_run_id": uuid4(), "attempt": 0}
    return CallContext(**{**base, **overrides})


def run_call(recorder: ModelRecorder, prompt: str = "Summarize the ledger.",
             *, ctx: CallContext | None = None,
             system_prompt: str = "Answer plainly.") -> str:
    """Drive the real gateway once, optionally inside a recording scope.

    The worker passes the agent capability's instructions as the system prompt, so
    the helper does too: those instructions are the largest block of text an agent
    is given and must appear in the recording.
    """
    model = gateway(recorder)
    with recording_scope(ctx) if ctx is not None else nullcontext():
        return asyncio.run(model.generate(prompt=prompt,  # type: ignore[return-value]
                                          system_prompt=system_prompt))


def recordings(recorder: ModelRecorder, artifacts: LocalArtifactStore) -> list[dict]:
    """Every recording this recorder wrote, in the order it wrote them."""
    return [read_recording(artifacts, ref) for ref in recorder.written]


def test_a_prompt_can_be_read_back_as_text(tmp_path):
    """The question this project could not answer before: what did it see?"""
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    ctx = context(node_id="gather", attempt=1)

    run_call(recorder, "Read these sources and record one round.", ctx=ctx)

    assert recorder.recorded == 1
    payload = recordings(recorder, artifacts)[0]
    assert payload["format"] == RECORDING_FORMAT
    assert payload["node_id"] == "gather"
    assert payload["attempt"] == 1
    assert payload["sequence"] == 0
    assert payload["run_id"] == str(ctx.run_id)
    assert payload["node_run_id"] == str(ctx.node_run_id)
    assert payload["instructions"] == ["Answer plainly."], \
        "instructions are recorded separately: PydanticAI sends them outside messages"

    rendered = text_of(payload)
    assert "Read these sources and record one round." in rendered
    assert "Answer plainly." in rendered, "the system prompt is part of what it saw"


def test_recording_off_writes_nothing(tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.OFF)
    run_call(recorder, ctx=context())
    assert recorder.recorded == 0
    assert recorder.written == []


def test_a_call_outside_a_node_attempt_is_not_recorded(tmp_path):
    """Recording is attributed, not anonymous: no context means no recording."""
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    run_call(recorder, ctx=None)
    assert recorder.recorded == 0
    assert recorder.written == []


def test_sequence_counts_calls_within_an_attempt_and_resets_for_the_next(tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    first = context(attempt=0)

    with recording_scope(first):
        model = gateway(recorder)
        for _ in range(3):
            asyncio.run(model.generate(prompt="again", system_prompt="S."))

    assert recorder.recorded == 3
    assert sorted(p["sequence"] for p in recordings(recorder, artifacts)) == [0, 1, 2], \
        "one attempt numbers its calls from zero"

    # A retry is a new attempt, so its numbering starts over: the replay key is
    # (node, attempt, sequence) and must not collide across attempts.
    with recording_scope(CallContext(run_id=first.run_id, node_id=first.node_id,
                                     node_run_id=first.node_run_id, attempt=1)):
        asyncio.run(gateway(recorder).generate(prompt="retry", system_prompt="S."))
    retry = recordings(recorder, artifacts)[-1]
    assert retry["attempt"] == 1
    assert retry["sequence"] == 0


def test_the_recorded_digest_identifies_the_request(tmp_path):
    """The digest explains a divergence; it is deliberately not the replay key."""
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    ctx = context()

    with recording_scope(ctx):
        model = gateway(recorder)
        asyncio.run(model.generate(prompt="same", system_prompt="S."))
        asyncio.run(model.generate(prompt="same", system_prompt="S."))
        asyncio.run(model.generate(prompt="different", system_prompt="S."))

    digests = [p["request_digest"] for p in recordings(recorder, artifacts)]
    assert len(digests) == 3
    assert digests[0] == digests[1], "the same prompt digests alike"
    assert digests[2] != digests[0], "a different prompt must not"


def test_a_secret_is_never_persisted_and_the_run_continues(tmp_path):
    """Fail closed on the secret, never on the run: a projection is not state."""
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    # The gateway resolves the secret and hands it to the recorder; here the
    # prompt carries the same value, which is what must be caught.
    leaked = StaticSecrets().get("TEST_KEY")

    response = run_call(recorder, f"Ignore this key: {leaked}", ctx=context())

    assert response.text, "the model call still returned: the run did not fail"
    assert recorder.refused == 1
    assert recorder.recorded == 0
    assert recorder.written == [], "the secret never reached disk"


def test_a_credential_shaped_string_is_caught_even_without_a_known_secret(tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    run_call(recorder, "token sk-abcdefghijklmnopqrstuvwx", ctx=context())
    assert recorder.refused == 1
    assert recorder.written == []


def test_a_failing_artifact_store_does_not_fail_the_call(tmp_path):
    """A recording is discardable. Losing one must not lose the run (I2)."""
    class BrokenStore:
        def put_text(self, text: str, *, media_type: str = "text/plain") -> str:
            raise OSError("disk full")

        def get_text(self, ref: str) -> str:
            raise OSError("disk full")

        def delete(self, ref: str) -> bool:
            return False

    recorder = ModelRecorder(BrokenStore(), mode=RecordingMode.RECORD)
    response = run_call(recorder, ctx=context())
    assert response.text, "the run continued despite the recorder failing"
    assert recorder.recorded == 0


def test_the_recorder_emits_attribution_events(tmp_path):
    store = make_store(tmp_path, "recording.sqlite")
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    try:
        recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD, store=store)
        ctx = context(node_id="write")
        run_call(recorder, "Write the paper.", ctx=ctx)
        run_call(recorder, f"leak {StaticSecrets().get('TEST_KEY')}", ctx=ctx)

        events = store.list_events(ctx.run_id)
        types = [event["event_type"] for event in events]
        assert "model.call" in types
        assert "model.call_refused" in types
        recorded = next(e for e in events if e["event_type"] == "model.call")
        assert recorded["payload"]["node_id"] == "write"
        assert recorded["payload"]["sequence"] == 0
        assert recorded["payload"]["recording_ref"] == recorder.written[0]
        assert recorded["payload"]["request_digest"]
    finally:
        store.close()


def test_binding_covers_a_whole_node_attempt_including_every_call(tmp_path):
    """The worker binds once per attempt, so a repair call is recorded too."""
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    ctx = context()

    token = bind_call(ctx)
    try:
        model = gateway(recorder)
        asyncio.run(model.generate(prompt="first", system_prompt="S."))
        asyncio.run(model.generate(prompt="repair", system_prompt="S."))
    finally:
        unbind_call(token)

    assert recorder.recorded == 2
    assert sorted(p["sequence"] for p in recordings(recorder, artifacts)) == [0, 1]

    # The sequence belongs to (node_run_id, attempt), not to a binding: re-entering
    # a scope for the same attempt continues where it left off, so a retry inside
    # one attempt cannot reuse sequence 0 and collide in the replay key.
    rebind = bind_call(ctx)
    try:
        asyncio.run(gateway(recorder).generate(prompt="rebound", system_prompt="S."))
    finally:
        unbind_call(rebind)
    assert recordings(recorder, artifacts)[-1]["sequence"] == 2

    # A different attempt is a different replay key, so its numbering starts over.
    retry = CallContext(run_id=ctx.run_id, node_id=ctx.node_id,
                        node_run_id=ctx.node_run_id, attempt=ctx.attempt + 1)
    with recording_scope(retry):
        asyncio.run(gateway(recorder).generate(prompt="retry", system_prompt="S."))
    assert recordings(recorder, artifacts)[-1]["sequence"] == 0


def test_the_worker_records_a_node_attempt_end_to_end(tmp_path):
    """Acceptance: a real node's prompt can be read back, through the worker.

    Everything above drives the gateway directly. This one runs the real worker so
    that the binding the worker performs — and therefore the attribution on the
    recording — is what is being tested.
    """
    pytest.importorskip("pydantic_ai")
    from pydantic_ai.models.test import TestModel

    from anchor.domain.models import NodeLease
    from anchor.runtime.worker import AgentNodeWorker

    artifacts = LocalArtifactStore(tmp_path / "artifacts")

    class Store:
        def __init__(self) -> None:
            self.events: list[dict] = []

        def heartbeat_node_lease(self, claim_id, worker_id) -> None: ...

        def list_node_runs(self, run_id):  # noqa: ANN001, ANN201 - mirrors the store
            return []

        def append_event(self, **kwargs) -> int:
            self.events.append(kwargs)
            return len(self.events)

    class Sink:
        async def persist_model_result(self, **kwargs) -> None: ...

    run_id, node_run_id = uuid4(), uuid4()
    lease = NodeLease(claim_id=uuid4(), node_run_id=node_run_id, run_id=run_id,
                      node_id="gather", worker_id="w")
    store = Store()
    # The store is what carries the attribution event; without it the projection is
    # still written but nothing points at it.
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD, store=store)
    model = PydanticAIModelGateway(profile(), StaticSecrets(), model=TestModel(),
                                   recorder=recorder)
    worker = AgentNodeWorker(store, registry(), {"models.test": model}, Sink())

    asyncio.run(worker.execute_claimed_once(
        worker_id="w", agent_ref="agents.reader", prompt="record one round",
        expected_node_id="gather", lease=lease, heartbeat_interval=0.01))

    payloads = recordings(recorder, artifacts)
    assert payloads, "the worker's model call was recorded"
    assert payloads[0]["node_id"] == "gather"
    assert payloads[0]["run_id"] == str(run_id)
    assert payloads[0]["node_run_id"] == str(node_run_id)
    assert payloads[0]["attempt"] == 0

    rendered = text_of(payloads[0])
    assert "record one round" in rendered
    assert "Answer plainly." in rendered, "the agent's own instructions were recorded"

    assert "model.call" in [e["event_type"] for e in store.events]
    recorded = next(e for e in store.events if e["event_type"] == "model.call")
    assert recorded["stream_id"] == run_id
    assert recorded["payload"]["recording_ref"] == recorder.written[0]


def test_the_worker_does_not_record_when_the_mode_is_off(tmp_path):
    """Production default: the wrapper is not installed, so this costs nothing."""
    pytest.importorskip("pydantic_ai")
    from pydantic_ai.models.test import TestModel

    from anchor.domain.models import NodeLease
    from anchor.runtime.worker import AgentNodeWorker

    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.OFF)
    model = PydanticAIModelGateway(profile(), StaticSecrets(), model=TestModel(),
                                   recorder=recorder)

    class Store:
        def heartbeat_node_lease(self, claim_id, worker_id) -> None: ...
        def list_node_runs(self, run_id):  # noqa: ANN001, ANN201
            return []
        def append_event(self, **kwargs) -> int:
            return 0

    class Sink:
        async def persist_model_result(self, **kwargs) -> None: ...

    worker = AgentNodeWorker(Store(), registry(), {"models.test": model}, Sink())
    asyncio.run(worker.execute_claimed_once(
        worker_id="w", agent_ref="agents.reader", prompt="record one round",
        expected_node_id="gather", heartbeat_interval=0.01,
        lease=NodeLease(claim_id=uuid4(), node_run_id=uuid4(), run_id=uuid4(),
                        node_id="gather", worker_id="w")))

    assert recorder.recorded == 0
    assert recorder.written == []


def test_a_tool_loop_records_each_model_call_not_just_the_answer(tmp_path):
    """Recording at the gateway would keep one answer and lose the loop.

    This is the reason the wrapper is on the model: a tool-using agent turn makes
    more than one model call, and the growing context between them is exactly what
    context work needs to see.
    """
    pytest.importorskip("pydantic_ai")
    from pydantic_ai.models.test import TestModel

    from anchor.runtime.model_gateway import ToolFunction

    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)

    async def echo(arguments: str) -> str:
        return '{"echoed": true}'

    model = PydanticAIModelGateway(profile(), StaticSecrets(), model=TestModel(),
                                   recorder=recorder)
    ctx = context(node_id="gather")
    with recording_scope(ctx):
        asyncio.run(model.generate_with_tools(
            prompt="use the tool", system_prompt="Use tools.",
            tools=[ToolFunction(name="echo", description="emit args", call=echo)]))

    payloads = recordings(recorder, artifacts)
    assert payloads, "a tool-using turn is recorded"
    assert all(p["node_id"] == "gather" for p in payloads)
    assert sorted(p["sequence"] for p in payloads) == list(range(len(payloads)))
    # One gateway call, several model calls: this is what gateway-level recording
    # would have collapsed into a single final answer.
    assert len(payloads) > 1
    # The tools on offer, and the instructions, are part of what the agent saw.
    assert payloads[0]["tools"] == ["echo"]
    assert "Use tools." in text_of(payloads[0])


# -- step 2: replay serves recorded answers by position, or fails loudly ----------


def replay_gateway(artifacts, plan, *, live_text="LIVE-MODEL-WAS-CALLED", store=None):
    """A replay wired to a model that would answer differently if it were reached."""
    pytest.importorskip("pydantic_ai")
    from pydantic_ai.models.test import TestModel
    from anchor.runtime.model_replay import ReplayModel

    recorder = ModelRecorder(artifacts, mode=RecordingMode.REPLAY, store=store)
    gateway = PydanticAIModelGateway(profile(), StaticSecrets(),
                                     model=TestModel(custom_output_text=live_text),
                                     recorder=recorder)
    model = gateway._model
    assert isinstance(model, ReplayModel), "replay mode must install a ReplayModel"
    # Swap in the plan under test. The production plan comes from the same store the
    # run recorded into, which is what `store=` above makes possible.
    model.plan = plan
    model.store = store
    return gateway, model


def test_a_recording_replays_to_the_same_answer(tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    ctx = context(node_id="write")
    recorded = run_call(recorder, "Write the paper.", ctx=ctx)

    plan = ReplayPlan.from_recordings(artifacts, recorder.written)
    gateway, model = replay_gateway(artifacts, plan)
    with recording_scope(ctx):
        replayed = asyncio.run(gateway.generate(prompt="Write the paper.",
                                                system_prompt="Answer plainly."))

    assert replayed.text == recorded.text
    assert model.replayed == 1
    assert model.changed == [], "the same prompt is not a change"


def test_replay_never_falls_through_to_the_live_model(tmp_path):
    """The dangerous failure would be a replay that quietly called the real model."""
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    ctx = context()
    recorded = run_call(recorder, "original question", ctx=ctx)

    plan = ReplayPlan.from_recordings(artifacts, recorder.written)
    gateway, _ = replay_gateway(artifacts, plan, live_text="LIVE-MODEL-WAS-CALLED")
    with recording_scope(ctx):
        # A completely different prompt: without replay this would reach TestModel
        # and come back as LIVE-MODEL-WAS-CALLED.
        replayed = asyncio.run(gateway.generate(prompt="something else entirely",
                                                system_prompt="Answer plainly."))
    assert replayed.text == recorded.text
    assert "LIVE-MODEL-WAS-CALLED" not in replayed.text


def test_a_changed_prompt_is_reported_rather_than_rejected(tmp_path):
    """Matching by position is the design: changing the prompt is the point."""
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    ctx = context()
    run_call(recorder, "the original prompt", ctx=ctx)

    plan = ReplayPlan.from_recordings(artifacts, recorder.written)
    gateway, model = replay_gateway(artifacts, plan)
    with recording_scope(ctx):
        asyncio.run(gateway.generate(prompt="a shorter prompt",
                                     system_prompt="Answer plainly."))

    assert model.replayed == 1, "the change does not stop the replay"
    assert len(model.changed) == 1
    change = model.changed[0]
    assert change["node_id"] == ctx.node_id
    assert change["sequence"] == 0
    assert change["recorded_digest"] != change["actual_digest"]
    assert model.summary()["prompt_changed"] == 1


def test_a_missing_recording_fails_with_a_located_divergence(tmp_path):
    """Acceptance: divergence fails loudly and says exactly which call."""
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    ctx = context(node_id="gather", attempt=1)
    with recording_scope(ctx):
        model = gateway(recorder)
        asyncio.run(model.generate(prompt="one", system_prompt="S."))
        asyncio.run(model.generate(prompt="two", system_prompt="S."))

    plan = ReplayPlan.from_recordings(artifacts, recorder.written)
    replay, _ = replay_gateway(artifacts, plan)
    with recording_scope(ctx):
        asyncio.run(replay.generate(prompt="one", system_prompt="S."))
        asyncio.run(replay.generate(prompt="two", system_prompt="S."))
        with pytest.raises(ReplayDivergence) as caught:
            # A third call was never recorded: there is nothing honest to answer with.
            asyncio.run(replay.generate(prompt="three", system_prompt="S."))

    error = caught.value
    assert "gather" in str(error)
    assert error.node_id == "gather"
    assert error.attempt == 1
    assert error.sequence == 2, "the divergence names the call that was missing"
    assert "2 call(s) were recorded" in str(error)


def test_a_replay_outside_a_node_attempt_refuses_to_guess(tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    ctx = context()
    run_call(recorder, "hello", ctx=ctx)

    plan = ReplayPlan.from_recordings(artifacts, recorder.written)
    gateway, _ = replay_gateway(artifacts, plan)
    with pytest.raises(ReplayDivergence, match="outside a node attempt"):
        asyncio.run(gateway.generate(prompt="hello", system_prompt="S."))


def test_a_plan_loads_recordings_from_the_run_events(tmp_path):
    """The production path: a plan is built from what the run recorded."""
    store = make_store(tmp_path, "replay.sqlite")
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    try:
        recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD, store=store)
        ctx = context(node_id="write")
        recorded = run_call(recorder, "Write the paper.", ctx=ctx)

        plan = ReplayPlan(artifacts, store=store)
        gateway, model = replay_gateway(artifacts, plan, store=store)
        with recording_scope(ctx):
            replayed = asyncio.run(gateway.generate(prompt="Write the paper.",
                                                    system_prompt="Answer plainly."))

        assert replayed.text == recorded.text
        assert model.replayed == 1
        assert [e["event_type"] for e in store.list_events(ctx.run_id)] == [
            "model.call", "model.call_replayed"]
    finally:
        store.close()


def test_streaming_replay_refuses_instead_of_going_live(tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    ctx = context()
    run_call(recorder, "hello", ctx=ctx)

    plan = ReplayPlan.from_recordings(artifacts, recorder.written)
    gateway, model = replay_gateway(artifacts, plan)

    async def stream() -> None:
        async with model.request_stream([], None, None):  # type: ignore[arg-type]
            pass

    with recording_scope(ctx), pytest.raises(ReplayDivergence, match="streaming"):
        asyncio.run(stream())


def test_a_worker_replay_reproduces_a_node_exactly(tmp_path):
    """End to end: run a node, then replay it and get the same answer back."""
    pytest.importorskip("pydantic_ai")
    from pydantic_ai.models.test import TestModel

    from anchor.domain.models import NodeLease
    from anchor.runtime.model_replay import ReplayModel
    from anchor.runtime.worker import AgentNodeWorker

    class Store:
        def __init__(self) -> None:
            self.events: list[dict] = []

        def heartbeat_node_lease(self, claim_id, worker_id) -> None: ...
        def list_node_runs(self, run_id):  # noqa: ANN001, ANN201
            return []

        def append_event(self, **kwargs) -> int:
            self.events.append(kwargs)
            return len(self.events)

        def list_events(self, stream_id):  # noqa: ANN001, ANN201
            return [e for e in self.events if e["stream_id"] == stream_id]

    class Sink:
        def __init__(self) -> None:
            self.responses: list[object] = []

        async def persist_model_result(self, **kwargs) -> None:
            self.responses.append(kwargs.get("response"))

    run_id, node_run_id = uuid4(), uuid4()
    lease = NodeLease(claim_id=uuid4(), node_run_id=node_run_id, run_id=run_id,
                      node_id="gather", worker_id="w")
    artifacts = LocalArtifactStore(tmp_path / "artifacts")

    # 1. A real node attempt records what the model said.
    #    One store throughout: the replay plan is built from the same run events the
    #    recording wrote, which is how a real replay finds its answers.
    store = Store()
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD, store=store)
    first = Sink()
    worker = AgentNodeWorker(
        store, registry(),
        {"models.test": PydanticAIModelGateway(profile(), StaticSecrets(),
                                               model=TestModel(), recorder=recorder)},
        first)
    asyncio.run(worker.execute_claimed_once(worker_id="w", agent_ref="agents.reader",
                                            prompt="record one round",
                                            expected_node_id="gather", lease=lease,
                                            heartbeat_interval=0.01))
    assert recorder.recorded >= 1

    # 2. The same node replays against a model that would answer differently.
    replay_recorder = ModelRecorder(artifacts, mode=RecordingMode.REPLAY, store=store)
    gateway = PydanticAIModelGateway(profile(), StaticSecrets(),
                                     model=TestModel(custom_output_text="LIVE"),
                                     recorder=replay_recorder)
    assert isinstance(gateway._model, ReplayModel), \
        "replay mode installs a replay model rather than a recording one"
    second = Sink()
    worker2 = AgentNodeWorker(store, registry(), {"models.test": gateway}, second)
    asyncio.run(worker2.execute_claimed_once(worker_id="w", agent_ref="agents.reader",
                                             prompt="record one round",
                                             expected_node_id="gather", lease=lease,
                                             heartbeat_interval=0.01))

    assert first.responses and second.responses
    assert second.responses[0].text == first.responses[0].text
    assert "LIVE" not in second.responses[0].text
    types = [e["event_type"] for e in store.events]
    assert types.count("model.call") == recorder.recorded, \
        "a replay does not write new recordings over the ones it is reading"
    assert "model.call_replayed" in types


def test_per_process_bookkeeping_stays_bounded(tmp_path):
    """A worker is long-lived: counters that never release would leak.

    One campaign runs about a thousand node attempts, and a service is expected to
    stay up for weeks, so unbounded per-attempt bookkeeping is a real defect rather
    than a style question.
    """
    from anchor.runtime.model_recording import MAX_TRACKED_ATTEMPTS, MAX_TRACKED_REFS

    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    for _ in range(MAX_TRACKED_ATTEMPTS * 3):
        recorder._next_sequence(context())
    assert len(recorder._sequences) <= MAX_TRACKED_ATTEMPTS

    for index in range(MAX_TRACKED_REFS * 3):
        recorder._remember(f"ref-{index}")
    assert len(recorder.written) <= MAX_TRACKED_REFS
    # The most recent entries survive: the log is for inspecting what just happened.
    assert recorder.written[-1] == f"ref-{MAX_TRACKED_REFS * 3 - 1}"


def test_a_replay_plan_keeps_a_bounded_number_of_runs(tmp_path):
    """The same reasoning as the recorder's counters, on the replay side."""
    from anchor.runtime.model_replay import MAX_PLANNED_RUNS

    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    ctx = context()
    run_call(recorder, "hello", ctx=ctx)
    payload = read_recording(artifacts, recorder.written[0])

    plan = ReplayPlan(artifacts)
    for index in range(MAX_PLANNED_RUNS * 2):
        plan._index({**payload, "run_id": str(uuid4())})
    assert len(plan._by_run) <= MAX_PLANNED_RUNS
    assert len(plan._loaded) <= MAX_PLANNED_RUNS


def test_a_different_run_does_not_match_an_old_recording(tmp_path):
    """Pins a real limitation, so it is a known boundary and not a surprise.

    The replay key contains `node_run_id`, which a new run generates afresh.
    Replaying therefore reproduces an *existing* run's attempts — a resume, a retry,
    a forensic re-execution — and cannot serve a freshly admitted run of the same
    graph. That is deliberate: matching a new run to an old one by node name and
    order would risk silently pairing a call with the wrong recorded answer, which
    is the failure mode the loud divergence exists to prevent.

    A different run of the same graph is reproduced by the graph itself being
    deterministic, not by replay; see `docs/RECORDING_AND_REPLAY.md`.
    """
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD)
    original = context(node_id="write")
    run_call(recorder, "Write the paper.", ctx=original)

    plan = ReplayPlan.from_recordings(artifacts, recorder.written)
    replay, _ = replay_gateway(artifacts, plan)

    # Same node, same attempt number, same prompt — but a new run's identifiers.
    fresh = context(node_id="write", attempt=0)
    with recording_scope(fresh), pytest.raises(ReplayDivergence, match="were recorded"):
        asyncio.run(replay.generate(prompt="Write the paper.",
                                    system_prompt="Answer plainly."))
