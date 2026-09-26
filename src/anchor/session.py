"""Small, file-backed Anchor Session store.

Anchor owns lifecycle and activity events; PydanticAI Harness owns conversation messages.
"""

from __future__ import annotations

import os
import tempfile
import threading
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Literal
from uuid import uuid4

from pydantic import BaseModel, Field

SessionStatus = Literal["active", "waiting_user", "interrupted", "archived"]


def _now() -> datetime:
    return datetime.now(timezone.utc)


class Session(BaseModel):
    id: str
    conversation_id: str
    title: str = ""
    status: SessionStatus = "active"
    waiting_reason: str = ""
    run_ids: list[str] = Field(default_factory=list)
    approval: dict[str, Any] | None = None
    approvals: list[dict[str, Any]] = Field(default_factory=list)
    questions: list[dict[str, Any]] = Field(default_factory=list)
    operation: dict[str, Any] | None = None
    operations: dict[str, dict[str, Any]] = Field(default_factory=dict)
    created_at: datetime
    updated_at: datetime


class SessionEvent(BaseModel):
    seq: int
    at: datetime
    kind: str
    data: dict[str, Any] = Field(default_factory=dict)


@dataclass(frozen=True)
class SessionStore:
    """Persist Anchor session facts under one root directory."""

    root: Path
    # ponytail: process-local lock; multi-process deployment needs a transactional store.
    _approval_lock: threading.RLock = field(default_factory=threading.RLock,
                                             init=False, repr=False, compare=False)

    def __post_init__(self) -> None:
        object.__setattr__(self, "root", Path(self.root).resolve())

    @property
    def sessions_dir(self) -> Path:
        return self.root / "sessions"

    @property
    def conversation_database(self) -> Path:
        return self.root / "state" / "pilot-conversations.sqlite"

    def _path(self, session_id: str) -> Path:
        if not session_id or "/" in session_id or "\\" in session_id or session_id in {".", ".."}:
            raise ValueError("invalid session id")
        return self.sessions_dir / session_id

    def _read(self, session_id: str) -> Session:
        path = self._path(session_id) / "session.json"
        try:
            session = Session.model_validate_json(path.read_text(encoding="utf-8"))
        except FileNotFoundError as exc:
            raise KeyError(session_id) from exc
        except (OSError, ValueError) as exc:
            raise ValueError(f"invalid session {session_id}: {exc}") from exc
        if session.approval and not session.approvals:
            # ponytail: a pending request from the pre-deferred flow holds no framework tool call to
            # resume, so it is dropped instead of showing a confirmation nobody can complete.
            session = session.model_copy(update={"approval": None,
                                                 "status": "active", "waiting_reason": ""})
        if session.operation:
            # Its content hash can never be produced again, so the record is kept for diagnosis only.
            legacy = session.operation
            session = session.model_copy(update={
                "operation": None,
                "operations": {**session.operations, legacy.get("intent_key") or "legacy": legacy}})
        return session

    @staticmethod
    def _atomic(path: Path, text: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                handle.write(text)
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, path)
        finally:
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass

    def _write(self, session: Session) -> Session:
        self._atomic(self._path(session.id) / "session.json", session.model_dump_json(indent=2) + "\n")
        return session

    def create(self, session_id: str | None = None) -> Session:
        now = _now()
        identifier = session_id or str(uuid4())
        session = Session(id=identifier, conversation_id=identifier,
                          created_at=now, updated_at=now)
        directory = self._path(identifier)
        if directory.exists():
            raise FileExistsError(identifier)
        self._write(session)
        self._append(session, "session.created")
        import asyncio
        asyncio.run(self.conversation_store().save(
            summary=self._conversation_summary(session), messages=[]))
        return session

    def get(self, session_id: str) -> Session:
        return self._read(session_id)

    def name_from_prompt(self, session_id: str, prompt: str) -> None:
        """Derive a short navigation label; conversation messages remain in Harness."""
        with self._approval_lock:
            session = self._read(session_id)
            if not session.title:
                self._write(session.model_copy(update={"title": " ".join(prompt.split())[:60]}))

    def list(self) -> list[Session]:
        if not self.sessions_dir.is_dir():
            return []
        sessions = []
        for directory in sorted(self.sessions_dir.iterdir()):
            if directory.is_dir() and (directory / "session.json").is_file():
                sessions.append(self._read(directory.name))
        return sorted(sessions, key=lambda item: item.updated_at, reverse=True)

    def events(self, session_id: str) -> list[SessionEvent]:
        self._read(session_id)
        path = self._path(session_id) / "events.jsonl"
        if not path.exists():
            return []
        result = []
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                result.append(SessionEvent.model_validate_json(line))
        return result

    def append(self, session_id: str, kind: str, data: dict[str, Any] | None = None) -> SessionEvent:
        with self._approval_lock:
            session = self._read(session_id)
            event = self._append(session, kind, data or {})
            self._write(session.model_copy(update={"updated_at": event.at}))
            return event

    def set_status(self, session_id: str, status: SessionStatus, *, reason: str = "") -> Session:
        with self._approval_lock:
            session = self._read(session_id)
            if session.status == "archived" and status != "archived":
                raise ValueError("archived sessions cannot be resumed")
            updated = session.model_copy(update={"status": status, "waiting_reason": reason,
                                                 "updated_at": _now()})
            self._write(updated)
            self._append(updated, f"session.{status}", {"reason": reason} if reason else {})
            return updated

    def attach_run(self, session_id: str, run_id: str) -> Session:
        with self._approval_lock:
            session = self._read(session_id)
            if run_id not in session.run_ids:
                session = session.model_copy(update={"run_ids": [*session.run_ids, run_id],
                                                     "updated_at": _now()})
                self._write(session)
                self._append(session, "run.attached", {"run": run_id})
            return session

    def set_pending(self, session_id: str, approvals: list[dict[str, Any]],
                    questions: list[dict[str, Any]] | None = None) -> Session:
        """Record the deferred tool calls a paused Pilot run is waiting on."""
        with self._approval_lock:
            session = self._read(session_id)
            pending = [{**item, "status": "requested"} for item in approvals]
            asked = list(questions or [])
            waiting = bool(pending or asked)
            updated = session.model_copy(update={
                "approvals": pending,
                "approval": pending[0] if pending else None,
                "questions": asked,
                "status": "waiting_user" if waiting else "active",
                "waiting_reason": (asked[0]["question"] if asked else
                                   "Pilot 请求用户确认" if pending else ""),
                "updated_at": _now(),
            })
            self._write(updated)
            for item in pending:
                self._append(updated, "session.confirmation.requested",
                             {"action": item.get("action", ""), "key": item.get("tool_call_id", "")})
            return updated

    def pending_question(self, session_id: str) -> dict[str, Any] | None:
        """The question a paused run is waiting on, if the pause was for one."""
        questions = self._read(session_id).questions
        return questions[0] if questions else None

    def approval_precondition(self, session_id: str, tool_call_id: str) -> dict[str, Any] | None:
        """What the resource looked like when the user was asked, or None if it was not captured."""
        match = next((item for item in self._read(session_id).approvals
                      if item.get("tool_call_id") == tool_call_id), None)
        return (match or {}).get("precondition")

    def decide_approval(self, session_id: str, tool_call_id: str, approved: bool,
                        action: str = "") -> Session:
        """Record one decision. The framework, not the client, holds the original call arguments."""
        with self._approval_lock:
            session = self._read(session_id)
            match = next((item for item in session.approvals
                          if item.get("tool_call_id") == tool_call_id), None)
            if match is None:
                raise ValueError("no matching confirmation request")
            if action and match.get("action") and action != match["action"]:
                raise ValueError("confirmation action does not match the pending request")
            decisions = [{**item, "status": "approved" if approved else "rejected"}
                         if item.get("tool_call_id") == tool_call_id else item
                         for item in session.approvals]
            waiting = any(item.get("status") == "requested" for item in decisions)
            updated = session.model_copy(update={
                "approvals": decisions,
                "approval": decisions[0] if decisions else None,
                "status": "waiting_user" if waiting else "active",
                "waiting_reason": "Pilot 请求用户确认" if waiting else "",
                "updated_at": _now(),
            })
            self._write(updated)
            self._append(updated, "session.confirmation.granted" if approved
                         else "session.confirmation.rejected", {"key": tool_call_id})
            return updated

    def approval_decisions(self, session_id: str) -> dict[str, bool]:
        """Decisions the user already made, keyed by tool call ID, for the next run to consume."""
        return {item["tool_call_id"]: item["status"] == "approved"
                for item in self._read(session_id).approvals
                if item.get("tool_call_id") and item.get("status") in {"approved", "rejected"}}

    def clear_pending(self, session_id: str) -> Session:
        with self._approval_lock:
            session = self._read(session_id)
            updated = session.model_copy(update={"approvals": [], "approval": None, "questions": [],
                                                 "status": "active", "waiting_reason": "",
                                                 "updated_at": _now()})
            self._write(updated)
            return updated

    def begin_operation(self, session_id: str, action: str, call_id: str) -> tuple[str, Any]:
        """Persist intent before a side effect so a crash cannot replay it silently.

        Keyed by the framework's tool call id, so several confirmed calls in one session each keep
        their own outcome instead of overwriting each other.
        """
        with self._approval_lock:
            session = self._read(session_id)
            recorded = session.operations.get(call_id)
            if recorded is not None:
                if recorded.get("status") == "completed":
                    return "completed", recorded.get("result")
                return "uncertain", None
            # ponytail: every recorded operation is kept; one session performs a handful of effects.
            updated = session.model_copy(update={
                "operations": {**session.operations,
                               call_id: {"action": action, "key": call_id, "status": "pending"}},
                "updated_at": _now(),
            })
            self._write(updated)
            self._append(updated, "session.operation.started", {"action": action, "key": call_id})
            return "started", None

    def finish_operation(self, session_id: str, call_id: str, result: Any) -> None:
        with self._approval_lock:
            session = self._read(session_id)
            operation = session.operations.get(call_id)
            if not operation or operation.get("status") != "pending":
                raise ValueError("no matching pending operation")
            updated = session.model_copy(update={
                "operations": {**session.operations,
                               call_id: {**operation, "status": "completed", "result": result}},
                "updated_at": _now(),
            })
            self._write(updated)
            self._append(updated, "session.operation.completed",
                         {"action": operation["action"], "key": call_id})

    def delete(self, session_id: str) -> None:
        session = self._read(session_id)
        if session.status not in {"archived", "interrupted"}:
            raise ValueError("archive or interrupt a session before deleting it")
        store = self.conversation_store()
        import asyncio
        summaries = asyncio.run(store.listing(query=""))
        summary = next((item for item in summaries if item.id == session.conversation_id), None)
        if summary is not None:
            asyncio.run(store.delete(source=summary))
        import shutil
        shutil.rmtree(self._path(session_id))

    def _append(self, session: Session, kind: str, data: dict[str, Any] | None = None) -> SessionEvent:
        events = self.events(session.id)
        event = SessionEvent(seq=len(events) + 1, at=_now(), kind=kind, data=data or {})
        path = self._path(session.id) / "events.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as handle:
            handle.write(event.model_dump_json() + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        return event

    def conversation_store(self):
        from pydantic_ai_harness.step_persistence.conversations import SqliteConversationStore
        return SqliteConversationStore(database=self.conversation_database)

    @staticmethod
    def _conversation_summary(session: Session):
        from pydantic_ai_harness.step_persistence.conversations import ConversationSummary
        return ConversationSummary(id=session.conversation_id, workspace=session.id)
