"""Channel projections and coordination over the public Rust Run ports."""

from __future__ import annotations

import json
from http.client import HTTPException
from pathlib import Path
import time
from typing import Any

from anchor.runtime_http import RuntimeHTTPError

POLL_INTERVAL = 0.05
SETTLED = {"completed", "stopped", "failed", "aborted"}


def _json(body: str, status: int) -> dict:
    try:
        value = json.loads(body)
    except (TypeError, ValueError) as exc:
        raise RuntimeHTTPError("Rust channel response is not valid JSON", 502) from exc
    if not isinstance(value, dict):
        raise RuntimeHTTPError("Rust channel response must be an object", 502)
    if status >= 400:
        raise RuntimeHTTPError(value.get("error") or "Rust channel request failed", status)
    return value


def graph_plugins(scheduler: Any) -> dict[str, list[str]]:
    body, status = scheduler.graph(scheduler.wecom_graph)
    value = _json(body, status)
    plugins = value.get("node_plugins")
    if (not isinstance(plugins, dict) or
            any(not isinstance(node, str) or not isinstance(ids, list) or
                any(not isinstance(identifier, str) for identifier in ids) for node, ids in plugins.items())):
        raise RuntimeHTTPError("Rust Graph has no valid compiled Plugin projection", 502)
    if scheduler.wecom_reply_node not in plugins:
        raise RuntimeHTTPError("ANCHOR_WECOM_REPLY_NODE must name a node in the Graph", 503)
    for identifiers in plugins.values():
        scheduler.library.attach(identifiers)
    return plugins


def _identifier(turn: dict) -> str:
    return "channel-" + turn["id"]


def _owned(snapshot: dict, session: Any, *, allow_legacy: bool = False) -> None:
    trigger = snapshot.get("state", {}).get("trigger", {})
    legacy = snapshot.get("backend") == "legacy"
    background = snapshot.get("session_call")
    bound_call = (trigger.get("source") == "graph_call" and isinstance(background, dict) and
                  background.get("context", {}).get("session") == session.id and
                  background.get("context", {}).get("channel") == session.channel)
    if (snapshot.get("graph") != session.graph or (trigger.get("source") != "channel" and not bound_call) or
            trigger.get("session") != session.id or
            (not (allow_legacy and legacy) and trigger.get("reply_node") != session.reply_node)):
        raise RuntimeHTTPError("Run does not belong to this channel conversation", 409)
    if legacy and not allow_legacy:
        raise RuntimeHTTPError("legacy channel Runs are read-only; Rust cannot continue their history", 409)


def _current(scheduler: Any, turn: dict) -> dict:
    return scheduler.turns.get(turn["session"], turn["id"])


def _cancelled(scheduler: Any, turn: dict) -> bool:
    return _current(scheduler, turn)["status"] == "stopped"


def _cancelled_result(scheduler: Any, turn: dict) -> tuple[str, int]:
    current = _current(scheduler, turn)
    if current["error"] == "superseded by a newer message":
        return json.dumps({"text": "", "superseded": True, "session": turn["session"],
                           "run": _identifier(turn)}), 200
    return json.dumps({"stopped": True, "error": current["error"] or "channel turn was stopped",
                       "session": turn["session"], "run": _identifier(turn)}), 409


def _previous(scheduler: Any, session: Any, turn: dict) -> str | None:
    identifier = _identifier(turn)
    current = scheduler.turns.list(session.id)
    offset = next(index for index, item in enumerate(current) if item["id"] == turn["id"])
    earlier = current[offset + 1:]
    associated = {}
    for run in session.run_ids:
        if run != identifier:
            snapshot = scheduler.run("", run)
            if snapshot is not None:
                _owned(snapshot, session)
                associated[run] = snapshot
    # Rust admission can survive a host crash before Session.attach_run. Include
    # bound public Runs so a background turn cannot disappear from the lineage.
    for item in scheduler.runs():
        if (item.get("backend") == "rust" and item.get("trigger", {}).get("session") == session.id
                and item["run"] != identifier and item["run"] not in associated):
            snapshot = scheduler.run("", item["run"])
            if snapshot is not None:
                _owned(snapshot, session)
                associated[item["run"]] = snapshot
    # The background call may be admitted between foreground turns. Its public
    # previous_run chain, rather than the Turn list order, identifies the head.
    if any(item.get("session_call") for item in associated.values()):
        referenced = {item["state"]["trigger"].get("previous_run") for item in associated.values()}
        heads = [run for run in associated if run not in referenced]
        if len(heads) != 1:
            raise RuntimeHTTPError("conversation lineage has no unique head", 409)
        return heads[0]
    # A host can exit after Rust admission but before the Session association is written.
    candidates = list(dict.fromkeys([*(_identifier(item) for item in earlier), *reversed(session.run_ids)]))
    for run in candidates:
        if run == identifier:
            continue
        snapshot = associated.get(run) or scheduler.run("", run)
        if snapshot is not None:
            _owned(snapshot, session)
            return run
    return None


def _matches_input(requested: Any, actual: Any) -> bool:
    if isinstance(requested, dict):
        return isinstance(actual, dict) and all(key in actual and _matches_input(value, actual[key])
                                               for key, value in requested.items())
    return requested == actual


def _stop_and_wait(scheduler: Any, identifier: str, session: Any) -> None:
    stop_sent = False
    while True:
        snapshot = scheduler.run("", identifier)
        if snapshot is None:
            raise RuntimeHTTPError("channel Run disappeared before handoff", 409)
        _owned(snapshot, session)
        if not snapshot.get("active") and snapshot["state"].get("status") in SETTLED:
            return
        if not stop_sent:
            body, status = scheduler.control_run(identifier, "stop")
            if status not in {200, 202}:
                # The worker can settle between observation and the stop request.
                after = scheduler.run("", identifier)
                if after is not None:
                    _owned(after, session)
                    if not after.get("active") and after["state"].get("status") in SETTLED:
                        return
                _json(body, status)
            stop_sent = True
        time.sleep(POLL_INTERVAL)


def _admit(scheduler: Any, session: Any, turn: dict, run_input: dict,
           previous: str | None, attachment_input: dict | None = None) -> None:
    from anchor.channel.attachments import attachment_payload, snapshot_manifest

    identifier = _identifier(turn)
    snapshot = scheduler.run("", identifier)
    if snapshot is None:
        try:
            options = ({"attachments": attachment_payload(scheduler, attachment_input)}
                       if attachment_input is not None else {})
            body, status = scheduler.conversation_run(
                session.graph, identifier, session.id, session.reply_node, run_input, previous, **options)
            accepted = _json(body, status)
            if status != 202 or accepted.get("run") != identifier or accepted.get("graph") != session.graph:
                raise RuntimeHTTPError("Rust channel admission returned a different Run identity", 502)
        except RuntimeHTTPError as exc:
            if exc.status < 500:
                raise
            # An interrupted response may follow durable admission. Query once; never resubmit.
            snapshot = scheduler.run("", identifier)
            if snapshot is None:
                raise exc
    snapshot = snapshot or scheduler.run("", identifier)
    if snapshot is None:
        raise RuntimeHTTPError("Rust accepted a channel Run without a readable record", 502)
    _owned(snapshot, session)
    if not _matches_input(run_input, snapshot["state"].get("input")):
        raise RuntimeHTTPError("channel Run already exists with different input", 409)
    if snapshot["state"]["trigger"].get("previous_run") != previous:
        raise RuntimeHTTPError("channel Run already exists with a different predecessor", 409)
    if attachment_input is not None:
        expected = snapshot_manifest(scheduler, attachment_input)
        actual = snapshot.get("attachments")
        if (not isinstance(actual, list) or any(not isinstance(item, dict) or
                not isinstance(item.get("name"), str) for item in actual) or
                actual != expected):
            raise RuntimeHTTPError("channel Run already exists with different attachment bytes", 409)


def _existing_attachment_input(scheduler: Any, session: Any, turn: dict,
                               channel_input: dict, snapshot: dict) -> dict:
    from anchor.channel.attachments import source_descriptors

    _owned(snapshot, session)
    run_input = snapshot["state"].get("input")
    expected = {"message": turn["prompt"], "session": session.id, "channel": session.channel}
    if not _matches_input(expected, run_input):
        raise RuntimeHTTPError("channel Run already exists with different input", 409)
    if "attachment_snapshot" in channel_input:
        expected["channel"] = {**session.channel, "attachments": channel_input["attachments"]}
        expected["attachment_content"] = channel_input["attachment_content"]
        if not _matches_input(expected, run_input):
            raise RuntimeHTTPError("channel Run already exists with different attachment input", 409)
        return run_input
    # Older Turns stored download paths. An admitted Rust Run's public frozen
    # metadata remains readable even after those source files have disappeared.
    sources = source_descriptors(channel_input["attachments"])
    actual = run_input.get("channel", {}).get("attachments")
    manifest = snapshot.get("attachments")
    if (not isinstance(actual, list) or len(actual) != len(sources) or not isinstance(manifest, list) or
            any(not isinstance(item, dict) or not isinstance(item.get("name"), str) for item in manifest)):
        raise RuntimeHTTPError("channel Run has no matching public attachment metadata", 409)
    if [item["name"] for item in manifest] != [Path(source["path"]).name for source in sources]:
        raise RuntimeHTTPError("channel Run has no matching public attachment metadata", 409)
    for source, virtual, file in zip(sources, actual, manifest, strict=True):
        name = Path(source["path"]).name
        if (not isinstance(virtual, dict) or virtual.get("path") != "/in/channel/" + name or
                virtual.get("size") != file.get("size") or any(
                    key in source and virtual.get(key) != source[key] for key in ("kind", "name"))):
            raise RuntimeHTTPError("channel Run already exists with different attachment input", 409)
    return run_input


def result(scheduler: Any, turn: dict) -> tuple[str, int]:
    if _cancelled(scheduler, turn):
        return _cancelled_result(scheduler, turn)
    session = scheduler.sessions.get(turn["session"])
    identifier = _identifier(turn)
    try:
        snapshot = scheduler.run("", identifier)
        if snapshot is None:
            return json.dumps({"error": turn.get("error") or "no such channel Run", "session": session.id,
                               "run": identifier}), 404
        _owned(snapshot, session, allow_legacy=True)
    except RuntimeHTTPError as exc:
        return exc.response()
    state = snapshot["state"]
    terminal = "finished" if snapshot.get("backend") == "legacy" else "completed"
    reply = state.get("nodes", {}).get(session.reply_node)
    if (state.get("status") == terminal and not snapshot.get("active") and isinstance(reply, dict) and
            reply.get("submitted") and isinstance(reply.get("submission"), str)):
        value = {"text": reply["submission"], "session": session.id, "graph": session.graph,
                 "run": identifier}
        response = json.dumps(value, ensure_ascii=False), 200
        # Rich channel replies are stored behind a dedicated Rust endpoint so
        # large base64 images do not inflate every Run detail response.
        if snapshot.get("backend") != "legacy" and snapshot.get("channel_reply") is True:
            if _cancelled(scheduler, turn):
                return _cancelled_result(scheduler, turn)
            try:
                with scheduler.runtime.download(f"/runs/{identifier}/channel-reply") as stream:
                    payload = stream.read(16 * 1024 * 1024 + 1)
                if len(payload) > 16 * 1024 * 1024:
                    raise RuntimeHTTPError("Rust channel reply exceeds its size limit", 502)
                saved = json.loads(payload)
                if not isinstance(saved, list) or not saved:
                    raise RuntimeHTTPError("Rust channel reply must be a nonempty list", 502)
                value["msg_item"] = saved
                response = json.dumps(value, ensure_ascii=False), 200
            except RuntimeHTTPError as exc:
                response = exc.response()
            except (ValueError, UnicodeDecodeError, OSError, HTTPException):
                response = RuntimeHTTPError("Rust channel reply could not be read as valid JSON", 502).response()
    else:
        response = json.dumps({"error": state.get("error") or turn.get("error") or state.get("status"),
                               "session": session.id, "run": identifier}, ensure_ascii=False), 502
    # create_turn supersedes an older turn while holding this same lock. Recheck
    # after the Rust projection so a concurrent replacement cannot receive the
    # old reply as a successful result.
    with scheduler.lock:
        if _cancelled(scheduler, turn):
            return _cancelled_result(scheduler, turn)
        return response


def history(scheduler: Any, session_id: str) -> list[dict[str, str]]:
    messages = []
    for turn in reversed(scheduler.turns.list(session_id)):
        if turn["prompt"]:
            messages.append({"role": "user", "text": turn["prompt"]})
        body, status = result(scheduler, turn)
        value = json.loads(body)
        if status == 200 and not value.get("superseded"):
            messages.append({"role": "assistant", "text": value["text"]})
        elif status >= 500 and value.get("backend") == "rust":
            raise RuntimeHTTPError(value["error"], status)
    return messages


def _wait(scheduler: Any, session: Any, turn: dict) -> tuple[str, int]:
    identifier = _identifier(turn)
    while True:
        if _cancelled(scheduler, turn):
            _stop_and_wait(scheduler, identifier, session)
            return _cancelled_result(scheduler, turn)
        snapshot = scheduler.run("", identifier)
        if snapshot is None:
            raise RuntimeHTTPError("admitted channel Run is missing", 409)
        _owned(snapshot, session)
        if not snapshot.get("active") and snapshot["state"].get("status") != "running":
            return result(scheduler, turn)
        time.sleep(POLL_INTERVAL)


def _turn_input(scheduler: Any, session: Any, turn: dict, channel_input: dict) -> tuple[
        dict, str | None, dict | None, bool]:
    from anchor.channel.assistant import _interrupted_messages
    from anchor.channel.attachments import prepare_channel_input

    existing = scheduler.run("", _identifier(turn)) if channel_input.get("attachments") else None
    if existing is not None:
        run_input = _existing_attachment_input(scheduler, session, turn, channel_input, existing)
        return (run_input, existing["state"]["trigger"].get("previous_run"),
                channel_input if "attachment_snapshot" in channel_input else None, True)
    channel_input = prepare_channel_input(scheduler, channel_input)
    run_input = {"message": turn["prompt"],
                 "channel": {**session.channel, "attachments": channel_input.get("attachments", [])},
                 "session": session.id,
                 "interrupted_messages": list(reversed(_interrupted_messages(scheduler, session.id, turn)))}
    if channel_input.get("attachment_content"):
        run_input["attachment_content"] = channel_input["attachment_content"]
    return (run_input, _previous(scheduler, session, turn),
            channel_input if channel_input.get("attachments") else None, False)


def execute(scheduler: Any, turn: dict) -> tuple[str, int]:
    session = scheduler.sessions.get(turn["session"])
    try:
        if _cancelled(scheduler, turn):
            return _cancelled_result(scheduler, turn)
        channel_input = json.loads(turn.get("channel_input") or "{}")
        if not isinstance(channel_input, dict):
            raise RuntimeHTTPError("stored channel input is invalid", 502)
        identifier = _identifier(turn)
        run_input, previous, attachment_input, reused = _turn_input(scheduler, session, turn, channel_input)
        if not reused and previous is not None:
            _stop_and_wait(scheduler, previous, session)
        if _cancelled(scheduler, turn):
            return _cancelled_result(scheduler, turn)
        _admit(scheduler, session, turn, run_input, previous, attachment_input)
        scheduler.sessions.name_from_prompt(session.id, turn["prompt"])
        scheduler.sessions.attach_run(session.id, identifier)
        if not _cancelled(scheduler, turn):
            scheduler.sessions.set_status(session.id, "active")
        scheduler.sessions.append(session.id, "graph.turn.started", {"run": identifier, "graph": session.graph})
        body, status = _wait(scheduler, session, turn)
        if _cancelled(scheduler, turn):
            if json.loads(body).get("superseded"):
                scheduler.sessions.append(session.id, "graph.turn.superseded", {"run": identifier})
            return body, status
        if status != 200:
            scheduler.sessions.set_status(session.id, "interrupted", reason=json.loads(body).get("error") or "Run failed")
            return body, status
        reply = json.loads(body)["text"]
        with scheduler.lock:
            if _cancelled(scheduler, turn):
                return _cancelled_result(scheduler, turn)
            scheduler.turns.append(turn["id"], {"type": "text-delta", "delta": reply})
            scheduler.sessions.append(session.id, "graph.turn.completed", {"run": identifier})
            return json.dumps({"message": reply}, ensure_ascii=False), 200
    except (RuntimeHTTPError, ValueError, OSError) as exc:
        if not _cancelled(scheduler, turn):
            scheduler.sessions.set_status(session.id, "interrupted", reason=
                                          "channel attachment snapshot is unavailable" if isinstance(exc, OSError)
                                          else str(exc))
        error = exc if isinstance(exc, RuntimeHTTPError) else RuntimeHTTPError("stored channel input is invalid", 502)
        return error.response()
