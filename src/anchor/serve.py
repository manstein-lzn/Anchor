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
import shutil
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import parse_qs, quote, unquote, urlparse

from anchor.simple import graph as graph_module
from anchor.simple import run as runner
from anchor.library import Library
from anchor.session import SessionStore
from anchor.pilot_turns import TurnStore


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
        # A pause lands between nodes; a stop also cancels the current model call or command.
        self.control: dict[str, str] = {}
        self.lock = threading.Lock()
        for session_id in self.turns.interrupt_running():
            try:
                self.sessions.set_status(session_id, "interrupted", reason="服务重启，未自动重放上次执行")
            except (KeyError, ValueError):
                pass

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

    def trigger(self, graph: str, objective: str | None) -> tuple[str, int]:
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
            if graph in self.running:
                # Refused, not queued: whoever asked should be able to tell that this run did not
                # start, and one graph doing two things at once is not something it can be asked to
                # keep straight.
                return json.dumps({"error": "this graph is already running",
                                   "running": self.running[graph]}), 409
            run_id = _stamp()
            self.running[graph] = run_id
        threading.Thread(target=self._run, args=(workspace, run_id, objective),
                         daemon=True).start()
        return json.dumps({"run": run_id, "graph": graph}), 202

    def control_run(self, run_id: str, what: str) -> tuple[str, int]:
        """Ask a run to pause, stop or carry on. Returns (body, status).

        A pause leaves off between nodes. A stop cancels the current node and is terminal.
        """
        if what not in ("pause", "stop", "resume"):
            return json.dumps({"error": f"unknown control: {what}"}), 400
        graph = next((name for name, current in self.running.items() if current == run_id), None)
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

    def delete_run(self, run_id: str) -> tuple[str, int]:
        """Remove a run and everything it left behind, unless it is still running."""
        if not run_id or "/" in run_id or run_id in (".", ".."):
            return json.dumps({"error": "no such run"}), 404
        with self.lock:
            if run_id in self.running.values():
                return json.dumps({"error": "that run is still running", "run": run_id}), 409
            run_dir = self.run_dir(run_id)
            if run_dir is None:
                return json.dumps({"error": "no such run"}), 404
            shutil.rmtree(run_dir)
        return json.dumps({"run": run_id, "deleted": True}), 200

    def delete_graph(self, name: str) -> tuple[str, int]:
        """Remove a graph workspace, including its runs, unless it is still running."""
        with self.lock:
            if self.running.get(name):
                return json.dumps({"error": "that graph is still running",
                                   "running": self.running[name]}), 409
            workspace = self.workspace(name)
            if workspace is None:
                return json.dumps({"error": f"no such graph: {name}"}), 404
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
        return json.dumps({"sessions": [item.model_dump(mode="json")
                                         for item in self.sessions.list()]},
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

    def pilot_messages(self, session_id: str) -> tuple[str, int]:
        from anchor.pilot import history
        try:
            session = self.sessions.get(session_id)
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

    def create_turn(self, session_id: str, request_id: str, prompt: str | None) -> tuple[str, int]:
        from pydantic_ai import CancellationToken

        if not isinstance(request_id, str) or not request_id.strip() or len(request_id) > 200:
            return json.dumps({"error": "request_id must contain 1 to 200 characters"}), 400
        if prompt is not None and (not isinstance(prompt, str) or not prompt.strip() or len(prompt) > 100_000):
            return json.dumps({"error": "message must contain 1 to 100000 characters"}), 400
        with self.lock:
            try:
                session = self.sessions.get(session_id)
                existing = self.turns.find_request(session_id, request_id)
                if existing:
                    if existing["prompt"] != prompt:
                        raise ValueError("request_id was already used for different input")
                    return json.dumps({"turn": existing}, ensure_ascii=False), 202
                if session_id in self.pilot_active:
                    raise ValueError("that session is already processing a message")
                if session.status not in {"active", "waiting_user"} and not (
                        session.status == "interrupted" and prompt is None):
                    raise ValueError(f"session is {session.status}")
                if any(item.get("status") == "requested" for item in session.approvals):
                    raise ValueError("confirm or reject the pending operation first")
                if prompt is not None and any(item.get("status") in {"approved", "rejected"}
                                              for item in session.approvals):
                    raise ValueError("resume the confirmed operation first")
                if self.turns.unsafe_to_retry(session_id):
                    raise ValueError("上次执行已进入有副作用的工具；请先核查执行结果，当前阶段禁止自动重放。")
                turn, _ = self.turns.create(session_id, request_id, prompt)
                self.pilot_active.add(session_id)
                self.pilot_tokens[session_id] = CancellationToken()
            except KeyError:
                return json.dumps({"error": "no such session"}), 404
            except ValueError as exc:
                return json.dumps({"error": str(exc)}, ensure_ascii=False), 409
            threading.Thread(target=self._run_turn, args=(turn,), daemon=True).start()
        return json.dumps({"turn": turn}, ensure_ascii=False), 202

    def _run_turn(self, turn: dict) -> None:
        try:
            body, code = self.pilot_message(turn["session"], turn["prompt"], turn_id=turn["id"])
            response = json.loads(body)
            status = ("waiting_approval" if response.get("approvals") else
                      "waiting_user" if response.get("paused") else
                      "completed" if code == 200 else
                      "stopped" if response.get("stopped") else "failed")
            self.turns.finish(turn["id"], status, response.get("error", ""))
        finally:
            with self.lock:
                self.pilot_active.discard(turn["session"])
                self.pilot_tokens.pop(turn["session"], None)

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
            if self.turns.unsafe_to_retry(session_id):
                return json.dumps({"error": "上次工具副作用需要核查，不能自动重放。"}, ensure_ascii=False), 409
            decisions = {} if prompt is not None else self.sessions.approval_decisions(session_id)
            question = self.sessions.pending_question(session_id) if prompt is not None else None
            if prompt is not None and session.status == "waiting_user":
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
            with self.lock:
                if workspace.name in self.running:
                    return json.dumps({"error": "this graph is already running",
                                       "running": self.running[workspace.name]}), 409
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
        if self.running.get(name):
            return json.dumps({"error": "this graph is running; changing it now would change what "
                                       "the run reads", "running": self.running[name]}), 409
        try:
            parsed = graph_module.parse(definition)
            for node in parsed.nodes.values():
                self.library.attach(node.plugins)
        except Exception as exc:  # noqa: BLE001 - the message is the point
            return json.dumps({"error": f"{type(exc).__name__}: {exc}"}), 400
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
        """Pick up anything a previous process left running. The whole of recovery."""
        for workspace in self.workspaces():
            runs = sorted((workspace / "runs").glob("*/run.json")) if (workspace / "runs").is_dir() else []
            for state_file in runs:
                state = json.loads(state_file.read_text(encoding="utf-8"))
                if state.get("status") != "running":
                    continue
                run_id = state_file.parent.name
                with self.lock:
                    if workspace.name in self.running:
                        continue
                    self.running[workspace.name] = run_id
                print(json.dumps({"resume": run_id, "graph": workspace.name}), flush=True)
                threading.Thread(target=self._run,
                                 args=(workspace, run_id, state.get("objective"), True),
                                 daemon=True).start()

    def _run(self, workspace: Path, run_id: str, objective: str | None,
             resume: bool = False) -> None:
        def asked() -> str | None:
            with self.lock:
                return self.control.get(run_id)

        try:
            runner.run(workspace, objective=objective, config_path=self.config, run_id=run_id,
                       resume=(workspace / "runs" / run_id) if resume else None,
                       stop_request=asked, library_root=self.library.root)
        except Exception:  # noqa: BLE001 - the run already recorded its own failure
            traceback.print_exc()
        finally:
            with self.lock:
                self.running.pop(workspace.name, None)
                self.control.pop(run_id, None)

    def runs(self) -> list[dict]:
        found = []
        for workspace in self.workspaces():
            base = workspace / "runs"
            for state_file in sorted(base.glob("*/run.json"), reverse=True) if base.is_dir() else []:
                state = json.loads(state_file.read_text(encoding="utf-8"))
                found.append({"run": state_file.parent.name, "graph": workspace.name,
                              "status": state.get("status"),
                              "running": self.running.get(workspace.name) == state_file.parent.name,
                              "started": state.get("started"), "updated": state.get("updated"),
                              "executed": state.get("executed", []),
                              "objective": (state.get("objective") or "")[:200]})
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
            return json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError:
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

    def _send_file(self, path: Path) -> None:
        """The bytes as they are, and as a download rather than something a browser renders.

        `attachment` and `application/octet-stream` on purpose: a node's workspace holds whatever the
        node wrote, and a browser that rendered it would be rendering text this program did not write
        inside a page this program does serve. A download has no such question.
        """
        payload = path.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
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

    def do_GET(self) -> None:  # noqa: C901 - one small HTTP router keeps endpoint behavior visible
        parsed = urlparse(self.path)
        query = parse_qs(parsed.query)
        path = PurePosixPath(unquote(parsed.path))
        parts = [part for part in path.parts if part != "/"]
        if not parts or parts[0] == "assets" or (len(parts) == 1 and "." in parts[0]):
            if self._serve_built(parts):
                return
        if parts == ["graphs"]:
            names = [{"graph": item.name, "running": self.scheduler.running.get(item.name)}
                     for item in self.scheduler.workspaces()]
            return self._send(json.dumps({"graphs": names}, ensure_ascii=False))
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
        parts = [part for part in PurePosixPath(unquote(urlparse(self.path).path)).parts
                 if part != "/"]
        if len(parts) != 2 or parts[0] != "graphs":
            return self._send(json.dumps({"error": "not found"}), 404)
        body = self._body()
        if body is None:
            return
        response, status = self.scheduler.save(parts[1], body.get("definition") or {})
        self._send(response, status)

    def do_POST(self) -> None:  # noqa: C901 - one small HTTP router keeps endpoint behavior visible
        parts = [part for part in PurePosixPath(unquote(urlparse(self.path).path)).parts if part != "/"]
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
        if parts != ["trigger"]:
            return self._send(json.dumps({"error": "not found"}), 404)
        body = self._body()
        if body is None:
            return
        if not body.get("graph"):
            return self._send(json.dumps({"error": "graph is required"}), 400)
        response, status = self.scheduler.trigger(str(body["graph"]), body.get("objective"))
        self._send(response, status)

    def do_DELETE(self) -> None:
        parts = [part for part in PurePosixPath(unquote(urlparse(self.path).path)).parts
                 if part != "/"]
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
    Handler.scheduler = scheduler
    server = ThreadingHTTPServer((host, port), Handler)
    (Path(root).expanduser() / "workspaces").mkdir(parents=True, exist_ok=True)
    print(json.dumps({"listening": f"http://{host}:{port}", "root": str(scheduler.root),
                      "graphs": [item.name for item in scheduler.workspaces()]}), flush=True)
    scheduler.resume_all()
    server.serve_forever()
