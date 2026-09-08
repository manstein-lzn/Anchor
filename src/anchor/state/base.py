"""Relational store core: connections, transactions, append-only events, heartbeats."""

from __future__ import annotations

import hashlib
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from uuid import UUID

import sqlalchemy as sa

from anchor.domain.models import utc_now
from . import schema as s
from .errors import ConcurrencyConflict, DuplicateEvent


WAITING_NODE_TYPES = frozenset({"approval", "human_task", "wait_for_event"})


def wait_status_for(node_type: str) -> str | None:
    """Ready-state override for nodes that wait instead of executing."""
    if node_type in ("approval", "human_task"):
        return "waiting_approval"
    if node_type == "wait_for_event":
        return "waiting_event"
    return None


def values(model):
    result = model.model_dump(mode="python")
    return {key: str(value) if isinstance(value, UUID) else value for key, value in result.items()}


def decode(model, row):
    if row is None:
        return None
    data = dict(row)
    # SQLite drops timezone metadata; all persisted application timestamps are UTC.
    for key, value in data.items():
        if isinstance(value, datetime) and value.tzinfo is None:
            data[key] = value.replace(tzinfo=timezone.utc)
    return model.model_validate(data)


class StoreBase:
    """Transaction, lock and event primitives shared by every store mixin."""
    def __init__(self, url: str):
        self.engine = sa.create_engine(url, pool_pre_ping=True)
        if self.engine.dialect.name not in {"sqlite", "postgresql"}:
            self.engine.dispose()
            raise ValueError("only SQLite and PostgreSQL are supported")
        if self.engine.dialect.name == "sqlite":
            @sa.event.listens_for(self.engine, "connect")
            def configure(connection, record):
                connection.execute("PRAGMA foreign_keys=ON")
                connection.execute("PRAGMA busy_timeout=10000")
    def close(self):
        self.engine.dispose()
    @contextmanager
    def _transaction(self):
        with self.engine.connect() as connection:
            if self.engine.dialect.name == "sqlite":
                connection.exec_driver_sql("BEGIN IMMEDIATE")
            else:
                connection.begin()
            try:
                yield connection
                connection.commit()
            except BaseException:
                connection.rollback()
                raise
    def _lock(self, connection, key):
        if self.engine.dialect.name == "postgresql":
            token = int.from_bytes(hashlib.sha256(key.encode()).digest()[:8], "big", signed=True)
            connection.execute(sa.select(sa.func.pg_advisory_xact_lock(token)))
    def _read(self, table, model, identity):
        with self.engine.connect() as connection:
            row = connection.execute(sa.select(table).where(list(table.primary_key)[0] == str(identity))).mappings().first()
            return decode(model, row)
    def _locked_running_node(self, connection, claim_id, worker_id):
        """Lock the lease and node, enforcing the worker-owns-running-node invariant."""
        lease = connection.execute(sa.select(s.node_leases).where(
            s.node_leases.c.claim_id == str(claim_id)).with_for_update()).mappings().first()
        if lease is None:
            raise KeyError(claim_id)
        if lease["worker_id"] != worker_id or lease["released_at"] is not None:
            raise ConcurrencyConflict("lease is not owned by worker")
        node = connection.execute(sa.select(s.node_runs).where(
            s.node_runs.c.id == lease["node_run_id"]).with_for_update()).mappings().one()
        if node["status"] != "running":
            raise ConcurrencyConflict("node is not running")
        return lease, node

    def _insert(self, connection, table, model):
        connection.execute(sa.insert(table).values(**values(model)))
        return model
    def event_page(self, run_id: UUID, after: int = 0, limit: int = 100) -> list[dict]:
        with self.engine.connect() as connection:
            return [dict(row) for row in connection.execute(sa.select(s.events).where(
                s.events.c.stream_id == str(run_id), s.events.c.sequence > after)
                .order_by(s.events.c.sequence).limit(limit)).mappings()]
    def check_schema(self) -> None:
        with self.engine.connect() as connection:
            version = connection.execute(sa.text("SELECT version_num FROM alembic_version")).scalar_one()
            if version not in {
                "0012_lease_history",
                "0013_progress_evidence",
                "0014_recovery_schedule",
            }:
                raise RuntimeError("database schema is not at the supported revision")
    def _append_event(self, connection, *, stream_id, event_type, payload, idempotency_key):
        self._lock(connection, f"stream:{stream_id}")
        prior = connection.execute(sa.select(s.events).where(
            s.events.c.stream_id == str(stream_id), s.events.c.idempotency_key == idempotency_key,
        )).mappings().first()
        if prior:
            if prior["event_type"] != event_type or prior["payload"] != payload:
                raise DuplicateEvent(idempotency_key)
            return prior["sequence"]
        sequence = connection.execute(sa.select(sa.func.coalesce(sa.func.max(s.events.c.sequence), 0) + 1)
                                      .where(s.events.c.stream_id == str(stream_id))).scalar_one()
        connection.execute(sa.insert(s.events).values(stream_id=str(stream_id), sequence=sequence,
            event_type=event_type, payload=payload, idempotency_key=idempotency_key, created_at=utc_now()))
        return sequence
    def append_event(self, **kwargs) -> int:
        with self._transaction() as connection:
            return self._append_event(connection, **kwargs)
    def list_events(self, stream_id: UUID) -> list[dict]:
        with self.engine.connect() as connection:
            return [dict(row) for row in connection.execute(sa.select(s.events).where(
                s.events.c.stream_id == str(stream_id)).order_by(s.events.c.sequence)).mappings()]
    def record_runtime_heartbeat(self, component: str, instance_id: UUID) -> None:
        now = utc_now()
        with self._transaction() as connection:
            existing = connection.execute(sa.select(s.runtime_heartbeats.c.component).where(
                s.runtime_heartbeats.c.component == component)).scalar_one_or_none()
            values_ = {"instance_id": str(instance_id), "observed_at": now}
            if existing:
                connection.execute(sa.update(s.runtime_heartbeats).where(
                    s.runtime_heartbeats.c.component == component).values(**values_))
            else:
                connection.execute(sa.insert(s.runtime_heartbeats).values(component=component, **values_))
    def runtime_connected(self, component: str, *, within_seconds: int = 15) -> bool:
        if within_seconds <= 0:
            raise ValueError("heartbeat freshness must be positive")
        with self.engine.connect() as connection:
            observed = connection.execute(sa.select(s.runtime_heartbeats.c.observed_at).where(
                s.runtime_heartbeats.c.component == component)).scalar_one_or_none()
        if observed is None:
            return False
        if observed.tzinfo is None:
            observed = observed.replace(tzinfo=timezone.utc)
        return observed >= utc_now() - timedelta(seconds=within_seconds)
    def _advance_run_event_pointer(self, connection, run_id: UUID, sequence: int) -> None:
        row = connection.execute(sa.select(s.runs.c.revision).where(
            s.runs.c.id == str(run_id)).with_for_update()).mappings().one()
        connection.execute(sa.update(s.runs).where(s.runs.c.id == str(run_id)).values(
            revision=row["revision"] + 1, last_event_sequence=sequence, updated_at=utc_now()))
