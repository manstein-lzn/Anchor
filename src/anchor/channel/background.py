"""Session binding for independently admitted Graph calls.

The ordinary Graph Run remains the job and recovery record. This module only arbitrates
access to an existing conversation and projects its completed reply to the channel.
"""
from __future__ import annotations

import hashlib
import json
import os
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from anchor.simple import graph as graph_module
from anchor.simple import run as runner
from anchor.session import SessionStore


def validate(scheduler: Any, source_workspace: Path, source_run_id: str, spec: dict) -> dict:
    session = scheduler.sessions.get(spec["session"])
    if session.status == "archived" or not session.channel or session.graph != spec["graph"]:
        raise ValueError("call.session must be an active channel conversation bound to the target Graph")
    current = source_workspace / "runs" / source_run_id
    visited = set()
    while True:
        if current in visited:
            raise ValueError("cyclic call ancestry")
        visited.add(current)
        source = runner.RunState.load(current)
        if source.trigger.get("session"):
            if source.trigger["session"] != session.id:
                raise ValueError("a conversation cannot call another user's session")
            raise ValueError("a conversation cannot enqueue work into itself")
        if source.trigger.get("source") != "graph_call":
            break
        parent = scheduler.workspace(source.trigger["graph"])
        if parent is None:
            raise ValueError("Graph call ancestor is missing")
        current = parent / "runs" / source.trigger["run"]
    graph = graph_module.load(scheduler.workspace(session.graph) / "graph.json")
    if session.reply_node not in graph.nodes:
        raise ValueError("the session reply node no longer exists")
    if session.channel.get("source") != "wecom":
        raise ValueError("unsupported session channel")
    allowed = {item.strip() for item in (os.environ.get("ANCHOR_WECOM_SEND_USERS") or
               os.environ.get("ANCHOR_WECOM_USERS", "")).split(",") if item.strip()}
    if session.channel.get("sender_id") not in allowed and "*" not in allowed:
        raise ValueError("session recipient is not allowed")
    return {"session": session.id, "reply_node": session.reply_node,
            "conversation_id": session.conversation_id, "channel": session.channel}


@dataclass
class _Lease:
    run_id: str = ""
    interrupted: threading.Event = field(default_factory=threading.Event)
    released: threading.Event = field(default_factory=threading.Event)

    def interrupt(self) -> None:
        self.interrupted.set()


def _settle_admission(run_dir: Path) -> None:
    path = run_dir / "admission.json"
    record = json.loads(path.read_text())
    record["session_pending"] = False
    SessionStore._atomic(path, json.dumps(record, ensure_ascii=False, indent=2))


def execute(scheduler: Any, record: dict, workspace: Path, identifier: str,
            run_options: dict) -> runner.RunState:
    """Wait for a conversation slot; yield to new user messages without dropping this Run."""
    context = record["session_context"]
    session_id = context["session"]
    run_dir = workspace / "runs" / identifier
    original_stop = run_options.get("stop_request", lambda: None)
    while True:
        lease = _Lease(run_id=identifier)
        with scheduler.lock:
            session = scheduler.sessions.get(session_id)
            if session.status == "archived":
                raise ValueError("the target session is archived")
            if original_stop() == "stopped":
                state = runner.RunState.load(run_dir)
                state.status, state.reason = "stopped", "asked"
                state.save(run_dir)
                _settle_admission(run_dir)
                return state
            tail = scheduler.channel_tail.get(session_id)
            active = scheduler.session_background.get(session_id)
            if not tail and active is None:
                scheduler.session_background[session_id] = lease
                acquired = True
            else:
                acquired = False
                predecessor = tail[1] if tail else active.released
        if not acquired:
            predecessor.wait(timeout=0.1)
            continue
        try:
            session = scheduler.sessions.get(session_id)
            if session.graph != record["graph"] or session.channel != context["channel"]:
                raise ValueError("the bound channel conversation changed after admission")
            previous = tuple(workspace / "runs" / name for name in reversed(session.run_ids)
                             if name != identifier and (workspace / "runs" / name / "run.json").is_file())
            if any(runner.RunState.load(path).trigger.get("session") != session_id for path in previous):
                raise ValueError("previous Run belongs to another conversation")
            scheduler.sessions.attach_run(session_id, identifier)
            scheduler.sessions.append(session_id, "graph.call.started", {"run": identifier,
                                      "source_run": record["trigger"]["run"]})
            options = dict(run_options)
            options.update(conversation_id=session.conversation_id, previous_runs=previous,
                           preserve_interrupted=False)
            options["stop_request"] = lambda current=lease: ("stopped" if current.interrupted.is_set() else original_stop())
            from anchor.channel.tools import factory
            options["toolset_factory"] = factory(
                scheduler, workspace, identifier,
                cancelled=lambda stop=options["stop_request"]: stop() == "stopped")
            # Inject trusted identity, never values supplied through an input mapping.
            state = runner.RunState.load(run_dir)
            if not state.cursor and not state.nodes:
                state.input = {**state.input, "channel": session.channel, "session": session_id}
                state.save(run_dir)
            if state.status != "finished":
                options["resume"] = run_dir
                state = runner.run(workspace, **options)
            if lease.interrupted.is_set() and original_stop() != "stopped":
                # Native cursor and step persistence resume the same job after the user turn.
                continue
            if state.status == "finished":
                _deliver(scheduler, session, record, state, original_stop)
            _settle_admission(run_dir)
            scheduler.sessions.append(session_id, "graph.call.completed", {"run": identifier,
                                      "status": state.status})
            return state
        except Exception as exc:
            state = runner.RunState.load(run_dir)
            state.status = "failed"
            state.error = f"{type(exc).__name__}: {exc}"
            state.save(run_dir)
            _settle_admission(run_dir)
            raise
        finally:
            with scheduler.lock:
                if scheduler.session_background.get(session_id) is lease:
                    scheduler.session_background.pop(session_id)
                lease.released.set()


def _deliver(scheduler: Any, session: Any, record: dict, state: runner.RunState, stopped: Any) -> None:
    reply = state.result(session.reply_node)
    if reply is None or not reply.submitted or not reply.submission.strip():
        raise ValueError("the assistant reply node did not produce a reply")
    if stopped() == "stopped":
        raise ValueError("the background call was stopped before delivery")
    allowed = {item.strip() for item in (os.environ.get("ANCHOR_WECOM_SEND_USERS") or
               os.environ.get("ANCHOR_WECOM_USERS", "")).split(",") if item.strip()}
    userid = session.channel["sender_id"]
    if userid not in allowed and "*" not in allowed:
        raise ValueError("session recipient is no longer allowed")
    supervisor = scheduler.channel_supervisor
    if supervisor is None:
        raise RuntimeError("WeCom gateway is unavailable; no message was sent")
    key = hashlib.sha256(f"graph-call-reply:{record['run']}".encode()).hexdigest()
    # The existing gateway ledger owns send claims and ambiguous ACKs, across service restarts.
    receipt = supervisor.send("wecom", {"operation": "send", "request_id": key,
                                       "userid": userid, "content": reply.submission})
    if not receipt.get("accepted"):
        raise RuntimeError("platform did not confirm the assistant reply")
    scheduler.sessions.append(session.id, "graph.call.delivered", {"run": record["run"],
                              "request_id": key, "accepted": True})
