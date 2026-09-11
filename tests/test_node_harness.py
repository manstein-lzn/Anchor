"""The node harness: one attempt, its real input, minutes instead of half an hour.

The properties that make it worth having, and each is a way it could quietly lie:

* it builds the prompt with the same function the worker uses, so an experiment
  measures the prompt under study rather than a lookalike;
* it reads the attempt's own persisted snapshot, not a re-derivation from current
  state, so a run that moved on cannot silently change the question;
* it refuses a tool-using node instead of executing tools outside any ledger;
* it never writes to the run it read.
"""

from __future__ import annotations

import asyncio
from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphVersion, Trigger
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import (
    AgentCapability,
    CapabilityRegistry,
    ModelProfile,
    ToolCapability,
)
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.model_gateway import PydanticAIModelGateway
from anchor.runtime.node_harness import HarnessUnsupported, NodeHarness, compare
from anchor.runtime.node_prompt import EMPTY_BLOCK, PromptParts, assemble_prompt
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.secrets import EnvironmentSecretProvider
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.runtime.worker import AgentNodeWorker

from conftest import make_store


class StaticSecrets(EnvironmentSecretProvider):
    def get(self, name: str) -> str:
        return "test-secret"


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
                              side_effect=False, operation_kind="read")])


def executed_run(tmp_path, *, tool_refs=()):
    """Run a two-node graph for real so there are frozen attempts to work from."""
    pytest.importorskip("pydantic_ai")
    from pydantic_ai.models.test import TestModel

    store = make_store(tmp_path, "harness.sqlite")
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    definition = GraphDefinition(
        graph_id="harness", name="Harness", entry_node_id="gather",
        nodes=[GraphNode(id="gather", type="agent", name="Gather",
                         agent_ref="agents.reader"),
               GraphNode(id="write", type="agent", name="Write",
                         agent_ref="agents.reader")],
        edges=[GraphEdge(source="gather", target="write")])
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(
        Trigger(graph_version_id=version.graph_version_id, type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id,
                                         idempotency_key=f"harness-{tmp_path.name}",
                                         objective="Write it well",
                                         inputs={"topic": "cost models"}))
    asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))
    gateway = PydanticAIModelGateway(profile(), StaticSecrets(), model=TestModel())
    worker = AgentNodeWorker(store, registry(tool_refs), {"models.test": gateway},
                             ArtifactCheckpointSink(store, artifacts, "worker"))
    for _ in range(4):
        lease = store.claim_ready_agent_node("worker", uuid4())
        if lease is None:
            break
        # The worker loop resolves this from the incoming edges; passing it here is
        # what persists the attempt's frozen input, which is the harness's subject.
        snapshot = {"topic": "cost models", "from": lease.node_id}
        asyncio.run(worker.execute_claimed_once(
            worker_id="worker", agent_ref="agents.reader", prompt="do it",
            expected_node_id=lease.node_id, lease=lease, input_snapshot=snapshot,
            heartbeat_interval=0.01))
    return store, artifacts, receipt, gateway


def harness_for(store, artifacts, gateway, *, tool_refs=()) -> NodeHarness:
    return NodeHarness(store, artifacts, gateway=gateway,
                       registry=registry(tool_refs))


def test_the_harness_reads_the_attempts_own_frozen_input(tmp_path):
    store, artifacts, receipt, gateway = executed_run(tmp_path)
    try:
        harness = harness_for(store, artifacts, gateway)
        attempt = harness.frozen_attempt(receipt.run_id, "write")
        assert attempt.agent_ref == "agents.reader"
        assert attempt.attempt == 0
        assert attempt.objective == "Write it well"
        assert attempt.node_name == "Write"
        assert attempt.snapshot, "the declared input was persisted and is readable"
        assert attempt.tools == ()
    finally:
        store.close()


def test_the_harness_builds_the_same_prompt_the_worker_builds(tmp_path):
    """The property the whole harness rests on: it measures the real prompt."""
    store, artifacts, receipt, gateway = executed_run(tmp_path)
    try:
        harness = harness_for(store, artifacts, gateway)
        attempt = harness.frozen_attempt(receipt.run_id, "write")
        expected = assemble_prompt(PromptParts(
            objective=attempt.objective, node_name=attempt.node_name,
            snapshot=attempt.snapshot))
        assert harness.prompt_for(attempt) == expected
        # And the pieces a policy would move are all present.
        prompt = harness.prompt_for(attempt)
        assert "Task objective:" in prompt
        assert "Execute graph node: Write" in prompt
        assert "Durable input snapshot:" in prompt
        assert f"Run memory:\n{EMPTY_BLOCK}" in prompt
    finally:
        store.close()


def test_a_run_that_moved_on_does_not_change_the_question(tmp_path):
    """The snapshot is the attempt's, not a re-derivation from today's state."""
    store, artifacts, receipt, gateway = executed_run(tmp_path)
    try:
        harness = harness_for(store, artifacts, gateway)
        before = harness.frozen_attempt(receipt.run_id, "write")
        before_prompt = harness.prompt_for(before)

        # Execute the node again, which advances the run's state.
        worker = AgentNodeWorker(store, registry(), {"models.test": gateway},
                                 ArtifactCheckpointSink(store, artifacts, "worker2"))
        lease = store.claim_ready_agent_node("worker2", uuid4())
        if lease is not None:
            asyncio.run(worker.execute_claimed_once(
                worker_id="worker2", agent_ref="agents.reader", prompt="again",
                expected_node_id=lease.node_id, lease=lease,
                input_snapshot={"topic": "cost models"}, heartbeat_interval=0.01))

        after = harness.frozen_attempt(receipt.run_id, "write", attempt=0)
        assert harness.prompt_for(after) == before_prompt, \
            "attempt 0's prompt must not move when later attempts happen"
    finally:
        store.close()


def test_a_policy_can_change_the_snapshot_without_touching_the_rest(tmp_path):
    """The policy surface: vary one part, leave the rest of the request alone."""
    store, artifacts, receipt, gateway = executed_run(tmp_path)
    try:
        harness = harness_for(store, artifacts, gateway)
        attempt = harness.frozen_attempt(receipt.run_id, "write")

        trimmed = harness.prompt_for(attempt, snapshot={"reduced": True})
        assert '"reduced":true' in trimmed
        assert harness.prompt_for(attempt) != trimmed
        # The frame around the snapshot is unchanged, which is what makes a
        # comparison attributable to the snapshot alone.
        assert trimmed.startswith("Task objective:")
        assert trimmed.endswith(f"Promoted organizational knowledge:\n{EMPTY_BLOCK}")

        without = harness.prompt_for(attempt, include_memory=False)
        assert f"Run memory:\n{EMPTY_BLOCK}" in without
    finally:
        store.close()


def test_the_harness_runs_the_attempt_and_reports_what_it_cost(tmp_path):
    pytest.importorskip("pydantic_ai")

    store, artifacts, receipt, gateway = executed_run(tmp_path)
    try:
        harness = harness_for(store, artifacts, gateway)
        attempt = harness.frozen_attempt(receipt.run_id, "write")
        result = asyncio.run(harness.run(attempt))
        assert result.node_id == "write"
        assert result.text, "the node produced output"
        assert result.prompt == harness.prompt_for(attempt)
        summary = result.summary()
        assert summary["prompt_chars"] == len(result.prompt)
        assert summary["output_chars"] == len(result.text)
        assert summary["seconds"] >= 0
    finally:
        store.close()


def test_a_tool_using_node_is_refused_rather_than_run_unaudited(tmp_path):
    pytest.importorskip("pydantic_ai")

    store, artifacts, receipt, gateway = executed_run(tmp_path, tool_refs=("echo",))
    try:
        harness = harness_for(store, artifacts, gateway, tool_refs=("echo",))
        attempt = harness.frozen_attempt(receipt.run_id, "gather")
        assert attempt.tools == ("echo",)
        with pytest.raises(HarnessUnsupported, match="ledger"):
            asyncio.run(harness.run(attempt))
        # Asking for the prompt is still allowed: inspecting costs nothing.
        assert "Durable input snapshot:" in harness.prompt_for(attempt)
    finally:
        store.close()


def test_the_harness_does_not_write_to_the_run_it_read(tmp_path):
    store, artifacts, receipt, gateway = executed_run(tmp_path)
    try:
        before = [(item.id, item.status) for item in store.list_node_runs(receipt.run_id)]
        harness = harness_for(store, artifacts, gateway)
        attempt = harness.frozen_attempt(receipt.run_id, "write")
        asyncio.run(harness.run(attempt))
        after = [(item.id, item.status) for item in store.list_node_runs(receipt.run_id)]
        assert after == before, "an experiment must not alter the evidence it studies"
    finally:
        store.close()


def test_an_unknown_node_or_attempt_fails_clearly(tmp_path):
    store, artifacts, receipt, gateway = executed_run(tmp_path)
    try:
        harness = harness_for(store, artifacts, gateway)
        with pytest.raises(KeyError, match="no node"):
            harness.frozen_attempt(receipt.run_id, "nonexistent")
        with pytest.raises(KeyError, match="no attempt"):
            harness.frozen_attempt(receipt.run_id, "write", attempt=99)
    finally:
        store.close()


def test_compare_reports_the_delta_and_whether_the_answer_moved(tmp_path):
    pytest.importorskip("pydantic_ai")

    store, artifacts, receipt, gateway = executed_run(tmp_path)
    try:
        harness = harness_for(store, artifacts, gateway)
        attempt = harness.frozen_attempt(receipt.run_id, "write")
        before = asyncio.run(harness.run(attempt))
        after = asyncio.run(harness.run(attempt, snapshot={"reduced": True}))
        report = compare(before, after)
        assert report["prompt_delta"] == len(after.prompt) - len(before.prompt)
        assert report["prompt_delta"] < 0, "the reduced snapshot is a shorter prompt"
        assert report["identical_output"] in (True, False), \
            "the report says whether the answer moved; it does not judge which is better"
        assert set(report) >= {"node_id", "prompt_chars", "output_chars", "cost", "seconds"}
    finally:
        store.close()


def test_prompt_assembly_is_shared_with_the_worker(tmp_path):
    """A second definition of the prompt would make every experiment meaningless."""
    import inspect

    from anchor.runtime import worker_service

    source = inspect.getsource(worker_service._resolver)
    assert "assemble_prompt" in source
    assert "Durable input snapshot" not in source, \
        "the worker must not keep its own copy of the prompt text"


def test_the_harness_says_how_much_to_trust_the_request(tmp_path):
    """A reconstruction and a recording are different claims.

    A run made before model call recording existed can only be rebuilt from its
    frozen snapshot plus today's configuration. That is still useful, and it is not
    the same as the request the model received — so the harness labels which it is
    instead of leaving the reader to assume.
    """
    store, artifacts, receipt, gateway = executed_run(tmp_path)
    try:
        harness = harness_for(store, artifacts, gateway)
        attempt = harness.frozen_attempt(receipt.run_id, "write")

        assert attempt.provenance == "reconstructed-from-current-configuration"
        report = harness.fidelity(attempt)
        assert report["recorded"] is False, "the fixture makes no recordings"
        assert "no recording" in report["note"]
        assert report["memory_available_to_harness"] is False
        assert "matches" not in report, "there is nothing to compare against"
    finally:
        store.close()


def test_a_recorded_request_is_reported_as_such_and_compared(tmp_path):
    """With a recording, the harness checks its own reconstruction.

    The comparison is against the user prompt the model actually received, not
    against the whole rendered transcript: the assembled prompt is the user text of
    the request, and comparing anything else would never match.
    """
    pytest.importorskip("pydantic_ai")
    from pydantic_ai.models.test import TestModel

    from anchor.runtime.model_recording import CallContext, ModelRecorder, RecordingMode, bind_call, unbind_call
    from anchor.runtime.model_gateway import PydanticAIModelGateway

    store, artifacts, receipt, gateway = executed_run(tmp_path)
    try:
        row = next(item for item in store.list_node_runs(receipt.run_id)
                   if item.node_id == "write")
        context_ = CallContext(run_id=receipt.run_id, node_id="write",
                               node_run_id=row.id, attempt=row.attempt)
        recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD, store=store)
        model = PydanticAIModelGateway(profile(), StaticSecrets(), model=TestModel(),
                                       recorder=recorder)

        harness = harness_for(store, artifacts, gateway)
        prompt = harness.prompt_for(harness.frozen_attempt(receipt.run_id, "write"))

        # Record the call the harness would make, at that attempt's position.
        token = bind_call(context_)
        try:
            asyncio.run(model.generate(prompt=prompt, system_prompt="Answer plainly."))
        finally:
            unbind_call(token)
        assert recorder.recorded == 1

        attempt = harness.frozen_attempt(receipt.run_id, "write")
        assert attempt.provenance == "recording"
        assert attempt.instructions == "Answer plainly.", \
            "instructions come from the recording, not from today's configuration"
        report = harness.fidelity(attempt)
        assert report["recorded"] is True
        assert report["matches"] is True, report
        assert report["recorded_chars"] == report["reassembled_chars"]
    finally:
        store.close()


def test_a_diverging_reconstruction_is_located_not_hand_waved(tmp_path):
    """If the rebuild stops matching, say where — do not report it as faithful."""
    pytest.importorskip("pydantic_ai")
    from pydantic_ai.models.test import TestModel

    from anchor.runtime.model_recording import CallContext, ModelRecorder, RecordingMode, bind_call, unbind_call
    from anchor.runtime.model_gateway import PydanticAIModelGateway

    store, artifacts, receipt, gateway = executed_run(tmp_path)
    try:
        row = next(item for item in store.list_node_runs(receipt.run_id)
                   if item.node_id == "write")
        context_ = CallContext(run_id=receipt.run_id, node_id="write",
                               node_run_id=row.id, attempt=row.attempt)
        recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD, store=store)
        model = PydanticAIModelGateway(profile(), StaticSecrets(), model=TestModel(),
                                       recorder=recorder)
        token = bind_call(context_)
        try:
            asyncio.run(model.generate(prompt="something else entirely",
                                       system_prompt="Answer plainly."))
        finally:
            unbind_call(token)

        harness = harness_for(store, artifacts, gateway)
        report = harness.fidelity(harness.frozen_attempt(receipt.run_id, "write"))
        assert report["matches"] is False
        assert "first_difference_at" in report
        assert report["first_difference_at"]["index"] >= 0
    finally:
        store.close()
