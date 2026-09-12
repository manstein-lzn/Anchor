"""Process-level proof that interrupted Agent work is recoverable explicitly."""

from __future__ import annotations

import asyncio
import os
import subprocess
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

pytest.importorskip("sqlalchemy")
pytest.importorskip("alembic")
from alembic import command
from alembic.config import Config

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphNode, GraphVersion, Trigger
from anchor.domain.models import RunStatus
from anchor.runtime.dispatch import dispatch_pending
from anchor.runtime.receiver import DurableExecutionReceiver
from anchor.runtime.supervisor import assess_leases
from anchor.state.relational import RelationalStateStore


ROOT = Path(__file__).resolve().parents[1]
CHILD = r'''
import asyncio
import os
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import AgentCapability, CapabilityRegistry, ModelProfile
from anchor.runtime.model_gateway import ModelResponse
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.runtime.worker import AgentNodeWorker
from anchor.runtime.worker_loop import ResolvedPrompt, run_worker_loop
from anchor.state.relational import RelationalStateStore

class Gateway:
    async def generate(self, *, prompt, system_prompt=""):
        if os.environ["ANCHOR_RECOVERY_MODE"] == "slow":
            await asyncio.sleep(60)
        return ModelResponse(text="recovered", provider="test", model="recovery")

async def main():
    store = RelationalStateStore(os.environ["ANCHOR_RECOVERY_DATABASE_URL"])
    registry = CapabilityRegistry(
        models=[ModelProfile(ref="models.recovery", provider="test", model="recovery", secret_ref="unused")],
        agents=[AgentCapability(ref="agents.recovery", model_ref="models.recovery")],
    )
    artifacts = LocalArtifactStore(os.environ["ANCHOR_RECOVERY_ARTIFACT_ROOT"])
    worker_id = os.environ["ANCHOR_RECOVERY_WORKER_ID"]
    worker = AgentNodeWorker(store, registry, {"models.recovery": Gateway()},
                             ArtifactCheckpointSink(store, artifacts, worker_id))
    async def resolve(run_id, node_id):
        return ResolvedPrompt(
            agent_ref="agents.recovery", prompt="recover this run", expected_node_id=node_id,
            input_snapshot={"inputs": {"mode": os.environ["ANCHOR_RECOVERY_MODE"]}})
    try:
        await run_worker_loop(worker, worker_id=worker_id, resolve_prompt=resolve, interval=0.02)
    finally:
        store.close()

asyncio.run(main())
'''


def migration_config(url: str) -> Config:
    config = Config(str(ROOT / "alembic.ini"))
    config.attributes["database_url"] = url
    return config


def test_interrupted_worker_lease_requires_explicit_recovery(tmp_path):
    url = f"sqlite:///{tmp_path / 'recovery.sqlite'}"
    command.upgrade(migration_config(url), "head")
    store = RelationalStateStore(url)
    child_env = dict(os.environ, PYTHONPATH=str(ROOT / "src"),
                     ANCHOR_RECOVERY_DATABASE_URL=url,
                     ANCHOR_RECOVERY_ARTIFACT_ROOT=str(tmp_path / "artifacts"))
    slow = fast = None
    try:
        graph = GraphVersion.publish(GraphDefinition(
            graph_id="recovery", name="Recovery",
            nodes=[GraphNode(id="work", type="agent", name="Work", agent_ref="agents.recovery")],
        ), 1)
        store.publish_graph(graph)
        trigger = store.create_trigger(Trigger(graph_version_id=graph.graph_version_id, type="manual"))
        receipt = store.admit_run(RunRequest(trigger_id=trigger.id, idempotency_key="recovery-1",
                                             objective="recover", inputs={"source": "test"}))
        asyncio.run(dispatch_pending(store, DurableExecutionReceiver(store)))

        slow_env = dict(child_env, ANCHOR_RECOVERY_MODE="slow", ANCHOR_RECOVERY_WORKER_ID="slow-worker")
        slow = subprocess.Popen([sys.executable, "-c", CHILD], cwd=ROOT, env=slow_env,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + 10
        active = []
        while time.monotonic() < deadline:
            active = store.list_active_leases()
            if active:
                break
            time.sleep(0.05)
        assert len(active) == 1 and active[0].node_id == "work"
        slow.terminate()
        assert slow.wait(timeout=10) is not None

        # Process death does not release the claim. The supervisor can only
        # recommend recovery after a stale-heartbeat assessment.
        active = store.list_active_leases()
        assert len(active) == 1
        assessment = assess_leases(active, stale_after=0.1,
                                   now=datetime.now(timezone.utc) + timedelta(seconds=1),
                                   node_types={"work": "agent"})[0]
        assert (assessment.state, assessment.recoverable) == ("stale", True)
        recovered = store.recover_model_lease(active[0].claim_id, reason="operator confirmed worker interruption")
        assert recovered.status.value == "ready"

        fast_env = dict(child_env, ANCHOR_RECOVERY_MODE="fast", ANCHOR_RECOVERY_WORKER_ID="fast-worker")
        fast = subprocess.Popen([sys.executable, "-c", CHILD], cwd=ROOT, env=fast_env,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + 15
        run = None
        while time.monotonic() < deadline:
            run = store.get_run(receipt.run_id)
            if run and run.status in {RunStatus.COMPLETED, RunStatus.FAILED, RunStatus.CANCELLED}:
                break
            time.sleep(0.05)
        assert run is not None and run.status is RunStatus.COMPLETED
        snapshot = store.list_context_snapshots(receipt.run_id)
        assert len(snapshot) == 1 and snapshot[0].snapshot == {"inputs": {"mode": "fast"}}
        assert store.list_node_runs(receipt.run_id)[0].output_ref.startswith("artifact://sha256/")
    finally:
        for process in (slow, fast):
            if process is not None and process.poll() is None:
                process.terminate()
        for process in (slow, fast):
            if process is not None:
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        store.close()
