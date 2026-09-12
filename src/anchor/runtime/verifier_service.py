"""Long-lived Verifier worker service."""

from __future__ import annotations

import asyncio
import logging
from contextlib import suppress
from uuid import uuid4

from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.capabilities import CapabilityRegistry
from anchor.runtime.config import load_runtime_config
from anchor.runtime.model_gateway import build_model_gateway
from anchor.runtime.model_recording import ModelRecorder, RecordingMode
from anchor.runtime.secrets import ChainedSecretProvider, EnvironmentSecretProvider, JsonFileSecretProvider
from anchor.runtime.sinks import VerificationCheckpointSink
from anchor.runtime.verifier import VerifierNodeWorker
from anchor.state.relational import RelationalStateStore


logger = logging.getLogger("anchor.verifier_worker")


async def run_verifier_loop(
    worker: VerifierNodeWorker,
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
            worker.store.record_runtime_heartbeat("verifier_worker", instance_id)
            outcome = await worker.execute_once(worker_id=worker_id)
            if outcome is None:
                try:
                    await asyncio.wait_for(stop.wait(), timeout=interval)
                except asyncio.TimeoutError:
                    pass
        except asyncio.CancelledError:
            raise
        except Exception:
            logger.exception("verifier worker iteration failed; lease requires supervision")
            await asyncio.sleep(0)


async def serve() -> None:
    from anchor.runtime.settings import AnchorSettings

    settings = AnchorSettings()
    config = load_runtime_config()
    database_url = settings.require_database_url()
    worker_id = settings.verifier_worker_id
    store = RelationalStateStore(database_url)
    providers = [EnvironmentSecretProvider()]
    if config.secret_file:
        providers.append(JsonFileSecretProvider(config.secret_file))
    secrets = ChainedSecretProvider(*providers)
    registry = CapabilityRegistry(
        models=config.models,
        agents=config.agents,
        tools=config.tools,
        verifiers=config.verifiers,
    )
    required_models = {
        item.model_ref for item in config.verifiers if item.model_ref is not None
    }
    artifacts = LocalArtifactStore(settings.artifact_root)
    # Verifier nodes call models too, so they record on the same terms as agent
    # nodes: any node's actual prompt should be readable afterwards.
    recorder = ModelRecorder(artifacts, mode=RecordingMode(settings.model_recording),
                             store=store, replay_of=settings.replay_of or None)
    gateways = {
        profile.ref: build_model_gateway(profile, secrets, recorder=recorder)
        for profile in config.models if profile.ref in required_models
    }
    worker = VerifierNodeWorker(
        store,
        registry,
        gateways,
        artifacts,
        VerificationCheckpointSink(store, worker_id),
    )
    try:
        await run_verifier_loop(
            worker,
            worker_id=worker_id,
            interval=settings.verifier_worker_interval,
        )
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
    require_environment(role="verifier_worker", database_url=settings.database_url,
                        runtime_config=settings.runtime_config,
                        artifact_root=settings.artifact_root)
    try:
        asyncio.run(serve())
    except KeyboardInterrupt:
        logger.info("verifier worker stopped")


if __name__ == "__main__":
    main()
