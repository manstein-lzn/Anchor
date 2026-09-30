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
    # Downloaded platform media.  Paths are host-side facts and are converted to a read-only
    # ``/in/channel`` mount before a Graph run is started.
    attachments: tuple[dict[str, Any], ...] = ()

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
            for column in ("conversation_id", "sender_id"):
                if column not in columns:
                    db.execute(f"ALTER TABLE channel_events ADD COLUMN {column} TEXT NOT NULL DEFAULT ''")
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
                "SELECT status, updated_at, conversation_id, sender_id FROM channel_events WHERE source=? AND event_id=?",
                (event.source, event.event_id),
            ).fetchone()
            if row is None:
                db.execute(
                    "INSERT INTO channel_events(source,event_id,status,updated_at,conversation_id,sender_id) "
                    "VALUES (?, ?, 'processing', ?, ?, ?)",
                    (event.source, event.event_id, now.isoformat(), event.conversation_id, event.sender_id),
                )
                return True
            if row["conversation_id"] and (row["conversation_id"], row["sender_id"]) != (
                    event.conversation_id, event.sender_id):
                return False
            # Upgrade legacy rows without changing their original insertion order.
            db.execute("UPDATE channel_events SET conversation_id=?,sender_id=? "
                       "WHERE source=? AND event_id=? AND conversation_id=''",
                       (event.conversation_id, event.sender_id, event.source, event.event_id))
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

    def is_latest(self, event: ChannelEvent) -> bool:
        """First admission order survives reconnects; retries never become new input.

        Legacy rows with unknown conversation are treated conservatively until replay fills
        their identity. No channel rows are deleted, so SQLite rowid retains insertion order.
        """
        with self._connect() as db:
            row = db.execute(
                "SELECT rowid FROM channel_events WHERE source=? AND event_id=? AND conversation_id=? AND sender_id=?",
                (event.source, event.event_id, event.conversation_id, event.sender_id)).fetchone()
            if row is None:
                return False
            newer = db.execute(
                "SELECT 1 FROM channel_events WHERE source=? AND rowid>? AND "
                "((conversation_id=? AND sender_id=?) OR conversation_id='') LIMIT 1",
                (event.source, row["rowid"], event.conversation_id, event.sender_id)).fetchone()
            return newer is None

    def prepare_reply(self, event: ChannelEvent, reply: str) -> None:
        """Save the handler result before attempting platform delivery."""
        with self._connect() as db:
            db.execute(
                "UPDATE channel_events SET status='ready', updated_at=?, error='', reply=? "
                "WHERE source=? AND event_id=?",
                (_now(), reply, event.source, event.event_id),
            )

    def claim_send(self, event: ChannelEvent, digest: str) -> dict | None:
        """Claim a non-replayable external send, or return its already confirmed result."""
        with self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute("SELECT status, reply FROM channel_events WHERE source=? AND event_id=?",
                             (event.source, event.event_id)).fetchone()
            if row:
                if row["reply"] != digest:
                    raise ValueError("send request_id was already used for different content")
                if row["status"] == "completed":
                    return {"accepted": True, "duplicate": True, "request_id": event.event_id}
                raise RuntimeError("previous delivery is unconfirmed; do not resend automatically")
            db.execute("INSERT INTO channel_events(source,event_id,status,updated_at,reply) VALUES(?,?,?,?,?)",
                       (event.source, event.event_id, "processing", _now(), digest))
        return None

    def pending_reply(self, event: ChannelEvent) -> str | None:
        with self._connect() as db:
            row = db.execute(
                "SELECT status, reply FROM channel_events WHERE source=? AND event_id=? AND conversation_id=? AND sender_id=?",
                (event.source, event.event_id, event.conversation_id, event.sender_id),
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
