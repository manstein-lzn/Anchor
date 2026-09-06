import asyncio
from datetime import datetime, timezone
from uuid import uuid4
from anchor.domain.graph import Trigger, TriggerType
from anchor.runtime.scheduler import run_interval_scheduler

class Store:
    def __init__(self, trigger): self.trigger=trigger; self.requests=[]
    def list_active_triggers(self): return [self.trigger]
    def admit_run(self, request): self.requests.append(request); return None

def test_scheduler_creates_one_occurrence_per_slot():
    trigger=Trigger(graph_version_id=uuid4(), type=TriggerType.INTERVAL, interval_seconds=60)
    store=Store(trigger); stop=asyncio.Event()
    async def run():
        calls=0
        def objective(t, now): return "scheduled", {"at": now.isoformat()}
        async def stopper():
            nonlocal calls
            while calls < 1:
                await asyncio.sleep(0.001); calls += 1; stop.set()
        await asyncio.gather(run_interval_scheduler(store, objective_factory=objective, stop=stop, interval=1), stopper())
    asyncio.run(run())
    assert len(store.requests) == 1
