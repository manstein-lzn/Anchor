import asyncio
from uuid import uuid4
from anchor.runtime.capabilities import AgentCapability, CapabilityRegistry, ModelProfile
from anchor.runtime.model_gateway import ModelResponse
from anchor.runtime.worker import AgentNodeWorker
from anchor.runtime.worker_loop import run_worker_loop

class Gateway:
    async def generate(self, *, prompt, system_prompt=""): return ModelResponse(text="done", provider="test", model="m")
class Store:
    def __init__(self): self.claims=0; self.heartbeats=0; self.runtime_heartbeats=0
    def record_runtime_heartbeat(self, component, instance_id): self.runtime_heartbeats += 1
    def claim_ready_node(self, worker_id, claim_id):
        if self.claims: return None
        self.claims += 1
        from anchor.domain.models import NodeLease
        return NodeLease(claim_id=claim_id,node_run_id=uuid4(),run_id=uuid4(),node_id="start",worker_id=worker_id)
    def heartbeat_node_lease(self, claim_id, worker_id): self.heartbeats += 1
class Sink:
    async def persist_model_result(self, **kwargs): pass
def test_worker_loop_claims_resolves_and_stops():
    store=Store(); stop=asyncio.Event()
    registry=CapabilityRegistry(models=[ModelProfile(ref="m",provider="test",model="x",secret_ref="s")],agents=[AgentCapability(ref="agents.start",model_ref="m")])
    async def resolve(run_id,node_id): stop.set(); return "agents.start","hello","start",{}
    asyncio.run(run_worker_loop(AgentNodeWorker(store,registry,{"m":Gateway()},Sink()),worker_id="worker",resolve_prompt=resolve,stop=stop))
    assert store.claims==1 and store.heartbeats==1 and store.runtime_heartbeats==1
