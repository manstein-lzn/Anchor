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
