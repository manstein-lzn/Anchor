"""Bind trusted channel messages to ordinary Graph executions, never to Pilot."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import time
from typing import TYPE_CHECKING, Any

from anchor.simple import graph as graph_module
from anchor.simple import run as runner

if TYPE_CHECKING:
    from anchor.serve import Scheduler


def run_id(turn: dict) -> str:
    return f"channel-{turn['id']}"


def history(scheduler: Scheduler, session_id: str) -> list[dict[str, str]]:
    messages = []
    for turn in reversed(scheduler.turns.list(session_id)):
        if turn["prompt"]:
            messages.append({"role": "user", "text": turn["prompt"]})
        body, status = result(scheduler, turn)
        if status == 200:
            messages.append({"role": "assistant", "text": json.loads(body)["text"]})
    return messages


def receive(scheduler: Scheduler, event: dict[str, Any]) -> tuple[str, int]:  # noqa: C901
    required = ("source", "event_id", "sender_id", "conversation_id")
    if any(not isinstance(event.get(name), str) or not event[name].strip() for name in required):
        return json.dumps({"error": "event is missing a required string"}), 400
    message_type = event.get("message_type", "text")
    if event["source"] != "wecom" or message_type not in {"text", "image", "file", "mixed"}:
        return json.dumps({"error": "unsupported WeCom message type"}), 400
    text = event.get("text", "")
    if not isinstance(text, str):
        return json.dumps({"error": "event text must be a string"}), 400
    attachments = event.get("attachments", [])
    if not isinstance(attachments, list) or any(not isinstance(item, dict) for item in attachments):
        return json.dumps({"error": "attachments must be a list of objects"}), 400
    if message_type != "text" and not attachments:
        return json.dumps({"error": "media message has no downloaded attachment"}), 400
    if not text.strip() and not attachments:
        return json.dumps({"error": "event must contain text or an attachment"}), 400
    metadata = event.get("metadata", {})
    if not isinstance(metadata, dict) or metadata.get("chat_type", "single") != "single":
        return json.dumps({"error": "only private conversations are supported"}), 400
    if len(text) > 100_000:
        return json.dumps({"error": "text must contain at most 100000 characters"}), 400
    try:
        _attachment_resources(scheduler, attachments)
    except ValueError as exc:
        return json.dumps({"error": str(exc)}, ensure_ascii=False), 400
    if event["sender_id"] not in scheduler.wecom_users and "*" not in scheduler.wecom_users:
        return json.dumps({"error": "WeCom user is not allowed"}), 403
    workspace = scheduler.workspace(scheduler.wecom_graph)
    if workspace is None:
        return json.dumps({"error": "configure ANCHOR_WECOM_GRAPH with an installed Graph"}), 503
    try:
        graph = graph_module.load(workspace / "graph.json")
        if scheduler.wecom_reply_node not in graph.nodes:
            raise ValueError("ANCHOR_WECOM_REPLY_NODE must name a node in the Graph")
        for node in graph.nodes.values():
            scheduler.library.attach(node.plugins)
    except (ValueError, OSError) as exc:
        return json.dumps({"error": str(exc)}), 503
    channel = {name: event[name] for name in ("source", "sender_id", "conversation_id")}
    identity = json.dumps([scheduler.wecom_graph, scheduler.wecom_reply_node, *channel.values()])
    session_id = "channel-graph-" + hashlib.sha256(identity.encode()).hexdigest()[:40]
    with scheduler.lock:
        try:
            scheduler.sessions.get(session_id)
        except KeyError:
            scheduler.sessions.create(session_id, graph=scheduler.wecom_graph,
                                      reply_node=scheduler.wecom_reply_node, channel=channel)
            scheduler.sessions.append(session_id, "channel.bound", channel)
    request_id = "channel-" + hashlib.sha256(json.dumps([event["source"], event["event_id"]]).encode()).hexdigest()
    body, status = scheduler.create_turn(session_id, request_id, text,
                                         channel_input={"attachments": attachments})
    if status != 202:
        return body, status
    turn = json.loads(body)["turn"]
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        current = scheduler.turns.get(session_id, turn["id"])
        if current["status"] != "running":
            return result(scheduler, current)
        time.sleep(0.05)
    return json.dumps({"error": "channel turn is still running", "session": session_id}), 504


def result(scheduler: Scheduler, turn: dict) -> tuple[str, int]:
    session = scheduler.sessions.get(turn["session"])
    current = scheduler.turns.get(session.id, turn["id"])
    if current["status"] == "stopped" and current["error"] == "superseded by a newer message":
        return json.dumps({"text": "", "superseded": True, "session": session.id,
                           "run": run_id(turn)}), 200
    workspace = scheduler.workspace(session.graph)
    path = workspace / "runs" / run_id(turn) / "run.json" if workspace else None
    # Run facts also cover a crash after Graph completion but before Turn settlement.
    state = runner.RunState.load(path.parent) if path and path.is_file() else None
    if state is not None and state.status == "finished":
        reply = state.result(session.reply_node)
        if reply is not None and reply.submitted:
            return json.dumps({"text": reply.submission, "session": session.id,
                               "graph": session.graph, "run": run_id(turn)}, ensure_ascii=False), 200
    return json.dumps({"error": turn.get("error") or (state.error if state else "") or turn["status"],
                       "session": session.id, "run": run_id(turn)}, ensure_ascii=False), 502


def execute(scheduler: Scheduler, turn: dict) -> tuple[str, int]:
    """One admitted turn, through the existing runner and its node/Plugin/sandbox boundary."""
    session = scheduler.sessions.get(turn["session"])
    workspace = scheduler.workspace(session.graph)
    if workspace is None:
        return json.dumps({"error": "the assistant Graph no longer exists"}), 404
    identifier = run_id(turn)
    previous = tuple(workspace / "runs" / name for name in reversed(session.run_ids)
                     if name != identifier and (workspace / "runs" / name / "run.json").is_file())
    if any(runner.RunState.load(path).trigger.get("session") != session.id for path in previous):
        return json.dumps({"error": "previous Run does not belong to this conversation"}), 409
    scheduler.sessions.name_from_prompt(session.id, turn["prompt"])
    scheduler.sessions.attach_run(session.id, identifier)
    scheduler.sessions.set_status(session.id, "active")
    scheduler.sessions.append(session.id, "graph.turn.started", {"run": identifier, "graph": session.graph})
    channel_input = {}
    if turn.get("channel_input"):
        try:
            channel_input = json.loads(turn["channel_input"])
        except (TypeError, ValueError):
            return json.dumps({"error": "stored channel input is invalid"}), 502
    try:
        resources, virtual_attachments = _attachment_resources(scheduler, channel_input.get("attachments", []))
    except ValueError as exc:
        scheduler.sessions.set_status(session.id, "interrupted", reason=str(exc))
        return json.dumps({"error": str(exc)}, ensure_ascii=False), 400
    channel = {**session.channel, "attachments": virtual_attachments}
    interrupted = []
    for prior in scheduler.turns.list(session.id):
        if prior["id"] == turn["id"] or prior["created_at"] > turn["created_at"]:
            continue
        if prior["status"] == "completed":
            break
        if prior["prompt"]:
            interrupted.append(prior["prompt"])
    try:
        state = runner.run(
            workspace, config_path=scheduler.config, run_id=identifier,
            run_input={"message": turn["prompt"], "channel": channel, "session": session.id,
                       "interrupted_messages": list(reversed(interrupted))},
            trigger={"source": "channel", "platform": session.channel.get("source"), "session": session.id},
            conversation_id=session.conversation_id, previous_runs=previous,
            library_root=scheduler.library.root,
            resources=resources,
            stop_request=lambda: scheduler.control.get(identifier))
        body, status = result(scheduler, {**turn, "status": state.status, "error": state.error})
        if status != 200:
            scheduler.sessions.set_status(session.id, "interrupted", reason=state.error or state.status)
            if state.status == "stopped":
                return json.dumps({"stopped": True, "error": state.reason}), 409
            return body, status
        if json.loads(body).get("superseded"):
            scheduler.sessions.append(session.id, "graph.turn.superseded", {"run": identifier})
            return body, status
        reply = json.loads(body)["text"]
        scheduler.turns.append(turn["id"], {"type": "text-delta", "delta": reply})
        scheduler.sessions.append(session.id, "graph.turn.completed", {"run": identifier})
        return json.dumps({"message": reply}, ensure_ascii=False), 200
    except Exception as exc:  # noqa: BLE001 - Graph records its failure; never replay an accepted turn
        scheduler.sessions.set_status(session.id, "interrupted", reason=type(exc).__name__)
        return json.dumps({"error": f"{type(exc).__name__}: {exc}"}, ensure_ascii=False), 502


def _attachment_resources(scheduler: Scheduler, attachments: object) -> tuple[
        tuple[tuple[str, str], ...], list[dict[str, Any]]]:
    """Validate downloaded channel files and expose their containing directory read-only."""
    if not attachments:
        return (), []
    if not isinstance(attachments, list) or len(attachments) > 16:
        raise ValueError("too many channel attachments")
    root = (scheduler.root / "state" / "channels" / "wecom").resolve()
    files: list[Path] = []
    normalized: list[dict[str, Any]] = []
    for item in attachments:
        if not isinstance(item, dict) or not isinstance(item.get("path"), str):
            raise ValueError("channel attachment path is invalid")
        path = Path(item["path"]).resolve()
        if not path.is_file() or not path.is_relative_to(root):
            raise ValueError("channel attachment is outside the managed state directory")
        if path.is_symlink() or path.stat().st_size > 20 * 1024 * 1024:
            raise ValueError("channel attachment is too large or is a symlink")
        files.append(path)
        name = str(item.get("name") or path.name)
        if Path(name).name != name:
            raise ValueError("channel attachment name is invalid")
        normalized.append({key: item[key] for key in ("kind", "name", "mime_type", "size") if key in item})
    parent = files[0].parent
    if any(path.parent != parent for path in files):
        raise ValueError("channel attachments must belong to one event directory")
    for item, path in zip(normalized, files):
        item["path"] = "/in/channel/" + path.name
    return ((str(parent), "/in/channel"),), normalized
