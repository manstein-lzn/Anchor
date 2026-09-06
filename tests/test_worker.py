import asyncio
from uuid import uuid4

from anchor.runtime.capabilities import AgentCapability, CapabilityRegistry, ModelProfile
from anchor.runtime.model_gateway import ModelResponse
from anchor.runtime.worker import AgentNodeWorker


class Gateway:
    async def generate(self, *, prompt, system_prompt=""):
        return ModelResponse(text=f"{prompt}|{system_prompt}", provider="test", model="test-model")


class Sink:
    def __init__(self): self.calls = []
    async def persist_model_result(self, **kwargs): self.calls.append(kwargs)


class Store:
    def __init__(self): self.claimed = False; self.heartbeats = 0
    def heartbeat_node_lease(self, claim_id, worker_id): self.heartbeats += 1
    def claim_ready_node(self, worker_id, claim_id):
        if self.claimed: return None
        self.claimed = True
        from anchor.domain.models import NodeLease
        return NodeLease(claim_id=claim_id, node_run_id=uuid4(), run_id=uuid4(), node_id="start", worker_id=worker_id)


def test_worker_resolves_capability_claims_and_persists_through_sink():
    registry = CapabilityRegistry(
        models=[ModelProfile(ref="models.test", provider="test", model="test-model", secret_ref="unused")],
        agents=[AgentCapability(ref="agents.start", model_ref="models.test", instructions="be concise")],
    )
    sink = Sink()
    outcome = asyncio.run(AgentNodeWorker(Store(), registry, {"models.test": Gateway()}, sink).execute_once(
        worker_id="worker", agent_ref="agents.start", prompt="hello", expected_node_id="start"))
    assert outcome is not None and outcome.response.text == "hello|be concise"
    assert sink.calls and sink.calls[0]["response"].text.startswith("hello")


def test_worker_does_not_claim_when_queue_is_empty():
    class Empty(Store):
        def claim_ready_node(self, worker_id, claim_id): return None
    registry = CapabilityRegistry(models=[ModelProfile(ref="m", provider="test", model="x", secret_ref="s")],
                                  agents=[AgentCapability(ref="a", model_ref="m")])
    assert asyncio.run(AgentNodeWorker(Empty(), registry, {"m": Gateway()}, Sink()).execute_once(
        worker_id="worker", agent_ref="a", prompt="hello")) is None


def test_worker_heartbeats_during_long_model_call():
    class SlowGateway:
        async def generate(self, *, prompt, system_prompt=""):
            await asyncio.sleep(0.03)
            return ModelResponse(text="done", provider="test", model="m")
    store = Store(); sink = Sink()
    registry = CapabilityRegistry(models=[ModelProfile(ref="m", provider="test", model="x", secret_ref="s")],
                                  agents=[AgentCapability(ref="agents.start", model_ref="m")])
    asyncio.run(AgentNodeWorker(store, registry, {"m": SlowGateway()}, sink).execute_once(
        worker_id="worker", agent_ref="agents.start", prompt="hello", expected_node_id="start",
        heartbeat_interval=0.005))
    assert store.heartbeats >= 1


def test_worker_forwards_snapshot_hash_to_sink():
    from anchor.domain.models import NodeLease
    from anchor.runtime.context import input_hash
    store = Store(); sink = Sink()
    registry = CapabilityRegistry(models=[ModelProfile(ref="m", provider="test", model="x", secret_ref="s")],
                                  agents=[AgentCapability(ref="agents.start", model_ref="m")])
    lease = NodeLease(claim_id=uuid4(), node_run_id=uuid4(), run_id=uuid4(),
                      node_id="start", worker_id="worker")
    snapshot = {"inputs": {"query": "hi"}}
    asyncio.run(AgentNodeWorker(store, registry, {"m": Gateway()}, sink).execute_claimed_once(
        worker_id="worker", agent_ref="agents.start", prompt="hello", lease=lease,
        expected_node_id="start", input_snapshot=snapshot, heartbeat_interval=1000))
    assert sink.calls and sink.calls[0]["input_hash"] == input_hash(snapshot)
    assert sink.calls[0]["input_snapshot"] == snapshot


def test_worker_without_snapshot_persists_null_input_hash():
    from anchor.domain.models import NodeLease
    store = Store(); sink = Sink()
    registry = CapabilityRegistry(models=[ModelProfile(ref="m", provider="test", model="x", secret_ref="s")],
                                  agents=[AgentCapability(ref="agents.start", model_ref="m")])
    lease = NodeLease(claim_id=uuid4(), node_run_id=uuid4(), run_id=uuid4(),
                      node_id="start", worker_id="worker")
    asyncio.run(AgentNodeWorker(store, registry, {"m": Gateway()}, sink).execute_claimed_once(
        worker_id="worker", agent_ref="agents.start", prompt="hello", lease=lease,
        expected_node_id="start", heartbeat_interval=1000))
    assert sink.calls and sink.calls[0]["input_hash"] is None
