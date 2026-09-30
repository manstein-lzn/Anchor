"""Small contracts shared by long-lived platform channels."""

from __future__ import annotations

import sqlite3
from dataclasses import asdict, dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


@dataclass(frozen=True)
class ChannelEvent:
    """A platform message after adapter validation and normalization."""

    source: str
    event_id: str
    sender_id: str
    conversation_id: str
    text: str = ""
    reply_target: str = ""
    message_type: str = "text"
    metadata: dict[str, Any] = field(default_factory=dict)

    def as_dict(self) -> dict[str, Any]:
        return asdict(self)


class EventLedger:
    """SQLite-backed event claim ledger for at-least-once channel delivery.

    A completed event is ignored on redelivery. A failed event can be claimed again; a stale
    processing claim is reclaimed after ``lease_seconds`` so a process exit cannot lose it forever.
    """

    def __init__(self, path: Path, *, lease_seconds: int = 300):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.lease_seconds = lease_seconds
        with self._connect() as db:
            db.execute("PRAGMA journal_mode = WAL")
            db.execute("""
                CREATE TABLE IF NOT EXISTS channel_events (
                    source TEXT NOT NULL,
                    event_id TEXT NOT NULL,
                    status TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    error TEXT NOT NULL DEFAULT '',
                    reply TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY (source, event_id)
                )
            """)
            columns = {row["name"] for row in db.execute("PRAGMA table_info(channel_events)")}
            if "reply" not in columns:
                db.execute("ALTER TABLE channel_events ADD COLUMN reply TEXT NOT NULL DEFAULT ''")

    def _connect(self) -> sqlite3.Connection:
        db = sqlite3.connect(self.path, timeout=10)
        db.row_factory = sqlite3.Row
        return db

    def claim(self, event: ChannelEvent) -> bool:
        now = datetime.now(timezone.utc)
        stale = (now - timedelta(seconds=self.lease_seconds)).isoformat()
        with self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute(
                "SELECT status, updated_at FROM channel_events WHERE source=? AND event_id=?",
                (event.source, event.event_id),
            ).fetchone()
            if row is None:
                db.execute(
                    "INSERT INTO channel_events VALUES (?, ?, 'processing', ?, '', '')",
                    (event.source, event.event_id, now.isoformat()),
                )
                return True
            if row["status"] in {"completed", "ready"}:
                return False
            if row["status"] == "processing" and row["updated_at"] >= stale:
                return False
            db.execute(
                "UPDATE channel_events SET status='processing', updated_at=?, error='' "
                "WHERE source=? AND event_id=?",
                (now.isoformat(), event.source, event.event_id),
            )
            return True

    def prepare_reply(self, event: ChannelEvent, reply: str) -> None:
        """Save the handler result before attempting platform delivery."""
        with self._connect() as db:
            db.execute(
                "UPDATE channel_events SET status='ready', updated_at=?, error='', reply=? "
                "WHERE source=? AND event_id=?",
                (_now(), reply, event.source, event.event_id),
            )

    def pending_reply(self, event: ChannelEvent) -> str | None:
        with self._connect() as db:
            row = db.execute(
                "SELECT status, reply FROM channel_events WHERE source=? AND event_id=?",
                (event.source, event.event_id),
            ).fetchone()
        return row["reply"] if row and row["status"] == "ready" else None

    def complete(self, event: ChannelEvent) -> None:
        with self._connect() as db:
            db.execute(
                "UPDATE channel_events SET status='completed', updated_at=?, error='' "
                "WHERE source=? AND event_id=?",
                (_now(), event.source, event.event_id),
            )

    def fail(self, event: ChannelEvent, error: str) -> None:
        with self._connect() as db:
            db.execute(
                "UPDATE channel_events SET status='failed', updated_at=?, error=? "
                "WHERE source=? AND event_id=?",
                (_now(), error[:1000], event.source, event.event_id),
            )

    def fail_delivery(self, event: ChannelEvent, error: str) -> None:
        """Keep a prepared reply retryable without rerunning the handler."""
        with self._connect() as db:
            db.execute(
                "UPDATE channel_events SET status='ready', updated_at=?, error=? "
                "WHERE source=? AND event_id=? AND status='ready'",
                (_now(), error[:1000], event.source, event.event_id),
            )


__all__ = ["ChannelEvent", "EventLedger"]
