"""A deployed Anchor: it listens, it routes a trigger to a graph, and it keeps its runs.

    anchor-serve --root ~/.anchor --config .local/runtime.json

    POST /trigger          {"graph": "academic", "objective": "…"}  -> run id, or 409
    GET  /graphs           the workspaces under the root, and whether one is running
    GET  /runs             every run, newest first
    GET  /runs/<id>        one run: its status, and the tail of each node's trace

One graph runs one thing at a time. A second trigger for the same graph is refused rather than
queued, because the answer to "I want this run too" should be visible to whoever asked, not a silent
position in a line. Different graphs run at the same time.

On startup every run recorded as running is resumed. There is no other recovery: a run's state is
`run.json` plus the directories and conversations beside it, so continuing one is reading it back and
stepping again.

Graphs and runs remain file-backed; Pilot uses Harness messages and SQLite delivery records.
"""

from __future__ import annotations

import json
import hmac
import hashlib
import ipaddress
import os
import shutil
import subprocess
import threading
import time
import traceback
from datetime import datetime, timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import parse_qs, quote, unquote, urlparse
from uuid import uuid4

from anchor.simple import graph as graph_module
from anchor.simple import run as runner
from anchor.library import Library
from anchor.graph_calls import GraphCalls
from anchor.session import SessionStore
from anchor.pilot_turns import TurnStore
from anchor.scheduling import next_after, occurrences, validate as validate_schedule


def _call_args(call: Any) -> Any:
    """Streamed tool calls arrive with JSON text; a proposal a person reads should be parsed."""
    args = call.args
    if isinstance(args, str):
        try:
            args = json.loads(args)
        except ValueError:
            return args
    return args


def _call_target(call: Any) -> str:
    """The name a person recognises in an approval prompt: the Graph or Run the call touches."""
    args = _call_args(call)
    if not isinstance(args, dict):
        return ""
    for name in ("graph", "run", "name"):
        if isinstance(args.get(name), str):
            return args[name]
    return ""


def _response_prompt(value: Any) -> str | None:
    if isinstance(value, str):
        return value if value.strip() else None
    if not isinstance(value, list) or not value:
        return None
    messages = []
    for item in value:
        if not isinstance(item, dict) or item.get("role") != "user":
            return None
        content = item.get("content")
        if isinstance(content, str):
            messages.append(content)
        elif isinstance(content, list):
            if any(not isinstance(part, dict) or part.get("type") != "input_text" or
                   not isinstance(part.get("text"), str) for part in content):
                return None
            messages.append("\n".join(part["text"] for part in content))
        else:
            return None
    text = "\n\n".join(messages)
    return text if text.strip() else None


def _local_time(value: str) -> datetime:
    result = datetime.fromisoformat(value)
    return result.astimezone().replace(tzinfo=None) if result.tzinfo else result


#: How much of a node's conversation an observer is given. Enough to see what it is doing, not so
#: much that the endpoint becomes a way to download a run's whole history one request at a time.
TAIL_LINES = 40

#: Where `vite build` leaves the interface. Its absence is not an error.
BUILT = Path(__file__).resolve().parents[2] / "apps" / "web" / "dist"


class Scheduler:
    """Which graphs are running, and the runs that finished."""

    def __init__(self, root: Path, config: Path) -> None:
        self.root = root
        self.config = config
        self.library = Library(root / "library")
        self.sessions = SessionStore(root)
        self.turns = TurnStore(root)
        self.pilot_active: set[str] = set()
        self.pilot_tokens: dict[str, Any] = {}
        self.running: dict[str, str] = {}         # graph -> run id
        self.channel_tail: dict[str, tuple[str, threading.Event]] = {}
        self.session_background: dict[str, Any] = {}
        self.channel_runs: dict[str, str] = {}    # conversation run id -> graph
        self.wecom_graph = os.environ.get("ANCHOR_WECOM_GRAPH", "").strip()
        self.wecom_reply_node = os.environ.get("ANCHOR_WECOM_REPLY_NODE", "assistant").strip()
        self.wecom_users = {item.strip() for item in os.environ.get("ANCHOR_WECOM_USERS", "").split(",")
                            if item.strip()}
        # A pause lands between nodes; a stop also cancels the current model call or command.
        self.control: dict[str, str] = {}
        self.channel_supervisor = None
        self.lock = threading.RLock()
        self.graph_calls = GraphCalls(self)
        self.schedule_path = root / "state" / "schedules.json"
        self.schedule_path.parent.mkdir(parents=True, exist_ok=True)
        self.schedules = (json.loads(self.schedule_path.read_text(encoding="utf-8"))
                          if self.schedule_path.exists() else [])
        self.responses_path = root / "state" / "responses.json"
        self.response_refs = (json.loads(self.responses_path.read_text(encoding="utf-8"))
                              if self.responses_path.exists() else {})
        raw_keys = os.environ.get("ANCHOR_API_KEYS", "").strip()
        if raw_keys:
            try:
                keys = json.loads(raw_keys)
            except json.JSONDecodeError as exc:
                raise ValueError("ANCHOR_API_KEYS must be a JSON array of strings") from exc
            if (not isinstance(keys, list) or not keys or
                    any(not isinstance(key, str) or len(key.encode()) < 32 for key in keys) or
                    len(set(keys)) != len(keys)):
                raise ValueError("ANCHOR_API_KEYS must contain unique secrets of at least 32 bytes")
            self.api_keys = tuple(keys)
        else:
            self.api_keys = ()
        for session_id in self.turns.interrupt_running():
            try:
                self.sessions.set_status(session_id, "interrupted", reason="服务重启，未自动重放上次执行")
            except (KeyError, ValueError):
                pass
        # Anything already due belongs to downtime, so advance past it instead of catching up.
        self._skip_missed_schedules(datetime.now())

    def start_channels(self, callback_url: str) -> None:
        """Start Plugin-declared channel daemons after the HTTP listener is ready."""
        key = os.environ.get("ANCHOR_API_KEY", "").strip()
        if not key or key not in self.api_keys:
            return
        from anchor.channel.supervisor import ChannelSupervisor
        self.channel_supervisor = ChannelSupervisor(
            self.root, self.library, self.workspaces,
            callback_url=callback_url, api_key=key)
        self.channel_supervisor.start()

    def stop_channels(self) -> None:
        if self.channel_supervisor is not None:
            self.channel_supervisor.stop()
            self.channel_supervisor = None

    def save_schedules(self) -> None:
        temp = self.schedule_path.with_suffix(".tmp")
        temp.write_text(json.dumps(self.schedules, ensure_ascii=False, indent=2) + "\n",
                        encoding="utf-8")
        temp.replace(self.schedule_path)

    def _skip_missed_schedules(self, now: datetime) -> None:
        for item in self.schedules:
            due = datetime.fromisoformat(item["next_at"])
            if due <= now:
                if item["rule"]["type"] == "once":
                    item["enabled"] = False
                else:
                    item["next_at"] = next_after(item["rule"], now).isoformat(timespec="seconds")
        self.save_schedules()

    def create_schedule(self, graph: str, rule: dict, run_input: dict | None = None) -> tuple[str, int]:
        if self.workspace(graph) is None:
            return json.dumps({"error": f"no such graph: {graph}"}), 404
        try:
            parsed = validate_schedule(rule, datetime.now())
        except (TypeError, ValueError) as exc:
            return json.dumps({"error": str(exc)}, ensure_ascii=False), 400
        now = datetime.now()
        schedule = {"id": str(uuid4()), "graph": graph, "rule": parsed,
                    "input": run_input or {}, "created_at": now.isoformat(timespec="seconds"),
                    "next_at": next_after(parsed, now).isoformat(timespec="seconds"),
                    "enabled": True}
        self.schedules.append(schedule)
        self.save_schedules()
        return json.dumps({"schedule": schedule}, ensure_ascii=False), 201

    def delete_schedule(self, identifier: str) -> tuple[str, int]:
        old = len(self.schedules)
        self.schedules = [item for item in self.schedules if item["id"] != identifier]
        if len(self.schedules) == old:
            return json.dumps({"error": "no such schedule"}), 404
        self.save_schedules()
        return json.dumps({"schedule": identifier, "deleted": True}), 200

    def response_turn(self, owner: str, body: dict) -> tuple[dict | None, int]:
        allowed = {"input", "model", "stream", "previous_response_id"}
        if set(body) - allowed:
            return {"error": f"unsupported fields: {', '.join(sorted(set(body) - allowed))}"}, 400
        if body.get("model", "anchor-copilot") != "anchor-copilot":
            return {"error": "model must be anchor-copilot"}, 400
        stream = body.get("stream", False)
        if type(stream) is not bool:
            return {"error": "stream must be a boolean"}, 400
        prompt = _response_prompt(body.get("input"))
        if prompt is None:
            return {"error": "input must be text or a list of user text messages"}, 400
        previous = body.get("previous_response_id")
        if previous is not None:
            ref = self.response_refs.get(previous)
            if not isinstance(ref, dict) or ref.get("owner") != owner:
                return {"error": "no such response"}, 404
            session_id = ref["session"]
        else:
            session_id = "responses-" + uuid4().hex
            self.sessions.create(session_id)
        response_id = "resp_" + uuid4().hex
        turn, status = self.create_turn(session_id, response_id, prompt)
        if status != 202:
            return json.loads(turn), status
        turn = json.loads(turn)
        with self.lock:
            self.response_refs[response_id] = {"owner": owner, "session": session_id,
                                               "turn": turn["turn"]["id"]}
            temp = self.responses_path.with_suffix(".tmp")
            temp.write_text(json.dumps(self.response_refs, ensure_ascii=False), encoding="utf-8")
            temp.replace(self.responses_path)
        return {"id": response_id, "session": session_id, "turn": turn["turn"]["id"],
                "stream": stream}, 200

    def response_object(self, response_id: str, status: str, text: str, error: str = "") -> dict:
        response = {"id": response_id, "object": "response", "created_at": int(time.time()),
                    "status": status, "model": "anchor-copilot", "output": [],
                    "output_text": text}
        if text:
            response["output"] = [{"id": "msg_" + response_id.removeprefix("resp_"),
                                   "type": "message", "role": "assistant", "status": status,
                                   "content": [{"type": "output_text", "text": text,
                                                "annotations": []}]}]
        if error:
            response["error"] = {"message": error, "type": "server_error"}
        return response

    def response_result(self, response_id: str) -> tuple[dict, int]:
        ref = self.response_refs[response_id]
        turn = self.turns.get(ref["session"], ref["turn"])
        chunks = []
        cursor = 0
        while True:
            events = self.turns.events(ref["session"], ref["turn"], cursor)
            chunks.extend(event["data"].get("delta", "") for event in events
                          if event["data"].get("type") == "text-delta")
            if len(events) < 256:
                break
            cursor = events[-1]["seq"]
        text = "".join(chunks)
        status = turn["status"]
        if status == "running":
            return self.response_object(response_id, "in_progress", text), 202
        if status == "completed":
            return self.response_object(response_id, "completed", text), 200
        return self.response_object(response_id, "failed", text, turn.get("error") or status), 200

    def tick_schedules(self, now: datetime | None = None) -> None:
        now = now or datetime.now()
        with self.lock:
            for item in self.schedules:
                if not item["enabled"] or datetime.fromisoformat(item["next_at"]) > now:
                    continue
                due = datetime.fromisoformat(item["next_at"])
                item["next_at"] = (next_after(item["rule"], now).isoformat(timespec="seconds")
                                    if item["rule"]["type"] != "once" else item["next_at"])
                if item["rule"]["type"] == "once":
                    item["enabled"] = False
                # Busy due times are skipped without creating a Run or a refusal record.
                if now - due > timedelta(seconds=1) or item["graph"] in self.running:
                    continue
                threading.Thread(target=self.trigger, args=(item["graph"], None, item["input"],
                                 {"source": "schedule", "schedule": item["id"],
                                  "scheduled_at": due.isoformat(timespec="seconds")}),
                                 daemon=True).start()
            self.save_schedules()

    def timeline(self, days_back: int = 30, before: str | None = None) -> dict:
        now = datetime.now()
        future_start = datetime.combine(now.date(), datetime.min.time())
        future_end = future_start + timedelta(days=8)
        end = (datetime.combine(datetime.fromisoformat(before).date(), datetime.min.time())
               if before else future_start + timedelta(days=1))
        start = end - timedelta(days=days_back)
        scheduled = []
        runs = self.runs()
        for item in self.schedules:
            windows = [(start, end)]
            if not before:
                windows.append((future_start, future_end))
            else:
                windows.append((future_start, future_end))
            seen = set()
            for window_start, window_end in windows:
              for due in occurrences(item["rule"], datetime.fromisoformat(item["created_at"]),
                                     window_start, window_end):
                if due in seen:
                    continue
                seen.add(due)
                if due > now:
                    scheduled.append({"schedule": item["id"], "graph": item["graph"],
                                      "scheduled_at": due.isoformat(timespec="seconds"),
                                      "status": "planned"})
                elif due <= now:
                    matched = next((run for run in runs if run.get("trigger", {}).get("schedule") == item["id"]
                                    and run.get("trigger", {}).get("scheduled_at") == due.isoformat(timespec="seconds")), None)
                    if matched:
                        scheduled.append({"schedule": item["id"], "graph": item["graph"],
                                          "scheduled_at": due.isoformat(timespec="seconds"),
                                          "run": matched["run"], "status": matched["status"]})
                    else:
                        running = next((run for run in runs if run["graph"] == item["graph"] and
                                        run.get("started") and run.get("updated") and
                                        _local_time(run["started"]) <= due <= _local_time(run["updated"])), None)
                        scheduled.append({"schedule": item["id"], "graph": item["graph"],
                                          "scheduled_at": due.isoformat(timespec="seconds"),
                                          "status": "missed_busy" if running else "missed_downtime"})
        return {"from": start.isoformat(timespec="seconds"), "to": end.isoformat(timespec="seconds"),
                "runs": runs, "scheduled": scheduled, "schedules": self.schedules}

    def workspaces(self) -> list[Path]:
        base = self.root / "workspaces"
        return sorted(item for item in base.iterdir()
                      if item.is_dir() and (item / "graph.json").is_file()) if base.is_dir() else []

    def workspace(self, name: str) -> Path | None:
        """A graph that exists — that is, a workspace with a definition in it.

        Deliberately stricter than the directory check below: listing graphs should not show a
        directory somebody made and did not fill in.
        """
        return next((item for item in self.workspaces() if item.name == name), None)

    def _directory(self, name: str) -> Path | None:
        """The workspace directory, whether or not it has a definition yet."""
        if not name or "/" in name or name.startswith("."):
            return None
        candidate = self.root / "workspaces" / name
        return candidate if candidate.is_dir() else None

    def trigger(self, graph: str, objective: str | None,
                run_input: dict | None = None,
                trigger: dict | None = None) -> tuple[str, int]:
        """Start a run, or say why not. Returns (body, status)."""
        workspace = self.workspace(graph)
        if workspace is None:
            return json.dumps({"error": f"no such graph: {graph}"}), 404
        try:
            parsed = graph_module.load(workspace / "graph.json")
            for node in parsed.nodes.values():
                self.library.attach(node.plugins)
        except (ValueError, OSError) as exc:
            return json.dumps({"error": str(exc)}, ensure_ascii=False), 400
        with self.lock:
            if self.active_run(graph):
                # Refused, not queued: whoever asked should be able to tell that this run did not
                # start, and one graph doing two things at once is not something it can be asked to
                # keep straight.
                return json.dumps({"error": "this graph is already running",
                                   "running": self.active_run(graph)}), 409
            run_id = _stamp()
            self.running[graph] = run_id
        threading.Thread(target=self._run, args=(workspace, run_id, objective),
                         kwargs={"run_input": run_input, "trigger": trigger},
                         daemon=True).start()
        return json.dumps({"run": run_id, "graph": graph}), 202

    def control_run(self, run_id: str, what: str) -> tuple[str, int]:
        """Ask a run to pause, stop or carry on. Returns (body, status).

        A pause leaves off between nodes. A stop cancels the current node and is terminal.
        """
        if what not in ("pause", "stop", "resume"):
            return json.dumps({"error": f"unknown control: {what}"}), 400
        if run_id in self.channel_runs and what != "stop":
            return json.dumps({"error": "channel Runs accept stop; continue with a new conversation message"}), 409
        graph = self.graph_calls.active.get(run_id) or self.channel_runs.get(run_id) or next(
            (name for name, current in self.running.items() if current == run_id), None)
        with self.lock:
            if what == "resume":
                self.control.pop(run_id, None)
            else:
                self.control[run_id] = "paused" if what == "pause" else "stopped"
                # The graph stays claimed until the worker exits; cancellation is asynchronous.
        if graph is None:
            return self._resume_cold(run_id) if what == "resume" else (
                json.dumps({"error": "that run is not running", "run": run_id}), 409)
        return json.dumps({"run": run_id, "asked": what}), 202

    def run_dir(self, run_id: str) -> Path | None:
        """Where a run lives, whichever graph it belongs to.

        A run id is unique across the root, so the graph does not have to be named to find one — which
        matters because a caller holding a run id from a trigger is not holding a graph name.
        """
        for workspace in self.workspaces():
            candidate = workspace / "runs" / run_id
            if (candidate / "run.json").is_file():
                return candidate
        return None

    def active_runs(self, graph: str) -> list[str]:
        return list(dict.fromkeys(
            ([self.running[graph]] if graph in self.running else []) +
            [identifier for identifier, name in self.channel_runs.items() if name == graph] +
            [identifier for identifier, name in self.graph_calls.active.items() if name == graph]))

    def active_run(self, graph: str) -> str | None:
        return next(iter(self.active_runs(graph)), None)

    def delete_run(self, run_id: str) -> tuple[str, int]:
        """Remove a run and everything it left behind, unless it is still running."""
        if not run_id or "/" in run_id or run_id in (".", ".."):
            return json.dumps({"error": "no such run"}), 404
        with self.lock:
            if (run_id in self.running.values() or run_id in self.channel_runs or
                    run_id in self.graph_calls.active):
                return json.dumps({"error": "that run is still running", "run": run_id}), 409
            run_dir = self.run_dir(run_id)
            if run_dir is None:
                return json.dumps({"error": "no such run"}), 404
            if run_dir.parent.parent.name in self.channel_runs.values():
                return json.dumps({"error": "this graph is using conversation history", "run": run_id}), 409
            for item in self.runs():
                if item.get("trigger", {}).get("run") == run_id and item["running"]:
                    return json.dumps({"error": "this run has an active called Run"}), 409
            for workspace in self.workspaces():
                for record in (workspace / "runs").glob("*/control/**/graph-call.json"):
                    if json.loads(record.read_text()).get("run") == run_id:
                        return json.dumps({"error": "this run is referenced by a Graph call"}), 409
            shutil.rmtree(run_dir)
        return json.dumps({"run": run_id, "deleted": True}), 200

    def delete_graph(self, name: str) -> tuple[str, int]:
        """Remove a graph workspace, including its runs, unless it is still running."""
        with self.lock:
            if self.active_run(name):
                return json.dumps({"error": "that graph is still running",
                                   "running": self.active_run(name)}), 409
            workspace = self.workspace(name)
            if workspace is None:
                return json.dumps({"error": f"no such graph: {name}"}), 404
            references = [item for item in self.graph_calls.relations()["calls"]
                          if item["target"] == name and item["graph"] != name]
            if references:
                return json.dumps({"error": "this graph is referenced by Graph calls",
                                   "calls": references}), 409
            for item in self.runs():
                if item["trigger"].get("graph") == name and item["running"]:
                    return json.dumps({"error": "this graph has an active called Run"}), 409
            shutil.rmtree(workspace)
        return json.dumps({"graph": name, "deleted": True}), 200

    def create_session(self, session_id: str | None = None) -> tuple[str, int]:
        """Create the Anchor-owned half of a Pilot conversation."""
        if session_id is not None and not isinstance(session_id, str):
            return json.dumps({"error": "session id must be a string"}), 400
        try:
            session = self.sessions.create(session_id)
        except FileExistsError:
            return json.dumps({"error": "that session already exists"}), 409
        except (ValueError, OSError) as exc:
            return json.dumps({"error": str(exc)}, ensure_ascii=False), 400
        return json.dumps({"session": session.model_dump(mode="json")}, ensure_ascii=False), 201

    def session(self, session_id: str) -> tuple[str, int]:
        try:
            session = self.sessions.get(session_id)
        except KeyError:
            return json.dumps({"error": "no such session"}), 404
        except ValueError as exc:
            return json.dumps({"error": str(exc)}, ensure_ascii=False), 400
        return json.dumps({"session": session.model_dump(mode="json")}, ensure_ascii=False), 200

    def sessions_list(self) -> tuple[str, int]:
        # The Pilot sidebar is a list of Pilot conversations, not every Session Anchor owns.
        # Channel Graph conversations share the same persistence but have a separate UI and
        # audience; listing them here leaked WeCom chats into Pilot's conversation picker.
        sessions = [item for item in self.sessions.list() if not item.graph and not item.channel]
        return json.dumps({"sessions": [item.model_dump(mode="json")
                                         for item in sessions]},
                          ensure_ascii=False), 200

    def session_events(self, session_id: str) -> tuple[str, int]:
        try:
            events = self.sessions.events(session_id)
        except KeyError:
            return json.dumps({"error": "no such session"}), 404
        except ValueError as exc:
            return json.dumps({"error": str(exc)}, ensure_ascii=False), 400
        return json.dumps({"events": [event.model_dump(mode="json") for event in events]},
                          ensure_ascii=False), 200

    def set_session_status(self, session_id: str, status: str, reason: str = "") -> tuple[str, int]:
        if status not in ("active", "waiting_user", "interrupted", "archived"):
            return json.dumps({"error": f"unknown session status: {status}"}), 400
        try:
            with self.lock:
                if session_id in self.pilot_active:
                    return json.dumps({"error": "that session is processing a message"}), 409
                session = self.sessions.set_status(session_id, status, reason=reason)
        except KeyError:
            return json.dumps({"error": "no such session"}), 404
        except ValueError as exc:
            return json.dumps({"error": str(exc)}, ensure_ascii=False), 400
        return json.dumps({"session": session.model_dump(mode="json")}, ensure_ascii=False), 200

    def confirm_session(self, session_id: str, action: str, approval_key: str) -> tuple[str, int]:
        if not approval_key:
            return json.dumps({"error": "approval_key is required"}), 400
        try:
            session = self.sessions.decide_approval(session_id, approval_key, True, action)
        except KeyError:
            return json.dumps({"error": "no such session"}), 404
        except ValueError as exc:
            return json.dumps({"error": str(exc)}, ensure_ascii=False), 409
        return json.dumps({"session": session.model_dump(mode="json"), "confirmed": True},
                          ensure_ascii=False), 200

    def reject_session(self, session_id: str, action: str, approval_key: str) -> tuple[str, int]:
        if not approval_key:
            return json.dumps({"error": "approval_key is required"}), 400
        try:
            session = self.sessions.decide_approval(session_id, approval_key, False, action)
        except KeyError:
            return json.dumps({"error": "no such session"}), 404
        except ValueError as exc:
            return json.dumps({"error": str(exc)}, ensure_ascii=False), 409
        return json.dumps({"session": session.model_dump(mode="json"), "rejected": True},
                          ensure_ascii=False), 200

    def attach_session_run(self, session_id: str, run_id: str) -> tuple[str, int]:
        if self.run_dir(run_id) is None:
            return json.dumps({"error": "no such run", "run": run_id}), 404
        try:
            session = self.sessions.attach_run(session_id, run_id)
        except KeyError:
            return json.dumps({"error": "no such session"}), 404
        except ValueError as exc:
            return json.dumps({"error": str(exc)}, ensure_ascii=False), 400
        return json.dumps({"session": session.model_dump(mode="json")}, ensure_ascii=False), 200

    def delete_session(self, session_id: str) -> tuple[str, int]:
        with self.lock:
            if session_id in self.pilot_active:
                return json.dumps({"error": "that session is processing a message"}), 409
            try:
                self.sessions.delete(session_id)
                self.turns.delete_session(session_id)
            except KeyError:
                return json.dumps({"error": "no such session"}), 404
            except ValueError as exc:
                return json.dumps({"error": str(exc)}, ensure_ascii=False), 409
            except OSError as exc:
                return json.dumps({"error": str(exc)}, ensure_ascii=False), 400
            return json.dumps({"session": session_id, "deleted": True}), 200

    def rename_session(self, session_id: str, title: str) -> tuple[str, int]:
        with self.lock:
            try:
                session = self.sessions.rename(session_id, title)
            except KeyError:
                return json.dumps({"error": "no such session"}), 404
            except (ValueError, OSError) as exc:
                return json.dumps({"error": str(exc)}, ensure_ascii=False), 400
        return json.dumps({"session": session.model_dump(mode="json")}, ensure_ascii=False), 200

    def pilot_messages(self, session_id: str) -> tuple[str, int]:
        try:
            session = self.sessions.get(session_id)
            if session.graph:
                from anchor.channel.assistant import history
                return json.dumps({"messages": history(self, session_id)}, ensure_ascii=False), 200
            from anchor.pilot import history
            messages = history(self.sessions, session)
            if not session.title:
                first = next((item["text"] for item in messages if item["role"] == "user"), "")
                if first:
                    self.sessions.name_from_prompt(session_id, first)
        except KeyError:
            return json.dumps({"error": "no such session"}), 404
        except Exception as exc:  # noqa: BLE001 - report persistence/configuration failures at API boundary
            return json.dumps({"error": str(exc)}, ensure_ascii=False), 500
        return json.dumps({"messages": messages}, ensure_ascii=False), 200

    def create_turn(self, session_id: str, request_id: str, prompt: str | None,  # noqa: C901
                    channel_input: dict | None = None) -> tuple[str, int]:  # noqa: C901
        from pydantic_ai import CancellationToken

        if not isinstance(request_id, str) or not request_id.strip() or len(request_id) > 200:
            return json.dumps({"error": "request_id must contain 1 to 200 characters"}), 400
        channel_attachment_only = isinstance(channel_input, dict) and bool(channel_input.get("attachments"))
        if prompt is not None and (not isinstance(prompt, str) or
                                   (not prompt.strip() and not channel_attachment_only) or
                                   len(prompt) > 100_000):
            return json.dumps({"error": "message must contain 1 to 100000 characters"}), 400
        with self.lock:
            try:
                session = self.sessions.get(session_id)
                existing = self.turns.find_request(session_id, request_id)
                if existing:
                    if existing["prompt"] != prompt:
                        raise ValueError("request_id was already used for different input")
                    return json.dumps({"turn": existing}, ensure_ascii=False), 202
                if session_id in self.pilot_active and not session.graph:
                    raise ValueError("that session is already processing a message")
                if session.graph and prompt is None:
                    raise ValueError("send a new message to continue this Graph conversation")
                if session.graph in self.running:
                    raise ValueError("the assistant Graph has an active non-conversation Run")
                if session.status not in {"active", "waiting_user", "interrupted"}:
                    raise ValueError(f"session is {session.status}")
                if any(item.get("status") == "requested" for item in session.approvals):
                    raise ValueError("confirm or reject the pending operation first")
                if prompt is not None and any(item.get("status") in {"approved", "rejected"}
                                              for item in session.approvals):
                    raise ValueError("resume the confirmed operation first")
                predecessor = None
                completed = None
                if session.graph:
                    tail = self.channel_tail.get(session_id)
                    if tail:
                        prior_id, predecessor = tail
                        self.control[f"channel-{prior_id}"] = "stopped"
                        self.turns.finish(prior_id, "stopped", "superseded by a newer message")
                    elif session_id in self.session_background:
                        background = self.session_background[session_id]
                        background.interrupt()
                        predecessor = background.released
                    completed = threading.Event()
                turn, _ = self.turns.create(session_id, request_id, prompt, channel_input=channel_input)
                self.pilot_active.add(session_id)
                self.pilot_tokens[session_id] = CancellationToken()
                if session.graph:
                    self.channel_runs[f"channel-{turn['id']}"] = session.graph
                    self.channel_tail[session_id] = (turn["id"], completed)
            except KeyError:
                return json.dumps({"error": "no such session"}), 404
            except ValueError as exc:
                return json.dumps({"error": str(exc)}, ensure_ascii=False), 409
            threading.Thread(target=self._run_turn, args=(turn, predecessor, completed), daemon=True).start()
        return json.dumps({"turn": turn}, ensure_ascii=False), 202

    def channel_message(self, event: dict[str, Any]) -> tuple[str, int]:
        from anchor.channel.assistant import receive
        return receive(self, event)

    def _run_turn(self, turn: dict, predecessor: threading.Event | None = None,
                  completed: threading.Event | None = None) -> None:
        try:
            if predecessor is not None:
                predecessor.wait()
            if self.sessions.get(turn["session"]).graph:
                from anchor.channel.assistant import execute
                body, code = execute(self, turn)
            else:
                body, code = self.pilot_message(turn["session"], turn["prompt"], turn_id=turn["id"])
            response = json.loads(body)
            status = ("waiting_approval" if response.get("approvals") else
                      "waiting_user" if response.get("paused") else
                      "completed" if code == 200 else
                      "stopped" if response.get("stopped") else "failed")
        except Exception as exc:  # noqa: BLE001 - settle admission even if execution setup fails
            status, response = "failed", {"error": f"{type(exc).__name__}: {exc}"}
        finally:
            with self.lock:
                try:
                    self.turns.finish(turn["id"], status, response.get("error", ""))
                finally:
                    tail = self.channel_tail.get(turn["session"])
                    if tail is None or tail[0] == turn["id"]:
                        self.pilot_active.discard(turn["session"])
                        self.pilot_tokens.pop(turn["session"], None)
                        self.channel_tail.pop(turn["session"], None)
                    self.channel_runs.pop(f"channel-{turn['id']}", None)
                    self.control.pop(f"channel-{turn['id']}", None)
                    if completed is not None:
                        completed.set()

    def pilot_message(self, session_id: str, prompt: str | None, *, turn_id: str | None = None) -> tuple[str, int]:
        # Imported here rather than at module scope: an op-only graph must run without the harness.
        from pydantic_ai import CancellationToken, DeferredToolRequests, DeferredToolResults

        if prompt is not None and (not prompt.strip() or len(prompt) > 100_000):
            return json.dumps({"error": "message must contain 1 to 100000 characters"}), 400
        with self.lock:
            if session_id in self.pilot_active and turn_id is None:
                return json.dumps({"error": "that session is already processing a message"}), 409
            self.pilot_active.add(session_id)
            token = self.pilot_tokens.get(session_id) if turn_id else CancellationToken()
            self.pilot_tokens[session_id] = token
        try:
            from anchor.pilot import approval_precondition, respond
            session = self.sessions.get(session_id)
            if session.graph:
                return json.dumps({"error": "Graph conversations use the turns endpoint"}), 409
            if prompt is not None and any(item.get("status") == "requested" for item in session.approvals):
                # Same rule as the turn API: a message is not a way around a decision nobody made yet.
                return json.dumps({"error": "confirm or reject the pending operation first"}), 409
            decisions = {} if prompt is not None else self.sessions.approval_decisions(session_id)
            question = self.sessions.pending_question(session_id) if prompt is not None else None
            if prompt is not None and session.status in {"waiting_user", "interrupted"}:
                # Answering a question, or starting a new turn after an interruption. What the next
                # run continues from is the saved record, not the status the previous attempt left.
                self.sessions.set_status(session_id, "active")
                session = self.sessions.get(session_id)
            if session.status != "active" and not (prompt is None and (
                    session.status == "interrupted" or (session.status == "waiting_user" and decisions))):
                return json.dumps({"error": f"session is {session.status}"}), 409
            if prompt is not None:
                self.sessions.name_from_prompt(session_id, prompt)
                self.sessions.append(session_id, "pilot.turn.started")
            extra = ({"turn_id": turn_id, "emit": lambda data: self.turns.append(turn_id, data)}
                     if turn_id else {})
            if question is not None:
                # The user's next message answers the deferred question; it is a tool result, not a
                # new user turn, so the model sees the reply to what it actually asked.
                deferred = DeferredToolResults(calls={question["tool_call_id"]: prompt})
                prompt = None
            else:
                deferred = DeferredToolResults(approvals=decisions) if decisions else None
            answer = respond(self.sessions, self.config, session, prompt, token, scheduler=self,
                             deferred=deferred, **extra)
            if isinstance(answer, DeferredToolRequests):
                pending = [{"tool_call_id": call.tool_call_id, "key": call.tool_call_id,
                            "action": call.tool_name, "target": _call_target(call),
                            "proposal": _call_args(call),
                            "precondition": approval_precondition(self, call.tool_name, _call_args(call))}
                           for call in answer.approvals]
                asked = [{"tool_call_id": call.tool_call_id,
                          "question": (answer.metadata.get(call.tool_call_id) or {}).get("question", "")}
                         for call in answer.calls]
                self.sessions.set_pending(session_id, pending, asked)
                self.sessions.append(session_id, "pilot.turn.paused",
                                     {"calls": [item["tool_call_id"] for item in [*pending, *asked]]})
                return json.dumps({"paused": True, "approvals": pending, "session": session_id},
                                  ensure_ascii=False), 200
            self.sessions.clear_pending(session_id)
            if prompt is None and session.status == "interrupted":
                self.sessions.set_status(session_id, "active")
            self.sessions.append(session_id, "pilot.turn.completed")
            return json.dumps({"message": answer, "session": session_id}, ensure_ascii=False), 200
        except KeyError:
            return json.dumps({"error": "no such session"}), 404
        except Exception as exc:  # noqa: BLE001 - provider and persistence errors become visible to the caller
            try:
                self.sessions.append(session_id, "pilot.turn.failed", {"error": type(exc).__name__})
                self.sessions.set_status(session_id, "interrupted", reason="Pilot turn failed; resume to retry")
            except (KeyError, ValueError, OSError):
                pass
            return json.dumps({"error": str(exc), "stopped": bool(token and token.cancelled)}, ensure_ascii=False), 502
        finally:
            if turn_id is None:
                with self.lock:
                    self.pilot_active.discard(session_id)
                    self.pilot_tokens.pop(session_id, None)

    def stop_pilot(self, session_id: str) -> tuple[str, int]:
        with self.lock:
            token = self.pilot_tokens.get(session_id)
            if token is None:
                return json.dumps({"error": "that session is not processing a message"}), 409
            token.cancel()
            for turn in self.turns.list(session_id):
                if turn["status"] == "running" and f"channel-{turn['id']}" in self.channel_runs:
                    self.control[f"channel-{turn['id']}"] = "stopped"
        return json.dumps({"session": session_id, "asked": "stop"}), 202

    def files(self, run_id: str, node: str) -> tuple[str, int]:
        """What a node left in its workspace. Returns (body, status)."""
        run_dir = self.run_dir(run_id)
        if run_dir is None:
            return json.dumps({"error": "no such run"}), 404
        workspace = _inside(run_dir, node) if _node_name(node) else None
        if workspace is None or not workspace.is_dir():
            return json.dumps({"error": f"no such node: {node}"}), 404
        found: list[dict] = []
        for item in sorted(workspace.rglob("*")):
            if ".git" in item.relative_to(workspace).parts or not item.is_file():
                continue
            found.append({"path": str(item.relative_to(workspace)), "size": item.stat().st_size})
            if len(found) >= FILE_LIST_CAP:
                break
        return json.dumps({"node": node, "files": found,
                           "truncated": len(found) >= FILE_LIST_CAP}, ensure_ascii=False), 200

    def locate(self, run_id: str, node: str, name: str) -> Path | None:
        """One file inside a node's workspace, or None if that is not where it is.

        Two methods rather than one returning either a dict or a path: a preview and a download want
        different things from the same file, and a return value whose type is its own mode is how a
        caller ends up sending a dict as bytes.
        """
        run_dir = self.run_dir(run_id)
        if run_dir is None:
            return None
        workspace = _inside(run_dir, node) if _node_name(node) else None
        if workspace is None or not workspace.is_dir():
            return None
        target = _inside(workspace, name)
        return target if target is not None and target.is_file() else None

    def read_file(self, run_id: str, node: str, name: str) -> tuple[str, int]:
        """One file's contents, as text, if it is text. Returns (body, status)."""
        target = self.locate(run_id, node, name)
        if target is None:
            return json.dumps({"error": f"no such file: {name}"}), 404
        size = target.stat().st_size
        try:
            text = target.read_text(encoding="utf-8")
        except (UnicodeDecodeError, ValueError):
            # Not text. A mangled decode of it would be worse than saying so, and downloading is where
            # a binary belongs anyway.
            return json.dumps({"path": name, "size": size, "binary": True, "text": "",
                               "truncated": False}, ensure_ascii=False), 200
        return json.dumps({"path": name, "size": size, "binary": False,
                           "text": text[:FILE_TEXT_CAP], "truncated": len(text) > FILE_TEXT_CAP},
                          ensure_ascii=False), 200

    def _resume_cold(self, run_id: str) -> tuple[str, int]:
        """Continue a run that is not in this process — one a restart left, or one paused earlier."""
        for workspace in self.workspaces():
            run_dir = workspace / "runs" / run_id
            if not (run_dir / "run.json").is_file():
                continue
            source = runner.RunState.load(run_dir).trigger.get("source")
            if source == "graph_call":
                state = runner.RunState.load(run_dir)
                state.status = "running"
                state.save(run_dir)
                self.graph_calls.start(workspace, run_id)
                return json.dumps({"run": run_id, "graph": workspace.name, "resumed": True}), 202
            if source == "channel":
                return json.dumps({"error": "continue a channel Graph with a new conversation message"}), 409
            with self.lock:
                if self.active_run(workspace.name):
                    return json.dumps({"error": "this graph is already running",
                                       "running": self.active_run(workspace.name)}), 409
                self.running[workspace.name] = run_id
            threading.Thread(target=self._run, args=(workspace, run_id, None, True),
                             daemon=True).start()
            return json.dumps({"run": run_id, "graph": workspace.name, "resumed": True}), 202
        return json.dumps({"error": "no such run"}), 404

    def save(self, name: str, definition: dict) -> tuple[str, int]:
        """Validate a graph and write it, or say why not.

        Validated with the same function a run uses, so a graph this accepts is a graph that runs —
        a page that saved something the runner then refused would be worse than no page. The file is
        replaced whole rather than edited, because `graph.json` is the graph and a half-written one is
        not a smaller graph, it is a broken one.
        """
        # The directory, not `workspace`: creating a graph makes the directory and then saves into
        # it, and looking it up by the stricter rule would report the graph it had just made as
        # missing.
        workspace = self._directory(name)
        if workspace is None:
            return json.dumps({"error": f"no such graph: {name}"}), 404
        if self.active_run(name):
            return json.dumps({"error": "this graph is running; changing it now would change what "
                                       "the run reads", "running": self.active_run(name)}), 409
        try:
            parsed = graph_module.parse(definition)
            self.graph_calls.validate_targets(parsed)
            for node in parsed.nodes.values():
                self.library.attach(node.plugins)
        except Exception as exc:  # noqa: BLE001 - the message is the point
            return json.dumps({"error": f"{type(exc).__name__}: {exc}"}), 400
        with self.lock:
            if self.active_run(name):
                return json.dumps({"error": "this graph is running", "running": self.active_run(name)}), 409
            target = workspace / "graph.json"
            staged = target.with_suffix(".json.incoming")
            staged.write_text(json.dumps(definition, ensure_ascii=False, indent=2) + "\n",
                              encoding="utf-8")
            staged.replace(target)                 # rename, so a reader never sees a partial file
        return json.dumps({"graph": name, "saved": True}), 200

    def create(self, name: str, definition: dict | None) -> tuple[str, int]:
        if not name or "/" in name or name.startswith("."):
            return json.dumps({"error": "a graph name may not be empty or contain a slash"}), 400
        if self._directory(name) is not None:
            return json.dumps({"error": f"a graph called {name!r} already exists"}), 409
        (self.root / "workspaces" / name).mkdir(parents=True, exist_ok=True)
        return self.save(name, definition or _starter_graph(name))

    def resume_all(self) -> None:
        self._recover_admissions()
        """Pick up anything a previous process left running. The whole of recovery."""
        for workspace in self.workspaces():
            runs = sorted((workspace / "runs").glob("*/run.json")) if (workspace / "runs").is_dir() else []
            for state_file in runs:
                state = json.loads(state_file.read_text(encoding="utf-8"))
                admission = state_file.parent / "admission.json"
                pending = admission.exists() and json.loads(admission.read_text()).get("session_pending")
                if state.get("status") != "running" and not pending:
                    continue
                if state.get("trigger", {}).get("source") == "channel":
                    interrupted = runner.RunState.load(state_file.parent)
                    interrupted.status = "interrupted"
                    interrupted.reason = "service restarted; channel operations are not automatically replayed"
                    interrupted.save(state_file.parent)
                    continue
                run_id = state_file.parent.name
                if state.get("trigger", {}).get("source") == "graph_call":
                    self.graph_calls.start(workspace, run_id)
                    continue
                with self.lock:
                    if workspace.name in self.running:
                        continue
                    self.running[workspace.name] = run_id
                print(json.dumps({"resume": run_id, "graph": workspace.name}), flush=True)
                threading.Thread(target=self._run,
                                 args=(workspace, run_id, state.get("objective"), True,
                                       None, state.get("trigger")),
                                 daemon=True).start()

    def _recover_admissions(self) -> None:
        """Find accepted child admissions whose native state was not yet visible at crash time."""
        for workspace in self.workspaces():
            for admission in (workspace / "runs").glob("*/admission.json"):
                try:
                    record = json.loads(admission.read_text(encoding="utf-8"))
                    child = admission.parent
                    if not (child / "run.json").exists():
                        definition = record["definition"]
                        graph = graph_module.parse(definition)
                        _write = getattr(__import__("anchor.graph_calls", fromlist=["_write"]), "_write")
                        _write(child / "graph.json", definition)
                        runner.RunState(objective=graph.objective, started=runner._now(),
                                        input=record.get("input", {}), trigger=record["trigger"]).save(child)
                    self.graph_calls.start(workspace, admission.parent.name)
                except (OSError, ValueError, KeyError, json.JSONDecodeError):
                    continue

    def _run(self, workspace: Path, run_id: str, objective: str | None,
             resume: bool = False, run_input: dict | None = None,
             trigger: dict | None = None) -> None:
        def asked() -> str | None:
            with self.lock:
                return self.control.get(run_id)

        try:
            from anchor.channel.tools import factory
            options = dict(objective=objective, config_path=self.config, run_id=run_id,
                           resume=(workspace / "runs" / run_id) if resume else None,
                           run_input=run_input, trigger=trigger,
                           call_handler=self.graph_calls.factory(workspace, run_id),
                           stop_request=asked, library_root=self.library.root,
                           toolset_factory=factory(self, workspace, run_id,
                                                   cancelled=lambda: asked() == "stopped"))
            admission = workspace / "runs" / run_id / "admission.json"
            record = json.loads(admission.read_text()) if admission.exists() else {}
            if record.get("spec", {}).get("session"):
                from anchor.channel.background import execute
                execute(self, record, workspace, run_id, options)
            else:
                runner.run(workspace, **options)
        except Exception as exc:  # noqa: BLE001 - preserve failures before the runner entered
            traceback.print_exc()
            run_dir = workspace / "runs" / run_id
            if (run_dir / "run.json").exists():
                state = runner.RunState.load(run_dir)
                state.error = f"{type(exc).__name__}: {exc}"
                if state.status == "running":
                    state.status = "interrupted"
                admission = run_dir / "admission.json"
                if admission.exists():
                    record = json.loads(admission.read_text(encoding="utf-8"))
                    if record.get("session_pending"):
                        record["delivery_error"] = state.error
                        record["session_pending"] = False
                        from anchor.graph_calls import _write
                        _write(admission, record)
                state.save(run_dir)
        finally:
            with self.lock:
                if self.running.get(workspace.name) == run_id:
                    self.running.pop(workspace.name, None)
                self.graph_calls.active.pop(run_id, None)
                self.control.pop(run_id, None)

    def runs(self) -> list[dict]:
        found = []
        for workspace in self.workspaces():
            base = workspace / "runs"
            for state_file in sorted(base.glob("*/run.json"), reverse=True) if base.is_dir() else []:
                state = json.loads(state_file.read_text(encoding="utf-8"))
                found.append({"run": state_file.parent.name, "graph": workspace.name,
                              "status": state.get("status"),
                              "running": (self.running.get(workspace.name) == state_file.parent.name
                                          or state_file.parent.name in self.channel_runs
                                          or state_file.parent.name in self.graph_calls.active),
                              "started": state.get("started"), "updated": state.get("updated"),
                              "executed": state.get("executed", []),
                              "objective": (state.get("objective") or "")[:200],
                              "trigger": state.get("trigger", {"source": "manual"})})
        return found

    def run(self, graph: str, run_id: str) -> dict | None:
        workspace = self.workspace(graph) or next(
            (item for item in self.workspaces()
             if (item / "runs" / run_id / "run.json").is_file()), None)
        if workspace is None:
            return None
        base = workspace / "runs" / run_id
        if not (base / "run.json").is_file():
            return None
        state = json.loads((base / "run.json").read_text(encoding="utf-8"))
        traces = {}
        for trace in sorted(base.glob("*.trace.jsonl")):
            lines = trace.read_text(encoding="utf-8").splitlines()[-TAIL_LINES:]
            traces[trace.name.removesuffix(".trace.jsonl")] = [message for line in lines
                                                                 for message in _readable(line)]
        return {"graph": workspace.name, "run": run_id, "state": state, "traces": traces,
                "calls": self.graph_calls.projections(base),
                "plugins": (json.loads((base / "plugins.json").read_text(encoding="utf-8"))
                            if (base / "plugins.json").is_file() else {}),
                "nodes": sorted(item.name for item in base.iterdir() if item.is_dir())}


def _starter_graph(name: str) -> dict:
    """A graph that runs, as the starting point for a new one.

    Small enough to read in one screen and complete enough to be a real graph: two nodes, one edge,
    and an objective. Anyone editing it will replace all of it, which is easier from something that
    works than from an empty file that fails validation for reasons they have to look up.
    """
    return {
        "entry": "first",
        "objective": f"{name}: 说明这个图要做什么。",
        "agents": {
            "worker": {"model": "models.academic", "network": False,
                       "instructions": "在一句话里说明这个节点要做什么。"},
        },
        "nodes": [{"id": "first", "agent": "worker"}],
        "edges": [],
    }


#: How much of one message a view is given. Generous, because the view collapses what it does not
#: need and a command's output is often the whole point — but not unbounded, because a node can read a
#: large file and the payload would carry it on every poll. What was cut is said rather than silently
#: dropped, so a reader is never shown a truncated result believing it is the whole one.
TAIL_TEXT = 20000


#: How many files one listing returns. A node can write a great many, and a listing is only ever
#: something a person scrolls.
FILE_LIST_CAP = 3000
#: How much of one text file the view is given. The rest is reachable by downloading it.
FILE_TEXT_CAP = 200000


def _node_name(name: str) -> bool:
    """Whether this could be a node id at all.

    A node id is a path of ordinary segments — a module's nodes are named `write/draft` — and no
    segment is `.`, `..` or empty. Checked as a name rather than by resolving, because resolving lets
    `notes/../..` land on the run's own directory: inside it, allowed, and not a node. That would turn
    "list this node's files" into "list every node's files", which the test for it found.
    """
    return bool(name) and all(part not in ("", ".", "..") for part in name.split("/"))


def _inside(base: Path, name: str) -> Path | None:
    """`base / name`, if that is inside `base`. Otherwise None.

    `name` comes from a URL, so this is the one place that decides whether a request can read something
    it should not. It **resolves** rather than looking for `..` in the string: `a/../../b`, an absolute
    name, and a symlink all reach elsewhere by different spellings, and a check that reads the text of
    the name catches none of them reliably. Resolving follows symlinks, so a link pointing out of the
    workspace is refused too — which is also what the sandbox does with one.
    """
    if not name or "\x00" in name:
        return None
    try:
        root = base.resolve(strict=True)
        candidate = (base / name).resolve(strict=True)
    except OSError:
        return None
    return candidate if candidate == root or root in candidate.parents else None


def _readable(line: str) -> list[dict]:  # noqa: C901 - both trace formats are projected here
    """A message as something a person can read, without knowing the library's shape.

    The **commands** are carried, not just the tool names. They are the most informative thing in a
    trace and the previous projection threw them away, which is most of why the conversation view had
    nothing to show but a wall of text.
    """
    message = json.loads(line)
    if message.get("kind") in ("request", "response"):
        def view(role: str, content: str, *, commands: list[str] | None = None,
                 exit_status: str | None = None) -> dict:
            return {"role": role, "text": content[:TAIL_TEXT],
                    "truncated": len(content) > TAIL_TEXT, "commands": commands or [],
                    "exit_status": exit_status}

        if message["kind"] == "response":
            words = [str(part.get("content") or "") for part in message.get("parts", [])
                     if part.get("part_kind") == "text"]
            commands = []
            for part in message.get("parts", []):
                if part.get("part_kind") != "tool-call":
                    continue
                arguments = part.get("args")
                try:
                    arguments = json.loads(arguments) if isinstance(arguments, str) else arguments
                except (TypeError, ValueError):
                    pass
                command = arguments.get("command") if isinstance(arguments, dict) else arguments
                commands.append(str(command or part.get("tool_name") or ""))
            return [view("assistant", "\n\n".join(words), commands=commands)] if words or commands else []

        result = []
        for part in message.get("parts", []):
            kind = part.get("part_kind")
            if kind in ("user-prompt", "system-prompt", "retry-prompt"):
                result.append(view("system" if kind == "system-prompt" else "user",
                                   str(part.get("content") or "")))
            elif kind == "tool-return":
                content = str(part.get("content") or "")
                status = None
                if content.startswith("<returncode>"):
                    code, separator, rest = content.partition("</returncode>\n")
                    if separator:
                        status = code.removeprefix("<returncode>")
                        content = rest
                        if content.startswith("<output>\n"):
                            content = content.removeprefix("<output>\n")
                            output, end, extra = content.rpartition("\n</output>")
                            if end:
                                content = output + extra
                        status = "succeeded" if status == "0" else f"exit {status}"
                result.append(view("tool", content, exit_status=status))
        return result

    content = message.get("content")
    if isinstance(content, list):
        content = " | ".join(str(part.get("text", part)) for part in content)
    text = str(content or "")
    commands = []
    for item in message.get("tool_calls") or []:
        arguments = (item.get("function") or {}).get("arguments")
        try:
            parsed = json.loads(arguments) if isinstance(arguments, str) else arguments
            command = (parsed or {}).get("command") if isinstance(parsed, dict) else arguments
        except (TypeError, ValueError):
            command = arguments
        commands.append(str(command) if command else "")
    return [{
        "role": message.get("role"),
        "text": text[:TAIL_TEXT],
        "truncated": len(text) > TAIL_TEXT,
        "commands": commands,
        "exit_status": (message.get("extra") or {}).get("exit_status"),
    }]


def _stamp() -> str:
    from datetime import datetime, timezone
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")


class Handler(BaseHTTPRequestHandler):
    scheduler: Scheduler          # set on the server's class by `serve`

    def log_message(self, fmt, *args):        # quieter: one line per request is enough
        print(json.dumps({"request": self.path, "status": args[1] if len(args) > 1 else ""}),
              flush=True)

    def _body(self) -> dict | None:
        """The request body, or None after answering that it was not usable."""
        length = int(self.headers.get("Content-Length") or 0)
        try:
            body = json.loads(self.rfile.read(length) or b"{}")
            if not isinstance(body, dict):
                raise ValueError("body must be a JSON object")
            return body
        except (json.JSONDecodeError, ValueError):
            self._send(json.dumps({"error": "body must be JSON"}), 400)
            return None

    def _serve_built(self, parts: list[str]) -> bool:
        """The built interface, if there is one.

        Served from the same process as the API so a deployment is one thing to start and one thing to
        reach. Absent until someone has run `npm --prefix apps/web run build`, and absent is fine —
        the API is the whole of the system and the page is a way of looking at it.
        """
        index = BUILT / "index.html"
        if not index.is_file():
            return False
        target = (BUILT.joinpath(*parts) if parts else index).resolve()
        if BUILT.resolve() not in target.parents and target != index.resolve():
            return False                       # never serve outside the built directory
        if not target.is_file():
            target = index                     # a client-side path; hand back the page
        kinds = {".html": "text/html", ".js": "text/javascript", ".css": "text/css",
                 ".svg": "image/svg+xml", ".json": "application/json",
                 ".woff2": "font/woff2", ".png": "image/png"}
        payload = target.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", kinds.get(target.suffix, "application/octet-stream"))
        # The HTML entry point names hashed JS/CSS bundles. Revalidate it so a deployment picks up
        # the newest bundle after a build instead of keeping an old color calculation indefinitely.
        if target.suffix == ".html":
            self.send_header("Cache-Control", "no-cache, must-revalidate")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
        return True

    def _send(self, body: str, status: int = 200) -> None:
        payload = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _authorized(self) -> bool:
        keys = self.scheduler.api_keys
        if not keys:
            return True  # serve() only allows this mode on a loopback listener.
        scheme, separator, value = self.headers.get("Authorization", "").partition(" ")
        valid = separator and scheme.lower() == "bearer" and value
        if valid and any(hmac.compare_digest(value.encode(), key.encode()) for key in keys):
            return True
        self._send(json.dumps({"error": "invalid API key"}), 401)
        return False

    def _send_file(self, path: Path) -> None:
        """Download artifacts; SVGs also work as passive images in Markdown previews."""
        payload = path.read_bytes()
        self.send_response(200)
        image_types = {".svg": "image/svg+xml", ".png": "image/png", ".jpg": "image/jpeg",
                       ".jpeg": "image/jpeg", ".gif": "image/gif", ".webp": "image/webp"}
        self.send_header("Content-Type", image_types.get(path.suffix.lower(), "application/octet-stream"))
        self.send_header("Content-Security-Policy", "sandbox; default-src 'none'; style-src 'unsafe-inline'")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Content-Disposition", f"attachment; filename*=UTF-8''{quote(path.name)}")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _get_plugin(self, parts: list[str]) -> None:
        try:
            if len(parts) == 1:
                return self._send(json.dumps({"plugins": self.scheduler.library.catalog()}, ensure_ascii=False))
            if len(parts) == 2:
                return self._send(json.dumps(self.scheduler.library.detail(parts[1]), ensure_ascii=False))
            if len(parts) >= 4 and parts[2] == "files":
                return self._send_file(self.scheduler.library.file(parts[1], "/".join(parts[3:])))
        except (ValueError, OSError) as exc:
            return self._send(json.dumps({"error": str(exc)}, ensure_ascii=False), 400)
        self._send(json.dumps({"error": "not found"}), 404)

    def _authorize_mcp(self, parts: list[str]) -> None:
        if len(parts) != 4 or parts[0] != "plugins" or parts[2] != "authorize":
            return self._send(json.dumps({"error": "not found"}), 404)
        try:
            import asyncio
            from anchor.node.mcp import http_toolset
            servers = dict(self.scheduler.library.mcp_servers(parts[1]))
            server = servers[parts[3]]
            if "url" not in server or not (server.get("oauth_resource") or server.get("auth") == "oauth"):
                raise ValueError("this MCP server does not declare OAuth")

            async def authorize() -> None:
                toolset = http_toolset(parts[3], server, interactive=True)
                async with toolset:
                    await toolset.get_tools()

            # Async servers already own this thread's loop; the authorization endpoint is sync HTTP.
            asyncio.run(authorize())
            self._send(json.dumps({"authorized": True}))
        except KeyError:
            self._send(json.dumps({"error": "no such MCP server"}), 404)
        except (ValueError, OSError, RuntimeError, TimeoutError) as exc:
            self._send(json.dumps({"error": str(exc)}, ensure_ascii=False), 400)

    def _turn_stream(self, session_id: str, turn_id: str, cursor: str) -> None:
        try:
            after = int(cursor)
            if after < 0:
                raise ValueError("cursor must be nonnegative")
            self.scheduler.sessions.get(session_id)
            self.scheduler.turns.get(session_id, turn_id)
        except (ValueError, KeyError) as exc:
            return self._send(json.dumps({"error": str(exc)}), 404 if isinstance(exc, KeyError) else 400)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("X-Accel-Buffering", "no")
        self.send_header("x-vercel-ai-ui-message-stream", "v1")
        self.end_headers()
        try:
            heartbeat = time.monotonic()
            while True:
                # Read terminal state BEFORE draining: completion commits after the last event.
                turn = self.scheduler.turns.get(session_id, turn_id)
                events = self.scheduler.turns.events(session_id, turn_id, after)
                for event in events:
                    payload = json.dumps(event["data"], ensure_ascii=False)
                    self.wfile.write(f'id: {event["seq"]}\ndata: {payload}\n\n'.encode())
                    after = event["seq"]
                self.wfile.flush()
                if turn["status"] != "running" and len(events) < 256:
                    self.wfile.write(f'event: turn\ndata: {json.dumps(turn, ensure_ascii=False)}\n\n'.encode())
                    self.wfile.flush()
                    return
                if len(events) == 256:
                    continue
                if time.monotonic() - heartbeat >= 10:
                    self.wfile.write(b": heartbeat\n\n")
                    self.wfile.flush()
                    heartbeat = time.monotonic()
                time.sleep(0.1)
        except (BrokenPipeError, ConnectionResetError, KeyError):
            return  # The worker belongs to the service, not this subscriber.

    def _channel_stream(self, turn: dict) -> None:
        """Only public answer snapshots and the validated final reply, using the existing turn store."""
        from anchor.channel.assistant import result
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("X-Accel-Buffering", "no")
        self.end_headers()
        after, heartbeat = 0, time.monotonic()
        try:
            while True:
                current = self.scheduler.turns.get(turn["session"], turn["id"])
                events = self.scheduler.turns.events(turn["session"], turn["id"], after)
                if current["status"] == "running":
                    for event in events:
                        if event["data"].get("type") == "channel-output":
                            data = json.dumps({"text": event["data"]["text"]}, ensure_ascii=False)
                            self.wfile.write(f"event: progress\ndata: {data}\n\n".encode())
                if events:
                    after = events[-1]["seq"]
                if current["status"] != "running":
                    body, status = result(self.scheduler, current)
                    value = json.loads(body)
                    if status != 200:
                        value = {"error": value.get("error", "channel execution failed")}
                    self.wfile.write(f"event: reply\ndata: {json.dumps(value, ensure_ascii=False)}\n\n".encode())
                    self.wfile.flush()
                    return
                if time.monotonic() - heartbeat >= 10:
                    self.wfile.write(b": heartbeat\n\n")
                    heartbeat = time.monotonic()
                self.wfile.flush()
                if len(events) < 256:
                    time.sleep(0.1)
        except (BrokenPipeError, ConnectionResetError, KeyError):
            return

    def _response_stream(self, response_id: str) -> None:
        ref = self.scheduler.response_refs[response_id]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("X-Accel-Buffering", "no")
        self.end_headers()

        def emit(kind: str, payload: dict) -> None:
            self.wfile.write(f"event: {kind}\ndata: {json.dumps(payload, ensure_ascii=False)}\n\n".encode())
            self.wfile.flush()

        response, _ = self.scheduler.response_result(response_id)
        response["status"] = "in_progress"
        response["output"] = []
        response["output_text"] = ""
        emit("response.created", {"type": "response.created", "response": response})
        emit("response.in_progress", {"type": "response.in_progress", "response": response})
        cursor = 0
        try:
            while True:
                turn = self.scheduler.turns.get(ref["session"], ref["turn"])
                events = self.scheduler.turns.events(ref["session"], ref["turn"], cursor)
                for event in events:
                    data = event["data"]
                    cursor = event["seq"]
                    if data.get("type") == "text-delta":
                        emit("response.output_text.delta", {"type": "response.output_text.delta",
                             "item_id": "msg_" + response_id.removeprefix("resp_"),
                             "output_index": 0, "content_index": 0,
                             "delta": data.get("delta", "")})
                if turn["status"] != "running" and len(events) < 256:
                    response, _ = self.scheduler.response_result(response_id)
                    kind = "response.completed" if turn["status"] == "completed" else "response.failed"
                    response["status"] = "completed" if kind.endswith("completed") else "failed"
                    emit(kind, {"type": kind, "response": response})
                    return
                if len(events) < 256:
                    time.sleep(0.1)
        except (BrokenPipeError, ConnectionResetError, KeyError):
            return

    def do_GET(self) -> None:  # noqa: C901 - one small HTTP router keeps endpoint behavior visible
        parsed = urlparse(self.path)
        query = parse_qs(parsed.query)
        path = PurePosixPath(unquote(parsed.path))
        parts = [part for part in path.parts if part != "/"]
        if not parts or parts[0] == "assets" or (len(parts) == 1 and "." in parts[0]):
            if self._serve_built(parts):
                return
        if not self._authorized():
            return
        if parts == ["graphs"]:
            names = [{"graph": item.name, "running": self.scheduler.active_run(item.name),
                      "active_runs": self.scheduler.active_runs(item.name)}
                     for item in self.scheduler.workspaces()]
            return self._send(json.dumps({"graphs": names}, ensure_ascii=False))
        if parts == ["graph-relations"]:
            return self._send(json.dumps(self.scheduler.graph_calls.relations(), ensure_ascii=False))
        if parts == ["timeline"]:
            try:
                days = int(query.get("days", ["30"])[0])
                if not 1 <= days <= 366:
                    raise ValueError
                before = query.get("before", [None])[0]
                if before is not None:
                    datetime.fromisoformat(before)
            except ValueError:
                return self._send(json.dumps({"error": "days must be between 1 and 366"}), 400)
            return self._send(json.dumps(self.scheduler.timeline(days, before), ensure_ascii=False))
        if parts == ["schedules"]:
            return self._send(json.dumps({"schedules": self.scheduler.schedules}, ensure_ascii=False))
        if parts == ["v1", "responses"]:
            # Responses are created with POST; GET is intentionally outside the frozen subset.
            return self._send(json.dumps({"error": "not found"}), 404)
        if parts == ["channel-sessions"]:
            return self._send(json.dumps({"sessions": [
                {"id": item.id, "title": item.title, "graph": item.graph,
                 "platform": item.channel.get("source", item.channel.get("platform", ""))}
                for item in self.scheduler.sessions.list() if item.graph and item.status != "archived"]}, ensure_ascii=False))
        if parts == ["sessions"]:
            return self._send(*self.scheduler.sessions_list())
        if len(parts) == 3 and parts[0] == "sessions" and parts[2] == "turns":
            try:
                self.scheduler.sessions.get(parts[1])
                return self._send(json.dumps({"turns": self.scheduler.turns.list(parts[1])}, ensure_ascii=False))
            except (KeyError, ValueError):
                return self._send(json.dumps({"error": "no such session"}), 404)
        if len(parts) == 5 and parts[0] == "sessions" and parts[2] == "turns" and parts[4] == "events":
            return self._turn_stream(parts[1], parts[3], self.headers.get("Last-Event-ID")
                                     or query.get("after", ["0"])[0])
        if len(parts) == 3 and parts[0] == "sessions" and parts[2] == "messages":
            return self._send(*self.scheduler.pilot_messages(parts[1]))
        if len(parts) == 3 and parts[0] == "sessions" and parts[2] == "events":
            return self._send(*self.scheduler.session_events(parts[1]))
        if len(parts) == 2 and parts[0] == "sessions":
            return self._send(*self.scheduler.session(parts[1]))
        if parts and parts[0] == "plugins":
            return self._get_plugin(parts)
        if len(parts) == 2 and parts[0] == "graphs":
            # The graph itself, so a view can draw the topology and not only a run through it.
            workspace = self.scheduler.workspace(parts[1])
            if workspace is None:
                return self._send(json.dumps({"error": "no such graph"}), 404)
            graph = json.loads((workspace / "graph.json").read_text(encoding="utf-8"))
            return self._send(json.dumps({"graph": parts[1], "definition": graph},
                                         ensure_ascii=False))
        if parts == ["runs"]:
            return self._send(json.dumps({"runs": self.scheduler.runs()}, ensure_ascii=False))
        if len(parts) == 4 and parts[0] == "runs" and parts[2] == "files":
            return self._send(*self.scheduler.files(parts[1], parts[3]))
        if len(parts) >= 5 and parts[0] == "runs" and parts[2] == "files":
            name = "/".join(parts[4:])
            if query.get("download"):
                target = self.scheduler.locate(parts[1], parts[3], name)
                if target is None:
                    return self._send(json.dumps({"error": f"no such file: {name}"}), 404)
                return self._send_file(target)
            return self._send(*self.scheduler.read_file(parts[1], parts[3], name))
        if len(parts) == 2 and parts[0] == "runs":
            found = self.scheduler.run(_graph_of(self.scheduler, parts[1]), parts[1])
            return self._send(json.dumps(found, ensure_ascii=False) if found
                              else json.dumps({"error": "no such run"}), 200 if found else 404)
        self._send(json.dumps({"error": "not found"}), 404)

    def do_PUT(self) -> None:
        if not self._authorized():
            return
        parts = [part for part in PurePosixPath(unquote(urlparse(self.path).path)).parts
                 if part != "/"]
        if len(parts) == 2 and parts[0] == "sessions":
            body = self._body()
            if body is None:
                return
            if set(body) != {"title"}:
                return self._send(json.dumps({"error": "body must contain only title"}), 400)
            return self._send(*self.scheduler.rename_session(parts[1], body["title"]))
        if len(parts) != 2 or parts[0] != "graphs":
            return self._send(json.dumps({"error": "not found"}), 404)
        body = self._body()
        if body is None:
            return
        response, status = self.scheduler.save(parts[1], body.get("definition") or {})
        self._send(response, status)

    def do_POST(self) -> None:  # noqa: C901 - one small HTTP router keeps endpoint behavior visible
        parts = [part for part in PurePosixPath(unquote(urlparse(self.path).path)).parts if part != "/"]
        channel_endpoint = parts == ["v1", "channels", "wecom", "events"]
        if not self._authorized():
            return
        if channel_endpoint:
            body = self._body()
            if body is None:
                return
            if set(body) != {"event"} or not isinstance(body["event"], dict):
                return self._send(json.dumps({"error": "body must contain only an event object"}), 400)
            if "text/event-stream" in self.headers.get("Accept", ""):
                from anchor.channel.assistant import receive
                response, status = receive(self.scheduler, body["event"], wait=False)
                if status != 202:
                    return self._send(response, status)
                return self._channel_stream(json.loads(response)["turn"])
            return self._send(*self.scheduler.channel_message(body["event"]))
        if len(parts) == 4 and parts[0] == "plugins" and parts[2] == "authorize":
            return self._authorize_mcp(parts)
        if parts == ["plugins", "install"]:
            body = self._body()
            if body is None:
                return
            try:
                installed = self.scheduler.library.install(body.get("source"), body.get("id"),
                                                            replace_existing=body.get("replace") is True)
                return self._send(json.dumps({"id": installed}, ensure_ascii=False), 201)
            except (ValueError, OSError, subprocess.SubprocessError) as exc:
                return self._send(json.dumps({"error": str(exc)}, ensure_ascii=False), 400)
        if parts == ["v1", "responses"]:
            body = self._body()
            if body is None:
                return
            bearer = self.headers.get("Authorization", "").partition(" ")[2]
            owner = hashlib.sha256(bearer.encode()).hexdigest()
            created, status = self.scheduler.response_turn(owner, body)
            if status != 200:
                return self._send(json.dumps(created, ensure_ascii=False), status)
            if created["stream"]:
                return self._response_stream(created["id"])
            deadline = time.monotonic() + 24 * 60 * 60
            while time.monotonic() < deadline:
                response, response_status = self.scheduler.response_result(created["id"])
                if response_status != 202:
                    return self._send(json.dumps(response, ensure_ascii=False), response_status)
                time.sleep(0.1)
            response, _ = self.scheduler.response_result(created["id"])
            return self._send(json.dumps(response, ensure_ascii=False), 202)
        if len(parts) == 3 and parts[0] == "sessions" and parts[2] == "turns":
            body = self._body()
            if body is None:
                return
            resume = body.get("resume") is True
            if (resume and "message" in body) or (not resume and not isinstance(body.get("message"), str)):
                return self._send(json.dumps({"error": "provide a message or resume: true"}), 400)
            return self._send(*self.scheduler.create_turn(
                parts[1], body.get("request_id"), None if resume else body["message"]))
        if parts == ["sessions"]:
            body = self._body()
            if body is None:
                return
            return self._send(*self.scheduler.create_session(body.get("id")))
        if parts == ["schedules"]:
            body = self._body()
            if body is None:
                return
            if set(body) - {"graph", "rule", "input"} or not body.get("graph") or \
                    not isinstance(body.get("rule"), dict) or not isinstance(body.get("input", {}), dict):
                return self._send(json.dumps({"error": "provide graph, rule, and optional object input"}), 400)
            return self._send(*self.scheduler.create_schedule(str(body["graph"]), body["rule"],
                                                              body.get("input", {})))
        if len(parts) == 3 and parts[0] == "sessions" and parts[2] == "status":
            body = self._body()
            if body is None:
                return
            return self._send(*self.scheduler.set_session_status(
                parts[1], str(body.get("status") or ""), str(body.get("reason") or "")))
        if len(parts) == 3 and parts[0] == "sessions" and parts[2] == "confirm":
            body = self._body()
            if body is None:
                return
            return self._send(*self.scheduler.confirm_session(
                parts[1], str(body.get("action") or ""),
                str(body.get("approval_key") or "")))
        if len(parts) == 3 and parts[0] == "sessions" and parts[2] == "reject":
            body = self._body()
            if body is None:
                return
            return self._send(*self.scheduler.reject_session(
                parts[1], str(body.get("action") or ""),
                str(body.get("approval_key") or "")))
        if len(parts) == 3 and parts[0] == "sessions" and parts[2] == "messages":
            body = self._body()
            if body is None:
                return
            prompt = body.get("message")
            if not isinstance(prompt, str):
                return self._send(json.dumps({"error": "message must be a string"}), 400)
            return self._send(*self.scheduler.pilot_message(parts[1], prompt))
        if len(parts) == 3 and parts[0] == "sessions" and parts[2] == "resume":
            return self._send(*self.scheduler.pilot_message(parts[1], None))
        if len(parts) == 3 and parts[0] == "sessions" and parts[2] == "stop":
            return self._send(*self.scheduler.stop_pilot(parts[1]))
        if len(parts) == 3 and parts[0] == "sessions" and parts[2] == "runs":
            body = self._body()
            if body is None:
                return
            run_id = str(body.get("run") or "")
            if not run_id:
                return self._send(json.dumps({"error": "run is required"}), 400)
            return self._send(*self.scheduler.attach_session_run(parts[1], run_id))
        if parts == ["graphs"]:
            body = self._body()
            if body is None:
                return
            response, status = self.scheduler.create(str(body.get("name") or ""),
                                                     body.get("definition"))
            return self._send(response, status)
        if len(parts) == 3 and parts[0] == "runs":
            # Every verb under a run goes to `control_run`, which names the ones it knows. Matching
            # the known ones here instead would answer "not found" for a typo, which reads as "no such
            # run" rather than "no such request".
            response, status = self.scheduler.control_run(parts[1], parts[2])
            return self._send(response, status)
        if len(parts) == 4 and parts[:3] == ["v1", "webhooks", "graphs"]:
            body = self._body()
            if body is None:
                return
            if set(body) - {"input"} or not isinstance(body.get("input", {}), dict):
                return self._send(json.dumps({"error": "body must contain only an object input"}), 400)
            response, status = self.scheduler.trigger(parts[3], None, body.get("input", {}),
                                                      {"source": "webhook"})
            return self._send(response, status)
        if parts != ["trigger"]:
            return self._send(json.dumps({"error": "not found"}), 404)
        body = self._body()
        if body is None:
            return
        if not body.get("graph"):
            return self._send(json.dumps({"error": "graph is required"}), 400)
        if body.get("input") is not None and not isinstance(body.get("input"), dict):
            return self._send(json.dumps({"error": "input must be an object"}), 400)
        response, status = self.scheduler.trigger(str(body["graph"]), body.get("objective"),
                                                  body.get("input"))
        self._send(response, status)

    def do_DELETE(self) -> None:
        if not self._authorized():
            return
        parts = [part for part in PurePosixPath(unquote(urlparse(self.path).path)).parts
                 if part != "/"]
        if len(parts) == 2 and parts[0] == "schedules":
            return self._send(*self.scheduler.delete_schedule(parts[1]))
        if len(parts) == 2 and parts[0] == "sessions":
            return self._send(*self.scheduler.delete_session(parts[1]))
        if len(parts) != 2 or parts[0] not in ("runs", "graphs"):
            return self._send(json.dumps({"error": "not found"}), 404)
        response, status = (self.scheduler.delete_run(parts[1]) if parts[0] == "runs"
                            else self.scheduler.delete_graph(parts[1]))
        self._send(response, status)


def _graph_of(scheduler: Scheduler, run_id: str) -> str:
    for item in scheduler.runs():
        if item["run"] == run_id:
            return str(item["graph"])
    return ""


def serve(root: str | Path, config: str | Path, host: str = "127.0.0.1", port: int = 8077) -> None:
    scheduler = Scheduler(Path(root).expanduser().resolve(), Path(config).resolve())
    try:
        loopback = ipaddress.ip_address(host).is_loopback
    except ValueError:
        loopback = host.lower() == "localhost"
    if not loopback and not scheduler.api_keys:
        raise ValueError("ANCHOR_API_KEYS is required when listening on a non-loopback address")
    Handler.scheduler = scheduler
    server = ThreadingHTTPServer((host, port), Handler)
    (Path(root).expanduser() / "workspaces").mkdir(parents=True, exist_ok=True)
    print(json.dumps({"listening": f"http://{host}:{port}", "root": str(scheduler.root),
                      "graphs": [item.name for item in scheduler.workspaces()]}), flush=True)
    callback_host = "127.0.0.1" if host in {"0.0.0.0", "::"} else host
    scheduler.start_channels(f"http://{callback_host}:{server.server_port}/v1/channels/wecom/events")
    scheduler.resume_all()
    def schedule_loop() -> None:
        while True:
            scheduler.tick_schedules()
            time.sleep(1)
    threading.Thread(target=schedule_loop, daemon=True).start()
    try:
        server.serve_forever()
    finally:
        scheduler.stop_channels()
        server.server_close()
