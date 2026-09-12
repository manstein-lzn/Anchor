"""Typed process configuration for Anchor services.

One ``BaseSettings`` replaces ad-hoc ``os.environ.get`` calls scattered across
service entrypoints. Values are validated at construction, so a malformed
interval fails fast instead of deep inside a worker loop.

Secret *values* never live here; this holds paths, worker IDs, intervals and
references only. Secret resolution stays in ``anchor.runtime.secrets``.
"""

from __future__ import annotations

from pydantic_settings import BaseSettings, SettingsConfigDict


class AnchorSettings(BaseSettings):
    """Process configuration, all variables prefixed with ``ANCHOR_``."""

    model_config = SettingsConfigDict(env_prefix="ANCHOR_", extra="ignore")

    database_url: str | None = None
    runtime_config: str = ".local/runtime.json"
    artifact_root: str = ".local/artifacts"
    workspace_root: str = ".local/workspaces"
    memory_path: str = ".local/memory.jsonl"
    # Model call recording: "off", "record" or "replay" (ADR-044). Kept as a
    # plain string because the recording module imports pydantic-ai at module
    # scope, and the API must remain usable in an install without that extra. The
    # worker service converts it to RecordingMode and rejects an unknown value.
    # Default off: a recording is a projection and production pays nothing for it.
    model_recording: str = "off"
    # When set, this process is a *replay* of that run: every model call it makes is
    # served by call ordinal from the recorded run. That is what makes a whole campaign
    # reproducible without a provider, and what separates "the model is nondeterministic"
    # from "our engine is". Empty means serve live.
    replay_of: str = ""
    # Cross-run cache for fetched research content. A projection, not canonical state: it lives
    # in its own directory, deleting it is supported, and nothing about a run's completion
    # depends on it. Empty disables it, which is the default because a cache that is on by
    # default is a cache nobody decided to have.
    content_cache_root: str = ""
    # Optional expiry. Unset means an entry never goes stale, which is right for immutable
    # published papers and wrong for anything that changes; the caller decides.
    content_cache_ttl_seconds: float | None = None
    api_token: str | None = None
    worker_id: str = "anchor-worker"
    control_worker_id: str = "anchor-control-worker"
    verifier_worker_id: str = "anchor-verifier-worker"
    worker_interval: float = 1.0
    control_worker_interval: float = 1.0
    verifier_worker_interval: float = 1.0
    dispatch_interval: float = 1.0
    scheduler_interval: float = 5.0
    supervisor_interval: float = 10.0
    lease_stale_after: float = 30.0
    # Explicit operator budgets are opt-in. When false, the supervisor does
    # not expire runs based on graph metadata budgets; healthy runs may
    # continue indefinitely without arbitrary numeric limits.
    expire_run_budgets: bool = False
    # Retention is a storage budget, never an execution budget: it only ever
    # evicts finished history, and never terminates a running node.
    storage_global_bytes: int | None = None
    storage_per_graph_bytes: int | None = None
    # Rolling retention. It only ever evicts finished history; nothing happens
    # while no budget is configured. Disable to keep budgets advisory only.
    storage_enforce: bool = True
    storage_sweep_interval: float = 60.0
    storage_sweep_batch: int = 25
    storage_sweep_max_rounds: int = 40
    log_level: str = "INFO"

    def require_database_url(self) -> str:
        """Return the database URL or fail with the historical message."""
        if not self.database_url:
            raise RuntimeError("set ANCHOR_DATABASE_URL explicitly")
        return self.database_url
