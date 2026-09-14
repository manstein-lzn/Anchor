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
        self.lock = threading.Lock()

    def workspaces(self) -> list[Path]:
        base = self.root / "workspaces"
        return sorted(item for item in base.iterdir()
                      if item.is_dir() and (item / "graph.json").is_file()) if base.is_dir() else []

    def workspace(self, name: str) -> Path | None:
        return next((item for item in self.workspaces() if item.name == name), None)

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
        try:
            runner.run(workspace, objective=objective, config_path=self.config, run_id=run_id,
                       resume=(workspace / "runs" / run_id) if resume else None)
        except Exception:  # noqa: BLE001 - the run already recorded its own failure
            traceback.print_exc()
        finally:
            with self.lock:
                self.running.pop(workspace.name, None)

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

    def do_POST(self) -> None:
        parts = [part for part in PurePosixPath(unquote(urlparse(self.path).path)).parts if part != "/"]
        if parts != ["trigger"]:
            return self._send(json.dumps({"error": "not found"}), 404)
        length = int(self.headers.get("Content-Length") or 0)
        try:
            body = json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError:
            return self._send(json.dumps({"error": "body must be JSON"}), 400)
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
