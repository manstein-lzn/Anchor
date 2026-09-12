"""Long-lived control-node worker service."""

from __future__ import annotations

import asyncio
import logging
from uuid import uuid4

from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.control_worker import ControlNodeWorker
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.state.relational import RelationalStateStore


logger = logging.getLogger("anchor.control_worker")


async def run_control_loop(
    worker: ControlNodeWorker,
    *,
    worker_id: str,
    interval: float = 1.0,
    stop: asyncio.Event | None = None,
) -> None:
    if not worker_id or interval <= 0:
        raise ValueError("worker_id and positive interval are required")
    stop = stop or asyncio.Event()
    instance_id = uuid4()
    while not stop.is_set():
        try:
            worker.store.record_runtime_heartbeat("control_worker", instance_id)
            outcome = await worker.execute_once(worker_id=worker_id)
            if outcome is None:
                try:
                    await asyncio.wait_for(stop.wait(), timeout=interval)
                except asyncio.TimeoutError:
                    pass
        except asyncio.CancelledError:
            raise
        except Exception:
            logger.exception("control worker iteration failed; lease requires supervision")
            await asyncio.sleep(0)


async def serve() -> None:
    from anchor.runtime.settings import AnchorSettings

    from anchor.runtime.academic import register_academic_behaviors
    from anchor.runtime.behaviors import BehaviorRegistry
    from anchor.runtime.capabilities import CapabilityRegistry
    from anchor.runtime.config import load_runtime_config
    from anchor.runtime.tool_gateway import BubblewrapBackend, SubprocessBackend, ToolGateway

    settings = AnchorSettings()
    database_url = settings.require_database_url()
    worker_id = settings.control_worker_id
    store = RelationalStateStore(database_url)
    artifacts = LocalArtifactStore(settings.artifact_root)
    try:
        profile = load_runtime_config()
        registry = CapabilityRegistry(models=profile.models, agents=profile.agents,
                                      tools=profile.tools, verifiers=profile.verifiers)
    except RuntimeError:
        registry = CapabilityRegistry()
    try:
        backend = BubblewrapBackend()
    except RuntimeError:
        logger.warning("bubblewrap unavailable; tool execution falls back to dev subprocess")
        backend = SubprocessBackend()
    behaviors = BehaviorRegistry()
    register_academic_behaviors(behaviors)
    from anchor.runtime.content_commit import ContentCommitter
    from anchor.runtime.join_merge import register_core_behaviors
    from anchor.runtime.workspaces import WorkspaceManager
    workspaces = WorkspaceManager(store, root=settings.workspace_root)
    register_core_behaviors(behaviors, store, workspaces)
    worker = ControlNodeWorker(
        store, artifacts, ArtifactCheckpointSink(store, artifacts, worker_id),
        tools=ToolGateway(store, registry, artifacts, backend), registry=registry,
        behaviors=behaviors, committer=ContentCommitter(store, workspaces),
    )
    try:
        await run_control_loop(
            worker,
            worker_id=worker_id,
            interval=settings.control_worker_interval,
        )
    finally:
        store.close()


def main() -> None:
    from anchor.runtime.preflight import require_environment
    from anchor.runtime.settings import AnchorSettings

    settings = AnchorSettings()
    logging.basicConfig(level=settings.log_level)
    # No runtime profile: the control worker resolves behaviors from code, not config.
    require_environment(role="control_worker", database_url=settings.database_url,
                        artifact_root=settings.artifact_root)
    try:
        asyncio.run(serve())
    except KeyboardInterrupt:
        logger.info("control worker stopped")


if __name__ == "__main__":
    main()
