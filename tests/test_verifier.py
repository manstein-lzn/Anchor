import asyncio
from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphVersion, Trigger
from anchor.domain.models import RunStatus, VerificationVerdict
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import CapabilityRegistry, ModelProfile, VerifierCapability
from anchor.runtime.model_gateway import ModelResponse
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.sinks import VerificationCheckpointSink
from anchor.runtime.verifier import VerifierNodeWorker
from anchor.state.errors import ConcurrencyConflict
from conftest import make_store


class FakeGateway:
    def __init__(self, text: str | None = None, error: Exception | None = None):
        self.text = text
        self.error = error
        self.calls = []

    async def generate(self, *, prompt: str, system_prompt: str = "") -> ModelResponse:
        self.calls.append((prompt, system_prompt))
        if self.error is not None:
            raise self.error
        return ModelResponse(
            text=self.text,
            provider="test-provider",
            model="test-model",
            response_id="response-1",
        )


def seed(tmp_path, *, input_value=True):
    store = make_store(tmp_path, "state.sqlite")
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    definition = GraphDefinition(
        graph_id="verified-run",
        name="Verified run",
        nodes=[
            GraphNode(id="produce", type="agent", name="Produce", agent_ref="agents.produce"),
            GraphNode(id="verify", type="verifier", name="Verify", verifier_ref="verifiers.evidence"),
        ],
        edges=[GraphEdge(source="produce", target="verify")],
    )
    version = store.publish_graph(GraphVersion.publish(definition, 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id, type="manual"))
    receipt = store.admit_run(RunRequest(
        trigger_id=trigger.id,
        idempotency_key="verify-1",
        objective="Deliver approved evidence",
        success_criteria=["The artifact is explicitly approved"],
        inputs={"expected": input_value},
    ))
    asyncio.run(DurableExecutionReceiver(store).accept(store.pending_dispatches()[0]))
    producer = store.claim_ready_agent_node("agent-worker", uuid4())
    assert producer is not None
    source_ref = artifacts.put_text(
        '{"approved": ' + ("true" if input_value else "false") + ', "claim": "supported"}',
        media_type="application/json",
    )
    store.complete_node_and_propagate(
        producer.claim_id,
        "agent-worker",
        output_ref=source_ref,
        input_snapshot={"inputs": {"expected": input_value}},
    )
    return store, artifacts, receipt, source_ref


def worker(store, artifacts, capability, gateway=None):
    models = []
    gateways = {}
    if capability.model_ref:
        models = [ModelProfile(
            ref=capability.model_ref,
            provider="test-provider",
            model="test-model",
            secret_ref="unused",
        )]
        gateways[capability.model_ref] = gateway
    registry = CapabilityRegistry(models=models, verifiers=[capability])
    worker_id = "verifier-worker"
    return VerifierNodeWorker(
        store,
        registry,
        gateways,
        artifacts,
        VerificationCheckpointSink(store, worker_id),
    ), worker_id


def test_deterministic_verifier_passes_and_opens_completion_gate(tmp_path):
    store, artifacts, receipt, source_ref = seed(tmp_path)
    capability = VerifierCapability(
        ref="verifiers.evidence",
        version="rules-2026-09",
        adapter="deterministic",
        expression="length(artifacts) == `1` && artifacts[0].content.approved == `true`",
    )
    verifier, worker_id = worker(store, artifacts, capability)
    outcome = asyncio.run(verifier.execute_once(worker_id=worker_id))

    assert outcome is not None and outcome.verdict is VerificationVerdict.PASSED
    assert store.get_run(receipt.run_id).status is RunStatus.COMPLETED
    node = next(item for item in store.list_node_runs(receipt.run_id) if item.node_id == "verify")
    assert node.status.value == "completed" and node.output_ref == outcome.evidence_ref
    records = store.list_verifications(receipt.run_id)
    assert len(records) == 1
    record = records[0]
    assert record.verifier_version == "rules-2026-09"
    assert record.verified_artifact_hashes == [source_ref.rsplit("/", 1)[1]]
    assert record.verified_context_hash == node.input_hash
    assert store.get_context_snapshot(node.id).input_hash == record.verified_context_hash
    evidence = artifacts.get_text(record.evidence_ref)
    assert '"verdict":"passed"' in evidence
    event_types = [item["event_type"] for item in store.list_events(receipt.run_id)]
    verification_event = event_types.index("verification.decided")
    assert event_types.index("node.completed", verification_event) > verification_event
    store.close()


def test_deterministic_rejection_persists_evidence_and_fails_run(tmp_path):
    store, artifacts, receipt, _ = seed(tmp_path, input_value=False)
    capability = VerifierCapability(
        ref="verifiers.evidence",
        adapter="deterministic",
        expression="artifacts[0].content.approved == `true`",
    )
    verifier, worker_id = worker(store, artifacts, capability)
    outcome = asyncio.run(verifier.execute_once(worker_id=worker_id))

    assert outcome is not None and outcome.verdict is VerificationVerdict.REJECTED
    assert store.get_run(receipt.run_id).status is RunStatus.FAILED
    node = next(item for item in store.list_node_runs(receipt.run_id) if item.node_id == "verify")
    assert (node.status.value, node.error_code) == ("failed", "verification_rejected")
    assert store.list_verifications(receipt.run_id)[0].verdict is VerificationVerdict.REJECTED
    assert store.get_context_snapshot(node.id) is not None
    store.close()


@pytest.mark.parametrize(
    ("response", "expected"),
    [
        ('{"verdict":"passed","reason":"artifact and criteria agree"}', VerificationVerdict.PASSED),
        ('{"verdict":"rejected","reason":"claim lacks a primary source"}', VerificationVerdict.REJECTED),
        ("looks good", VerificationVerdict.ERROR),
    ],
)
def test_model_verifier_requires_structured_verdict(tmp_path, response, expected):
    store, artifacts, receipt, _ = seed(tmp_path)
    gateway = FakeGateway(response)
    capability = VerifierCapability(
        ref="verifiers.evidence",
        version="prompt-v3",
        adapter="model",
        model_ref="models.verifier",
        instructions="Require direct evidence.",
    )
    verifier, worker_id = worker(store, artifacts, capability, gateway)
    outcome = asyncio.run(verifier.execute_once(worker_id=worker_id))

    assert outcome is not None and outcome.verdict is expected
    record = store.list_verifications(receipt.run_id)[0]
    assert record.model_ref == "models.verifier"
    assert (record.model_provider, record.model_name, record.model_response_id) == (
        "test-provider", "test-model", "response-1",
    )
    assert gateway.calls and "strict JSON" in gateway.calls[0][1]
    expected_status = RunStatus.COMPLETED if expected is VerificationVerdict.PASSED else RunStatus.FAILED
    assert store.get_run(receipt.run_id).status is expected_status
    store.close()


def test_model_transport_failure_keeps_lease_for_supervision(tmp_path):
    store, artifacts, receipt, _ = seed(tmp_path)
    capability = VerifierCapability(
        ref="verifiers.evidence", adapter="model", model_ref="models.verifier",
    )
    verifier, worker_id = worker(
        store, artifacts, capability, FakeGateway(error=RuntimeError("transport disconnected")),
    )
    with pytest.raises(RuntimeError, match="transport disconnected"):
        asyncio.run(verifier.execute_once(worker_id=worker_id))
    node = next(item for item in store.list_node_runs(receipt.run_id) if item.node_id == "verify")
    assert node.status.value == "running"
    assert store.list_verifications(receipt.run_id) == []
    assert len(store.list_active_leases()) == 1
    store.close()


def test_verifier_cannot_be_claimed_or_completed_through_other_executors(tmp_path):
    store, artifacts, receipt, _ = seed(tmp_path)
    assert store.claim_ready_agent_node("agent", uuid4()) is None
    assert store.claim_ready_control_node("control", uuid4()) is None
    lease = store.claim_ready_verifier_node("verifier", uuid4())
    assert lease is not None
    with pytest.raises(ConcurrencyConflict, match="verification verdict"):
        store.complete_node_and_propagate(
            lease.claim_id,
            "verifier",
            output_ref=artifacts.put_text("not evidence"),
            input_snapshot={"inputs": {"expected": True}},
        )
    assert store.list_verifications(receipt.run_id) == []
    assert store.get_context_snapshot(lease.node_run_id) is None
    recovered = store.recover_node_lease(lease.claim_id, reason="operator confirmed process exit")
    assert recovered.status.value == "ready"
    store.close()
