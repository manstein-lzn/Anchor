"""Development worker process entrypoint.

It intentionally requires an explicit runtime config and database URL. No
implicit migration or fake Agent capability is created by this command.
"""
from __future__ import annotations

import asyncio
import logging
from contextlib import suppress
from uuid import UUID

from anchor.runtime.academic import register_academic_behaviors
from anchor.runtime.agent_tools import AgentToolLoop
from anchor.runtime.behaviors import BehaviorRegistry
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import CapabilityRegistry
from anchor.runtime.tool_gateway import BubblewrapBackend, SubprocessBackend, ToolGateway
from anchor.runtime.config import load_runtime_config
from anchor.runtime.content_cache import ContentCache
from anchor.runtime.model_gateway import build_model_gateway
from anchor.runtime.model_recording import ModelRecorder, RecordingMode
from anchor.runtime.secrets import ChainedSecretProvider, EnvironmentSecretProvider, JsonFileSecretProvider
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.runtime.worker import AgentNodeWorker
from anchor.runtime.worker_loop import ResolvedPrompt, run_worker_loop
from anchor.runtime.memory import LocalMemoryStore, MemoryStore
from anchor.runtime.node_prompt import PromptParts, prompt_segments, render
from anchor.runtime.resolution import resolve_node_context
from anchor.state.relational import RelationalStateStore


async def _resolver(store, run_id: UUID, node_id: str, memory: MemoryStore | None = None,
                    artifacts: LocalArtifactStore | None = None) -> ResolvedPrompt:
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
    # The prompt is assembled by the shared function the node harness also uses, so
    # an experiment there measures this prompt rather than a lookalike.
    parts = PromptParts(
        objective=resolved.task.objective,
        node_name=resolved.node.name,
        snapshot=resolved.snapshot,
        run_memory=[item.content for item in run_memories],
        promoted_memory=[(item.domain or "general", item.content) for item in promoted])
    # The segments travel with the prompt so the spend report can say which part moved. A
    # stable prefix is served from the provider's cache and is nearly free; one that changes
    # on every call is billed in full, and the token counts cannot tell the two apart.
    # The prefix is left empty here because the instructions come from the capability, which
    # the worker resolves and is what actually sends the system prompt.
    segments = prompt_segments(parts)
    return ResolvedPrompt(agent_ref=resolved.node.agent_ref, prompt=render(segments),
                          expected_node_id=resolved.node.id,
                          input_snapshot=resolved.snapshot, segments=segments)


def _workspace_toolset(store, settings):
    """Native workspace tools, with a sandbox for workspace.exec.

    bubblewrap is the only enforcement backend: it is a single unprivileged
    binary with no daemon, which is the smallest stable isolation available.
    If it is missing, workspace.exec is disabled rather than silently degrading
    to an unisolated subprocess; reading and writing still work.
    """
    from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox
    from anchor.runtime.workspace_tools import WorkspaceToolset
    from anchor.runtime.workspaces import WorkspaceManager
    try:
        sandbox = BubblewrapWorkspaceSandbox()
    except RuntimeError:
        logging.getLogger("anchor.worker").warning(
            "bubblewrap unavailable; workspace.exec is disabled (read/write remain available)")
        sandbox = None
    manager = WorkspaceManager(store, root=settings.workspace_root)
    return manager, WorkspaceToolset(store, manager, sandbox=sandbox)


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
    artifacts = LocalArtifactStore(settings.artifact_root)
    # One recorder for every profile: the node attempt is carried in a ContextVar
    # by the worker, not by the gateway, because a gateway is built once and one
    # shared model serves every node. The mode is validated here so a typo fails
    # at startup rather than silently recording nothing.
    recorder = ModelRecorder(artifacts, mode=RecordingMode(settings.model_recording),
                             store=store, replay_of=settings.replay_of or None)
    gateways = {profile.ref: build_model_gateway(profile, secrets, recorder=recorder)
                for profile in config.models}
    if recorder.enabled:
        logging.getLogger("anchor.worker").info("model call recording is %s",
                                                recorder.mode.value)
    memory = LocalMemoryStore(settings.memory_path)
    sink = ArtifactCheckpointSink(store, artifacts, worker_id)
    try:
        backend = BubblewrapBackend()
    except RuntimeError:
        logger = logging.getLogger("anchor.worker")
        logger.warning("bubblewrap unavailable; tool execution falls back to dev subprocess")
        backend = SubprocessBackend()
    workspace_manager, workspace_tools = _workspace_toolset(store, settings)
    tool_gateway = ToolGateway(store, registry, artifacts, backend)
    if settings.content_cache_root:
        # Off unless configured: a cache that is on by default is a cache nobody decided to have,
        # and its whole cost model depends on what is being fetched.
        tool_gateway.content_cache = ContentCache(
            settings.content_cache_root, ttl_seconds=settings.content_cache_ttl_seconds)
    tool_loop = AgentToolLoop(tool_gateway, artifacts, native=workspace_tools)
    from anchor.runtime.content_commit import ContentCommitter
    committer = ContentCommitter(store, workspace_manager)
    behaviors = BehaviorRegistry()
    register_academic_behaviors(behaviors)
    worker = AgentNodeWorker(store, registry, gateways, sink, tool_loop=tool_loop,
                             behaviors=behaviors, committer=committer)
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
    from anchor.runtime.preflight import require_environment
    from anchor.runtime.settings import AnchorSettings

    settings = AnchorSettings()
    logging.basicConfig(level=settings.log_level)
    # Before the loop, not inside it: a worker that starts against a broken environment
    # either crash-loops invisibly or runs and does nothing, and both look healthy.
    require_environment(role="agent_worker", database_url=settings.database_url,
                        runtime_config=settings.runtime_config,
                        artifact_root=settings.artifact_root)
    try:
        asyncio.run(serve())
    except KeyboardInterrupt:
        logging.getLogger("anchor.worker").info("worker stopped")


if __name__ == "__main__":
    main()
