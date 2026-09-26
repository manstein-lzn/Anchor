"""Durable Pilot submissions and a replayable projection of framework UI events.

Messages remain in Harness. This database owns request deduplication, execution identity,
and delivery cursors; replaying its events never invokes the model or a tool.
"""
from __future__ import annotations

import json
import sqlite3
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from uuid import uuid4


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


class TurnStore:
    def __init__(self, root: Path):
        self.path = root / "state" / "pilot-turns.sqlite"
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.connect() as db:
            db.executescript("""
                CREATE TABLE IF NOT EXISTS turns (
                    id TEXT PRIMARY KEY, session TEXT NOT NULL, request_id TEXT NOT NULL,
                    prompt TEXT, status TEXT NOT NULL, error TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                    UNIQUE(session, request_id)
                );
                CREATE UNIQUE INDEX IF NOT EXISTS one_active_pilot_turn
                    ON turns(session) WHERE status = 'running';
                CREATE TABLE IF NOT EXISTS events (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    turn TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
                    data TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS turn_events ON events(turn, seq);
            """)

    @contextmanager
    def connect(self):
        db = sqlite3.connect(self.path, timeout=10)
        db.row_factory = sqlite3.Row
        db.execute("PRAGMA foreign_keys = ON")
        try:
            with db:
                yield db
        finally:
            db.close()

    def create(self, session: str, request_id: str, prompt: str | None) -> tuple[dict, bool]:
        with self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            found = db.execute("SELECT * FROM turns WHERE session=? AND request_id=?",
                               (session, request_id)).fetchone()
            if found:
                if found["prompt"] != prompt:
                    raise ValueError("request_id was already used for different input")
                return dict(found), False
            now = _now()
            identifier = str(uuid4())
            try:
                db.execute("INSERT INTO turns VALUES (?, ?, ?, ?, 'running', '', ?, ?)",
                           (identifier, session, request_id, prompt, now, now))
            except sqlite3.IntegrityError as exc:
                raise ValueError("that session is already processing a message") from exc
            return dict(db.execute("SELECT * FROM turns WHERE id=?", (identifier,)).fetchone()), True

    def find_request(self, session: str, request_id: str) -> dict | None:
        with self.connect() as db:
            row = db.execute("SELECT * FROM turns WHERE session=? AND request_id=?",
                             (session, request_id)).fetchone()
            return dict(row) if row else None

    def get(self, session: str, identifier: str) -> dict:
        with self.connect() as db:
            row = db.execute("SELECT * FROM turns WHERE session=? AND id=?", (session, identifier)).fetchone()
            if row is None:
                raise KeyError(identifier)
            return dict(row)

    def list(self, session: str) -> list[dict]:
        with self.connect() as db:
            return [dict(row) for row in db.execute(
                "SELECT * FROM turns WHERE session=? ORDER BY rowid DESC", (session,))]

    def append(self, identifier: str, data: dict) -> None:
        encoded = json.dumps(data, ensure_ascii=False)
        with self.connect() as db:
            db.execute("INSERT INTO events(turn, data) VALUES (?, ?)", (identifier, encoded))

    def events(self, session: str, identifier: str, after: int = 0) -> list[dict]:
        self.get(session, identifier)
        with self.connect() as db:
            return [{"seq": row["seq"], "data": json.loads(row["data"])} for row in db.execute(
                "SELECT seq, data FROM events WHERE turn=? AND seq>? ORDER BY seq LIMIT 256",
                (identifier, after))]

    def finish(self, identifier: str, status: str, error: str = "") -> None:
        if status not in {"completed", "failed", "stopped", "interrupted",
                          "waiting_approval", "waiting_user"}:
            raise ValueError("invalid turn outcome")
        with self.connect() as db:
            db.execute("UPDATE turns SET status=?,error=?,updated_at=? WHERE id=? AND status='running'",
                       (status, error, _now(), identifier))

    def interrupt_running(self) -> list[str]:
        # Single-service deployment: only called during Scheduler startup, never during a request.
        with self.connect() as db:
            rows = db.execute("SELECT DISTINCT session FROM turns WHERE status='running'").fetchall()
            db.execute("UPDATE turns SET status='interrupted',error=?,updated_at=? WHERE status='running'",
                       ("服务在执行期间退出；未自动重放，请检查执行记录。", _now()))
            return [row["session"] for row in rows]

    def unsafe_to_retry(self, session: str) -> bool:
        """Until P2 step recovery lands, never replay a failed turn that entered a mutating tool."""
        turns = self.list(session)
        mutating = {"graph_create", "graph_update", "graph_delete", "graph_run",
                    "run_pause", "run_resume", "run_stop"}
        import asyncio
        from pydantic_ai_harness.step_persistence import SqliteStepStore
        store = SqliteStepStore(database=self.path.parent / "pilot-steps.sqlite")
        for turn in turns:
            if turn["status"] == "completed":
                break
            events = asyncio.run(store.list_events(run_id=turn["id"]))
            if any(event.kind == "tool_call_started" and event.tool_name in mutating for event in events):
                return True
        return False

    def delete_session(self, session: str) -> None:
        with self.connect() as db:
            db.execute("DELETE FROM turns WHERE session=?", (session,))
