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

The state is files. There is no database, and nothing here reads anything but the directory tree.
"""

from __future__ import annotations

import json
import threading
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path, PurePosixPath
from urllib.parse import unquote, urlparse

from anchor.simple import graph as graph_module
from anchor.simple import run as runner

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
        self.running: dict[str, str] = {}         # graph -> run id
        # What a run has been asked to do next, by run id: "paused" or "stopped". Asked between
        # nodes, because a node in flight is inside a sandbox command or a model call and nothing
        # here can reach into it — so the request lands when the running node finishes.
        self.control: dict[str, str] = {}
        self.lock = threading.Lock()

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

        Pause and stop are the same request — the loop leaves off between nodes — and differ only in
        the status the run keeps, which is what decides whether it is picked up again on restart.
        Neither cancels the node that is running: nothing outside a sandbox command can stop it, and
        a button that appeared to would be lying.
        """
        if what not in ("pause", "stop", "resume"):
            return json.dumps({"error": f"unknown control: {what}"}), 400
        graph = next((name for name, current in self.running.items() if current == run_id), None)
        with self.lock:
            if what == "resume":
                self.control.pop(run_id, None)
            else:
                self.control[run_id] = "paused" if what == "pause" else "stopped"
                # The graph stays claimed until the loop actually leaves, which is not now: the node
                # in flight has to finish first. Releasing it here would let a second trigger start
                # while the first is still inside a node — and would make "is it still running" lie
                # to whoever is watching for it to stop.
        if graph is None:
            return self._resume_cold(run_id) if what == "resume" else (
                json.dumps({"error": "that run is not running", "run": run_id}), 409)
        return json.dumps({"run": run_id, "asked": what}), 202

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
            graph_module.parse(definition)
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
                       stop_request=asked)
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
            traces[trace.name.removesuffix(".trace.jsonl")] = [_readable(item) for item in lines]
        return {"graph": workspace.name, "run": run_id, "state": state, "traces": traces,
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


def _readable(line: str) -> dict:
    """A message as something a person can read, without knowing the library's shape."""
    message = json.loads(line)
    content = message.get("content")
    if isinstance(content, list):
        content = " | ".join(str(part.get("text", part)) for part in content)
    calls = message.get("tool_calls") or []
    return {"role": message.get("role"), "text": str(content or "")[:2000],
            "tools": [item.get("function", {}).get("name") for item in calls],
            "exit_status": (message.get("extra") or {}).get("exit_status")}


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

    def do_GET(self) -> None:
        path = PurePosixPath(unquote(urlparse(self.path).path))
        parts = [part for part in path.parts if part != "/"]
        if not parts or parts[0] == "assets" or (len(parts) == 1 and "." in parts[0]):
            if self._serve_built(parts):
                return
        if parts == ["graphs"]:
            names = [{"graph": item.name, "running": self.scheduler.running.get(item.name)}
                     for item in self.scheduler.workspaces()]
            return self._send(json.dumps({"graphs": names}, ensure_ascii=False))
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

    def do_POST(self) -> None:
        parts = [part for part in PurePosixPath(unquote(urlparse(self.path).path)).parts if part != "/"]
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
