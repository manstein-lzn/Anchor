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
    memory_path: str = ".local/memory.jsonl"
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
    log_level: str = "INFO"

    def require_database_url(self) -> str:
        """Return the database URL or fail with the historical message."""
        if not self.database_url:
            raise RuntimeError("set ANCHOR_DATABASE_URL explicitly")
        return self.database_url
