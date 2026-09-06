"""Development worker process entrypoint.

It intentionally requires an explicit runtime config and database URL. No
implicit migration or fake Agent capability is created by this command.
"""
from __future__ import annotations

import asyncio
import logging
from contextlib import suppress
from uuid import UUID

from anchor.runtime.agent_tools import AgentToolLoop
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import CapabilityRegistry
from anchor.runtime.tool_gateway import BubblewrapBackend, SubprocessBackend, ToolGateway
from anchor.runtime.config import load_runtime_config
from anchor.runtime.model_gateway import build_model_gateway
from anchor.runtime.secrets import ChainedSecretProvider, EnvironmentSecretProvider, JsonFileSecretProvider
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.runtime.worker import AgentNodeWorker
from anchor.runtime.worker_loop import run_worker_loop
from anchor.runtime.memory import LocalMemoryStore, MemoryStore
from anchor.runtime.context import canonical_json
from anchor.runtime.resolution import resolve_node_context
from anchor.state.relational import RelationalStateStore


async def _resolver(store, run_id: UUID, node_id: str, memory: MemoryStore | None = None,
                    artifacts: LocalArtifactStore | None = None) -> tuple[str, str, str, dict]:
    from anchor.runtime.integrity import require_clean

    # Mechanical anti-drift gate: refuse to build a prompt from corrupt
    # canonical state. Raises IntegrityError; the worker loop leaves the
    # lease for supervision, exactly like a model transport failure.
    require_clean(store, run_id,
                  read_artifact=(artifacts.get_text if artifacts is not None else None))
    resolved = resolve_node_context(store, run_id, node_id, artifacts)
    if not resolved.node.agent_ref:
        raise ValueError(f"node {node_id} is not an executable Agent node")
    run_memories = memory.list(run_id=run_id) if memory is not None else []
    seen = {item.memory_id for item in run_memories}
    promoted = [item for item in (memory.list(status="promoted") if memory is not None else [])
                if item.memory_id not in seen]
    run_block = "\n".join(f"- {item.content}" for item in run_memories)
    promoted_block = "\n".join(f"- [{item.domain or 'general'}] {item.content}"
                                  for item in promoted)
    prompt = (f"Task objective:\n{resolved.task.objective}\n\nExecute graph node: {resolved.node.name}\n"
              f"\nDurable input snapshot:\n{canonical_json(resolved.snapshot)}"
              f"\n\nRun memory:\n{run_block or '(none)'}"
              f"\n\nPromoted organizational knowledge:\n{promoted_block or '(none)'}")
    return resolved.node.agent_ref, prompt, resolved.node.id, resolved.snapshot


async def serve() -> None:
    from anchor.runtime.settings import AnchorSettings

    settings = AnchorSettings()
    config = load_runtime_config()
    if not config.models:
        raise RuntimeError("runtime config must define at least one model")
    database_url = settings.require_database_url()
    worker_id = settings.worker_id
    store = RelationalStateStore(database_url)
    secret_providers = [EnvironmentSecretProvider()]
    if config.secret_file:
        secret_providers.append(JsonFileSecretProvider(config.secret_file))
    secrets = ChainedSecretProvider(*secret_providers)
    registry = CapabilityRegistry(models=config.models, agents=config.agents, tools=config.tools,
                                  verifiers=config.verifiers)
    gateways = {profile.ref: build_model_gateway(profile, secrets) for profile in config.models}
    artifacts = LocalArtifactStore(settings.artifact_root)
    memory = LocalMemoryStore(settings.memory_path)
    sink = ArtifactCheckpointSink(store, artifacts, worker_id)
    try:
        backend = BubblewrapBackend()
    except RuntimeError:
        logger = logging.getLogger("anchor.worker")
        logger.warning("bubblewrap unavailable; tool execution falls back to dev subprocess")
        backend = SubprocessBackend()
    tool_loop = AgentToolLoop(ToolGateway(store, registry, artifacts, backend), artifacts)
    worker = AgentNodeWorker(store, registry, gateways, sink, tool_loop=tool_loop)
    stop = asyncio.Event()
    try:
        await run_worker_loop(worker, worker_id=worker_id,
                              resolve_prompt=lambda run_id, node_id: _resolver(store, run_id, node_id, memory,
                                                                               artifacts),
                              interval=settings.worker_interval, stop=stop)
    finally:
        for gateway in gateways.values():
            close = getattr(gateway, "close", None)
            if close is not None:
                with suppress(Exception):
                    await close()
        store.close()


def main() -> None:
    from anchor.runtime.settings import AnchorSettings

    logging.basicConfig(level=AnchorSettings().log_level)
    try:
        asyncio.run(serve())
    except KeyboardInterrupt:
        logging.getLogger("anchor.worker").info("worker stopped")


if __name__ == "__main__":
    main()
