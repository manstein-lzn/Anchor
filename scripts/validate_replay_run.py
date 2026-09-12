#!/usr/bin/env python
"""Acceptance for P1.1: a recorded run is reproducible with no provider reachable.

Two runs of one graph. The first is executed live and recorded. The second is a *replay*:
its model calls are served by call ordinal from the first run's recordings, and the gateway
it is given points at a port nothing listens on — so if the replay ever reached for the
model, it would fail rather than quietly answer.

That is the property being proved, and it is not about replay machinery: **given the model's
answers, the engine's path is determined.** Everything else about comparing behaviour rests
on it — without it, a change in outcome cannot be attributed to a prompt or a context policy
rather than to the engine itself.

    ANCHOR_DATABASE_URL=... .venv/bin/python scripts/validate_replay_run.py
"""

from __future__ import annotations

import asyncio
import json
import pathlib
import sys
import time
from typing import Any
from uuid import uuid4

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from anchor.domain.admission import RunRequest  # noqa: E402
from anchor.domain.graph import GraphDefinition, GraphVersion, Trigger  # noqa: E402
from anchor.runtime.academic import register_academic_behaviors  # noqa: E402
from anchor.runtime.artifacts import LocalArtifactStore  # noqa: E402
from anchor.runtime.behaviors import BehaviorRegistry  # noqa: E402
from anchor.runtime.capabilities import CapabilityRegistry, ModelProfile  # noqa: E402
from anchor.runtime.control_worker import ControlNodeWorker  # noqa: E402
from anchor.runtime.dispatch import dispatch_pending  # noqa: E402
from anchor.runtime.model_gateway import build_model_gateway  # noqa: E402
from anchor.runtime.model_recording import ModelRecorder, RecordingMode  # noqa: E402
from anchor.runtime.receiver import DurableExecutionReceiver  # noqa: E402
from anchor.runtime.secrets import ChainedSecretProvider, EnvironmentSecretProvider, JsonFileSecretProvider  # noqa: E402
from anchor.runtime.settings import AnchorSettings  # noqa: E402
from anchor.runtime.sinks import ArtifactCheckpointSink  # noqa: E402
from anchor.runtime.worker import AgentNodeWorker  # noqa: E402
from anchor.state.relational import RelationalStateStore  # noqa: E402

AGENT = "agents.academic.planner"
#: Events that record what the model was asked and answered. They are a projection, not the
#: execution path, and they are *required* to differ: a replay must not write new recordings
#: over the ones it reads. Everything else has to match exactly.
RECORDING_EVENTS = frozenset({"model.call", "model.call_replayed", "model.call_refused"})
#: A port nothing listens on. A replay that reached for the model would fail here rather
#: than silently answer from the network.
UNREACHABLE = "http://127.0.0.1:9/v1"

GRAPH = GraphDefinition.model_validate({
    "graph_id": "replay-acceptance",
    "name": "Replay acceptance",
    "entry_node_id": "start",
    "nodes": [
        {"id": "start", "type": "artifact", "name": "Start"},
        {"id": "one", "type": "agent", "name": "One", "agent_ref": AGENT},
        {"id": "two", "type": "agent", "name": "Two", "agent_ref": AGENT},
    ],
    "edges": [{"source": "start", "target": "one"}, {"source": "one", "target": "two"}],
})


def profile(base_url: str | None) -> ModelProfile:
    """The academic model profile, optionally pointed somewhere unreachable."""
    live = next(item for item in typed_config().models if item.ref == "models.academic")
    return live.model_copy(update={"base_url": base_url}) if base_url else live


def secrets() -> ChainedSecretProvider:
    config = typed_config()
    providers: list[Any] = [EnvironmentSecretProvider()]
    if config.secret_file:
        providers.append(JsonFileSecretProvider(config.secret_file))
    return ChainedSecretProvider(*providers)


def typed_config():
    from anchor.runtime.config import load_runtime_config

    return load_runtime_config(str(ROOT / ".local" / "runtime.json"))


def registry() -> CapabilityRegistry:
    config = typed_config()
    return CapabilityRegistry(models=config.models, agents=config.agents,
                              tools=config.tools, verifiers=config.verifiers)


async def admit(store, version_id, key: str) -> str:
    trigger = store.create_trigger(Trigger(graph_version_id=version_id, type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id, idempotency_key=key,
                                         objective="Plan a short literature review on cost models.",
                                         inputs={}))
    await dispatch_pending(store, DurableExecutionReceiver(store))
    return str(receipt.run_id)


async def execute(store, artifacts, run_id: str, gateway) -> list[str]:
    """Drive one run to completion with a control worker and an agent worker.

    Everything async happens inside one event loop, and every attempt is driven from it. The
    gateway's HTTP client binds to the loop it is first used in, so a second ``asyncio.run``
    fails with an error that reads as a provider fault and is not one — the node harness says
    the same thing, and this acceptance originally ignored it.
    """
    behaviors = BehaviorRegistry()
    register_academic_behaviors(behaviors)
    control = ControlNodeWorker(store, artifacts,
                                ArtifactCheckpointSink(store, artifacts, "control"),
                                behaviors=behaviors)
    worker = AgentNodeWorker(store, registry(), {profile(None).ref: gateway},
                             ArtifactCheckpointSink(store, artifacts, "agent"),
                             behaviors=behaviors)
    seen: list[str] = []
    for _ in range(20):
        before = len(store.list_node_runs(run_id))
        await control.execute_once(worker_id="control")
        lease = store.claim_ready_agent_node("agent", uuid4())
        if lease is not None:
            seen.append(lease.node_id)
            try:
                await worker.execute_claimed_once(
                    worker_id="agent", agent_ref=AGENT, lease=lease,
                    prompt="task", heartbeat_interval=0.01, input_snapshot={})
            except Exception as exc:  # noqa: BLE001 - a transient provider fault is not the
                # subject of this acceptance. The worker has already scheduled a retry (or
                # failed the run); keep driving so the retry happens rather than aborting the
                # comparison on the provider's behalf.
                print(f"   attempt on {lease.node_id} did not complete: {type(exc).__name__}")
                time.sleep(3)
        if store.get_run(run_id).status.value in ("completed", "failed", "cancelled"):
            break
        if len(store.list_node_runs(run_id)) == before and lease is None:
            break
    return seen


def path_of(store, run_id: str) -> dict[str, Any]:
    """The shape a replay has to reproduce: the status, the events, and the nodes.

    The execution path excludes the recording events, and says so rather than hiding them:
    both are reported, so the one difference that is expected is visible instead of filtered
    out of sight.
    """
    events = [item["event_type"] for item in store.list_events(run_id)]
    return {
        "status": store.get_run(run_id).status.value,
        "events": events,
        "execution_events": [name for name in events if name not in RECORDING_EVENTS],
        "nodes": sorted((item.node_id, item.status.value) for item in store.list_node_runs(run_id)),
    }


async def run_acceptance() -> int:
    settings = AnchorSettings()
    store = RelationalStateStore(settings.require_database_url())
    artifacts = LocalArtifactStore(settings.artifact_root)
    stamp = int(time.time())
    version = store.publish_graph(GraphVersion.publish(GRAPH, 1))
    version_id = version.graph_version_id
    report: dict[str, Any] = {"graph_version_id": str(version_id)}

    try:
        print("── 1. record a run, live ──")
        recorder = ModelRecorder(artifacts, mode=RecordingMode.RECORD, store=store)
        live = build_model_gateway(profile(None), secrets(), recorder=recorder)
        # The provider is intermittently flaky, and a run that died partway leaves a partial
        # recording — a replay of it then diverges for a reason that is not the subject here.
        # Retry until one run completes; report the attempts rather than hiding them.
        attempt = 0
        for attempt in range(1, 6):
            recorded_run = await admit(store, version_id, f"record-{stamp}-{attempt}")
            await execute(store, artifacts, recorded_run, live)
            if store.get_run(recorded_run).status.value == "completed":
                break
            print(f"   attempt {attempt}: the live run ended"
                  f" {store.get_run(recorded_run).status.value}; recording again")
        report["recorded"] = {"run_id": recorded_run, "calls": recorder.recorded,
                              "attempts": attempt, "path": path_of(store, recorded_run)}
        print(f"   run {recorded_run}: {recorder.recorded} call(s) recorded,"
              f" status {report['recorded']['path']['status']}")
        if report["recorded"]["path"]["status"] != "completed":
            raise SystemExit("could not record a completed run; the comparison would be "
                             "against a partial recording")

        print("── 2. replay it against a port nothing listens on ──")
        replay_recorder = ModelRecorder(artifacts, mode=RecordingMode.REPLAY, store=store,
                                        replay_of=recorded_run)
        replay_gateway = build_model_gateway(profile(UNREACHABLE), secrets(),
                                             recorder=replay_recorder)
        replay_run = await admit(store, version_id, f"replay-{stamp}")
        await execute(store, artifacts, replay_run, replay_gateway)
        report["replayed"] = {"run_id": replay_run, "calls": replay_recorder.recorded and None,
                              "path": path_of(store, replay_run)}
        print(f"   run {replay_run}: status {report['replayed']['path']['status']}")

        print("── 3. compare the two paths ──")
        left = report["recorded"]["path"]
        right = report["replayed"]["path"]
        report["comparison"] = {
            "same_status": left["status"] == right["status"],
            "same_execution_path": left["execution_events"] == right["execution_events"],
            "same_nodes": left["nodes"] == right["nodes"],
            "recorded_events": left["events"],
            "replayed_events": right["events"],
        }
        for key in ("same_status", "same_execution_path", "same_nodes"):
            print(f"   {key}: {report['comparison'][key]}")
        if left["events"] != right["events"]:
            print("   (the full event paths differ only in the recording events, which is "
                  "expected and required)")

        failures: list[str] = []
        if report["replayed"]["path"]["status"] not in ("completed", "failed"):
            failures.append("the replay did not reach a terminal state")
        if not report["comparison"]["same_status"]:
            failures.append(f"status differs: {left['status']} vs {right['status']}")
        if not report["comparison"]["same_nodes"]:
            failures.append(f"node paths differ: {left['nodes']} vs {right['nodes']}")
        if not report["comparison"]["same_execution_path"]:
            failures.append(f"execution paths differ: {left['execution_events']} vs "
                            f"{right['execution_events']}")
        report["failures"] = failures

        out = ROOT / ".local" / "reports" / f"replay-{replay_run}.json"
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        print("\nwritten:", out)
        if failures:
            print("\nFAILED:", "; ".join(failures))
            return 1
        print("\nPASSED")
        return 0
    finally:
        store.close()


def main() -> int:
    """One loop for the whole acceptance, because the gateway's client outlives any one call."""
    return asyncio.run(run_acceptance())


if __name__ == "__main__":
    raise SystemExit(main())
