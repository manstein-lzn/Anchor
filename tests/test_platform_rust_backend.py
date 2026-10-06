"""The original platform delegates execution to HTTP without owning a second Run."""

from contextlib import contextmanager
from datetime import datetime, timedelta
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import threading
from urllib.parse import unquote, urlsplit

import pytest

from anchor.runtime_http import RuntimeHTTPClient, RuntimeHTTPError
from anchor.serve import Handler, Scheduler


@contextmanager
def running_server(handler):
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    worker = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True)
    worker.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=2)


class FakeRuntime:
    def __init__(self):
        self.definition = {"entry": "work", "objective": "Remote definition", "ops": {"work": {"run": "true"}},
                           "nodes": [{"id": "work", "op": "work"}], "edges": []}
        self.graphs = {"demo": self.definition}
        self.runs = {}
        self.files = {}
        self.schedules = []
        self.requests = []
        self.unavailable = False
        self.overrides = {}
        self.next_id = 0

    def handler(self):
        runtime = self

        class Upstream(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def handle_api(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                body = json.loads(body) if body else None
                parsed = urlsplit(self.path)
                runtime.requests.append({"method": self.command, "path": self.path, "body": body,
                                         "authorization": self.headers.get("Authorization")})
                status, value = runtime.dispatch(self.command, parsed, body)
                payload = value if isinstance(value, bytes) else json.dumps(value).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/octet-stream" if isinstance(value, bytes)
                                 else "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            do_GET = do_POST = do_PUT = do_DELETE = handle_api

        return Upstream

    def dispatch(self, method, parsed, body):  # noqa: C901 - one fake HTTP application
        if self.unavailable:
            return 503, {"error": "fixture runtime unavailable"}
        if (method, parsed.path) in self.overrides:
            return self.overrides[(method, parsed.path)]
        parts = [unquote(part) for part in parsed.path.split("/") if part]
        if parts == ["schedules"] and method == "GET":
            return 200, {"schedules": self.schedules}
        if parts == ["schedules"] and method == "POST":
            schedule = {"id": f"schedule-{len(self.schedules) + 1}", "graph": body["graph"],
                        "rule": body["rule"], "input": body.get("input", {}),
                        "created_at": datetime.now().replace(microsecond=0).isoformat(),
                        "next_at": body["rule"].get("at", "2026-10-07T09:00:00"), "enabled": True}
            self.schedules.append(schedule)
            return 201, {"schedule": schedule}
        if len(parts) == 2 and parts[0] == "schedules" and method == "DELETE":
            for schedule in self.schedules:
                if schedule["id"] == parts[1]:
                    self.schedules.remove(schedule)
                    return 200, {"schedule": parts[1], "deleted": True}
            return 404, {"error": "no such schedule"}
        if parts == ["timeline"] and method == "GET":
            return 200, {"from": "2026-10-01T00:00:00", "to": "2026-10-07T00:00:00",
                         "runs": [], "scheduled": [], "schedules": self.schedules,
                         "capabilities": {"scheduling": True}}
        if parts == ["plugins"] and method == "GET":
            return 200, {"plugins": [{"id": "sample", "name": "Sample", "description": "Existing Plugin",
                                       "skills": ["skills/sample/SKILL.md"], "mcpServers": {},
                                       "available": True, "digest": "fixture"}]}
        if len(parts) == 2 and parts[0] == "plugins" and method == "GET":
            return 200, {"id": parts[1], "name": "Sample", "description": "Existing Plugin",
                         "skills": ["skills/sample/SKILL.md"], "mcpServers": {},
                         "available": True, "digest": "fixture", "instructions": "# Existing skill\n"}
        if len(parts) >= 4 and parts[0] == "plugins" and parts[2] == "files" and method == "GET":
            payload = f"plugin-file:{'/'.join(parts[3:])}".encode()
            return 200, payload
        if parts == ["graph-validation"] and method == "POST":
            return 200, {"valid": True, "entry": body["definition"].get("entry"),
                         "nodes": [node["id"] for node in body["definition"].get("nodes", [])]}
        if parts == ["graphs"] and method == "GET":
            graphs = []
            for graph in self.graphs:
                active = [run for run, record in self.runs.items()
                          if record["graph"] == graph and record["active"]]
                graphs.append({"graph": graph, "running": next(iter(active), None), "active_runs": active})
            return 200, {"graphs": graphs}
        if parts == ["graphs"] and method == "POST":
            self.graphs[body["name"]] = body["definition"]
            return 201, {"graph": body["name"], "definition": body["definition"]}
        if len(parts) == 2 and parts[0] == "graphs":
            name = parts[1]
            if name not in self.graphs:
                return 404, {"error": "no such graph"}
            if method == "GET":
                return 200, {"graph": name, "definition": self.graphs[name]}
            if method == "PUT":
                self.graphs[name] = body["definition"]
                return 200, {"graph": name, "definition": self.graphs[name]}
            if method == "DELETE":
                del self.graphs[name]
                return 204, b""
        if parts == ["trigger"] and method == "POST":
            if body["graph"] not in self.graphs:
                return 404, {"error": "no such graph"}
            if any(record["graph"] == body["graph"] and record["active"] for record in self.runs.values()):
                return 409, {"error": "this graph is already running"}
            self.next_id += 1
            run = f"opaque-{self.next_id}"
            now = datetime.now().isoformat()
            self.runs[run] = {"run": run, "graph": body["graph"], "active": True,
                              "state": {"status": "running", "objective": body["objective"],
                                        "input": body["input"], "trigger": body["trigger"],
                                        "started": now, "updated": now, "nodes": {}, "executed": []},
                              "nodes": ["module/work"], "traces": {}, "calls": []}
            return 202, {"run": run, "graph": body["graph"]}
        if parts == ["runs"] and method == "GET":
            return 200, {"runs": [{**record["state"], "run": run, "graph": record["graph"],
                                    "running": record["active"]} for run, record in self.runs.items()]}
        if len(parts) >= 2 and parts[0] == "runs":
            run = parts[1]
            if run not in self.runs:
                return 404, {"error": "no such run"}
            record = self.runs[run]
            if len(parts) == 2:
                if method == "DELETE":
                    del self.runs[run]
                    return 204, b""
                return 200, record
            if len(parts) == 3 and parts[2] in {"pause", "resume", "stop"} and method == "POST":
                record["state"]["status"] = {"pause": "paused", "resume": "running", "stop": "stopped"}[parts[2]]
                record["active"] = parts[2] == "resume"
                return 202, {"run": run, "asked": parts[2]}
            if len(parts) >= 4 and parts[2] == "files":
                node = parts[3]
                if len(parts) == 4:
                    return 200, {"node": node, "files": [{"path": path, "size": len(data)}
                                                         for (identifier, name, path), data in self.files.items()
                                                         if identifier == run and name == node], "truncated": False}
                name = "/".join(parts[4:])
                payload = self.files.get((run, node, name))
                if payload is None:
                    return 404, {"error": "no such file"}
                if parsed.query == "download=1":
                    return 200, payload
                try:
                    text, binary = payload.decode(), False
                except UnicodeDecodeError:
                    text, binary = "", True
                return 200, {"path": name, "size": len(payload), "text": text, "binary": binary, "truncated": False}
        return 404, {"error": "not found"}


@pytest.fixture
def platform(tmp_path, monkeypatch):
    monkeypatch.delenv("ANCHOR_API_KEYS", raising=False)
    monkeypatch.setenv("ANCHOR_RUNTIME_BACKEND", "rust")
    monkeypatch.setenv("ANCHOR_RUNTIME_API_KEY", "upstream-fixture-key")
    monkeypatch.setenv("ANCHOR_RUNTIME_TIMEOUT_SECONDS", "2")
    runtime = FakeRuntime()
    with running_server(runtime.handler()) as upstream:
        monkeypatch.setenv("ANCHOR_RUNTIME_URL", f"http://127.0.0.1:{upstream.server_port}")
        scheduler = Scheduler(tmp_path, tmp_path / "config.json")
        forbidden = []

        def no_runner(*args, **kwargs):
            forbidden.append((args, kwargs))
            raise AssertionError("Python Runner must not execute in Rust backend mode")

        monkeypatch.setattr("anchor.serve.runner.run", no_runner)
        handler = type("PlatformHandler", (Handler,), {"scheduler": scheduler})
        with running_server(handler) as frontend:
            yield scheduler, runtime, frontend, forbidden
        assert not forbidden


def request(server, method, path, body=None):
    client = HTTPConnection(*server.server_address, timeout=3)
    try:
        client.request(method, path, json.dumps(body) if body is not None else None,
                       {"Content-Type": "application/json"})
        response = client.getresponse()
        payload = response.read()
        headers = dict(response.getheaders())
        value = json.loads(payload) if headers.get("Content-Type", "").startswith("application/json") else payload
        return response.status, value, headers
    finally:
        client.close()


def start(platform):
    _scheduler, _runtime, frontend, _forbidden = platform
    status, accepted, _headers = request(frontend, "POST", "/trigger",
                                         {"graph": "demo", "objective": "Requested objective", "input": {"x": 1}})
    assert status == 202, accepted
    return accepted["run"]


def legacy_run(root, run="rust-looking-legacy", graph="old", status="running"):
    directory = root / "workspaces" / graph / "runs" / run
    (directory / "work").mkdir(parents=True)
    (directory / "run.json").write_text(json.dumps({"status": status, "started": "2020-01-01T00:00:00",
                                                    "objective": "legacy", "runs": {}, "trigger": {"source": "manual"}}))
    (directory / "work" / "old.txt").write_text("legacy file")
    (directory.parent.parent / "graph.json").write_text('{"objective":"Legacy graph"}')
    return directory


def test_original_graph_and_run_routes_use_remote_facts_without_local_graph_copy(platform):
    scheduler, runtime, frontend, _forbidden = platform
    assert request(frontend, "GET", "/graphs")[1]["graphs"][0]["graph"] == "demo"
    assert request(frontend, "GET", "/graphs/demo")[1]["definition"] == runtime.definition
    definition = {**runtime.definition, "objective": "Created remotely"}
    assert request(frontend, "POST", "/graphs", {"name": "new", "definition": definition})[0] == 201
    assert runtime.graphs["new"] == definition
    assert request(frontend, "PUT", "/graphs/new", {"definition": runtime.definition})[0] == 200
    assert request(frontend, "DELETE", "/graphs/new")[1] == {"graph": "new", "deleted": True}
    assert not (scheduler.root / "workspaces").exists()

    run = start(platform)
    assert not run.startswith("rust-")
    assert scheduler.active_run("demo") == run
    assert scheduler.running == scheduler.control == {}
    runtime.runs[run]["control_requested"] = "stop"
    detail = request(frontend, "GET", f"/runs/{run}")[1]
    assert detail["control_requested"] == "stop"
    assert detail["state"]["objective"] == "Requested objective"
    assert detail["state"]["input"] == {"x": 1}
    assert detail["state"]["trigger"] == {"source": "manual"}
    assert request(frontend, "POST", "/trigger", {"graph": "demo"})[0] == 409
    for operation in ["pause", "resume", "stop"]:
        assert request(frontend, "POST", f"/runs/{run}/{operation}")[0] == 202
    assert scheduler.running == scheduler.control == {}
    assert request(frontend, "GET", "/runs")[1]["runs"][0]["status"] == "stopped"

    reopened = Scheduler(scheduler.root, scheduler.config)
    reopened.resume_all()
    assert reopened.control_run(run, "resume")[1] == 202
    assert reopened.run("", run)["state"]["status"] == "running"
    assert reopened.run_dir(run) is None
    assert all(item["authorization"] == "Bearer upstream-fixture-key" for item in runtime.requests)
    assert request(frontend, "DELETE", f"/runs/{run}")[1] == {"run": run, "deleted": True}


def test_remote_file_preview_and_stream_download_support_module_ids_and_passive_images(platform):
    scheduler, runtime, frontend, _forbidden = platform
    run = start(platform)
    blob = bytes(range(256)) * 1000
    runtime.files[(run, "module/work", "report.md")] = b"# Remote report\n"
    runtime.files[(run, "module/work", "large.bin")] = blob
    runtime.files[(run, "module/work", "image.svg")] = b'<svg xmlns="http://www.w3.org/2000/svg"/>'
    status, listing, _headers = request(frontend, "GET", f"/runs/{run}/files/module%2Fwork")
    assert status == 200 and len(listing["files"]) == 3
    assert request(frontend, "GET", f"/runs/{run}/files/module%2Fwork/report.md")[1]["text"] == "# Remote report\n"
    assert request(frontend, "GET", f"/runs/{run}/files/module%2Fwork/large.bin")[1]["binary"] is True
    status, downloaded, headers = request(frontend, "GET", f"/runs/{run}/files/module%2Fwork/large.bin?download=1")
    assert status == 200 and downloaded == blob and headers["Content-Length"] == str(len(blob))
    status, downloaded, headers = request(frontend, "GET", f"/runs/{run}/files/module%2Fwork/image.svg?download=1")
    assert status == 200 and downloaded.startswith(b"<svg")
    assert headers["Content-Type"] == "image/svg+xml" and headers["X-Content-Type-Options"] == "nosniff"
    assert headers["Content-Disposition"].startswith("attachment;") and "sandbox" in headers["Content-Security-Policy"]
    assert scheduler.locate(run, "module/work", "report.md") is None
    assert request(frontend, "GET", f"/runs/{run}/files/module%2Fwork/%2E%2E/run.json")[0] == 400


def test_rust_backend_proxies_schedules_and_timeline_without_python_writes(platform):
    scheduler, runtime, frontend, _forbidden = platform
    due = datetime.now().replace(microsecond=0) + timedelta(seconds=5)
    status, value, _headers = request(frontend, "POST", "/schedules", {
        "graph": "demo", "rule": {"type": "once", "at": due.isoformat()}, "input": {"scheduled": True},
    })
    assert status == 201
    schedule = value["schedule"]
    assert schedule["input"] == {"scheduled": True}
    assert request(frontend, "GET", "/schedules")[1]["schedules"] == [schedule]
    status, timeline, _headers = request(frontend, "GET", "/timeline?days=7&before=2026-10-07")
    assert status == 200 and timeline["capabilities"]["scheduling"] is True
    assert timeline["schedules"] == [schedule]
    assert request(frontend, "DELETE", f"/schedules/{schedule['id']}")[1] == {
        "schedule": schedule["id"], "deleted": True,
    }
    assert runtime.schedules == []
    assert scheduler.schedules == []
    assert not scheduler.schedule_path.exists()
    scheduler.tick_schedules(due)
    assert runtime.runs == {}
    assert not (scheduler.root / "workspaces").exists()


def test_rust_backend_schedule_unavailability_does_not_fall_back_to_python(platform):
    scheduler, runtime, frontend, _forbidden = platform
    due = datetime.now().replace(microsecond=0) + timedelta(minutes=1)
    runtime.unavailable = True
    body = {"graph": "demo", "rule": {"type": "once", "at": due.isoformat()}}
    assert request(frontend, "POST", "/schedules", body)[0] == 503
    assert request(frontend, "GET", "/schedules")[0] == 503
    assert request(frontend, "GET", "/timeline")[0] == 503
    assert request(frontend, "DELETE", "/schedules/absent")[0] == 503
    assert scheduler.schedules == [] and not scheduler.schedule_path.exists()
    assert not _forbidden


def test_legacy_history_is_readonly_by_store_presence_not_run_prefix(platform):
    scheduler, runtime, frontend, _forbidden = platform
    directory = legacy_run(scheduler.root)
    graph_before = (directory.parent.parent / "graph.json").read_bytes()
    before = (directory / "run.json").read_bytes()
    scheduler.resume_all()
    scheduler._recover_admissions()
    identifier = directory.name
    history = request(frontend, "GET", "/runs")[1]["runs"][0]
    assert history["backend"] == "legacy" and history["read_only"] and not history["running"]
    detail = request(frontend, "GET", f"/runs/{identifier}")[1]
    assert detail["read_only"] and detail["state"]["status"] == "running"
    assert request(frontend, "GET", f"/runs/{identifier}/files/work/old.txt")[1]["text"] == "legacy file"
    assert request(frontend, "GET", f"/runs/{identifier}/files/work/old.txt?download=1")[1] == b"legacy file"
    for operation in ["resume", "stop", "pause"]:
        assert request(frontend, "POST", f"/runs/{identifier}/{operation}")[0] == 409
    assert request(frontend, "DELETE", f"/runs/{identifier}")[0] == 409
    assert (directory / "run.json").read_bytes() == before
    assert (directory.parent.parent / "graph.json").read_bytes() == graph_before
    assert not runtime.runs


def test_duplicate_run_ownership_refuses_reads_and_controls_without_mutating_either_store(platform):
    scheduler, runtime, frontend, _forbidden = platform
    run = start(platform)
    directory = legacy_run(scheduler.root, run=run)
    before = (directory / "run.json").read_bytes()
    for method, path in [("GET", "/runs"), ("GET", f"/runs/{run}"),
                         ("GET", f"/runs/{run}/files/work"), ("POST", f"/runs/{run}/resume"),
                         ("DELETE", f"/runs/{run}")]:
        assert request(frontend, method, path)[0] == 409
    assert (directory / "run.json").read_bytes() == before
    assert runtime.runs[run]["state"]["status"] == "running"
    assert not any(item["path"].endswith("/resume") for item in runtime.requests)


def test_duplicate_legacy_run_ids_refuse_detail_and_files_without_changing_history(platform):
    scheduler, runtime, frontend, _forbidden = platform
    run = "duplicate-legacy"
    directories = [legacy_run(scheduler.root, run=run, graph=graph) for graph in ["old-a", "old-b"]]
    before = [(directory / "run.json").read_bytes() for directory in directories]
    for method, path in [("GET", "/runs"), ("GET", f"/runs/{run}"),
                         ("GET", f"/runs/{run}/files/work"), ("POST", f"/runs/{run}/resume"),
                         ("DELETE", f"/runs/{run}")]:
        assert request(frontend, method, path)[0] == 409
    assert [(directory / "run.json").read_bytes() for directory in directories] == before
    assert not runtime.runs
    assert not any(item["path"].endswith("/resume") for item in runtime.requests)


def test_backend_errors_never_start_local_runner_or_mutate_legacy_history(platform):
    scheduler, runtime, frontend, _forbidden = platform
    directory = legacy_run(scheduler.root)
    before = (directory / "run.json").read_bytes()
    runtime.unavailable = True
    for method, path, body in [
        ("POST", "/trigger", {"graph": "old"}),
        ("GET", "/graphs", None), ("GET", "/runs", None),
        ("GET", f"/runs/{directory.name}", None),
        ("POST", f"/runs/{directory.name}/resume", None),
        ("PUT", "/graphs/old", {"definition": runtime.definition}),
        ("GET", f"/runs/{directory.name}/files/work/old.txt?download=1", None),
    ]:
        assert request(frontend, method, path, body)[0] == 503
    assert (directory / "run.json").read_bytes() == before
    assert scheduler.running == scheduler.control == {}
    assert len([item for item in runtime.requests if item["path"] == "/trigger"]) == 1
    with pytest.raises(RuntimeHTTPError, match="disabled"):
        scheduler._run(directory.parent.parent, directory.name, None)


def test_invalid_channel_and_unconnected_webhook_do_not_admit_turns(platform):
    scheduler, _runtime, frontend, _forbidden = platform
    assert request(frontend, "POST", "/v1/channels/wecom/events", {"event": {}})[0] == 400
    assert request(frontend, "POST", "/v1/webhooks/graphs/demo", {"input": {}})[0] == 501
    assert request(frontend, "GET", "/graph-relations")[0] == 501
    assert not scheduler.turns.list("missing")
    assert not scheduler.sessions.list()
    scheduler.sessions.create("channel", graph="demo", channel={"platform": "wecom"})
    assert request(frontend, "GET", "/sessions/channel/messages")[0:2] == (200, {"messages": []})
    assert scheduler.create_turn("channel", "attachment", "hi", channel_input={"attachments": [{}]})[1] == 400
    assert not scheduler.turns.list("channel")
    scheduler.sessions.create("channel-only", channel={"platform": "wecom"})
    assert request(frontend, "POST", "/sessions/channel-only/messages", {"message": "hi"})[0] == 501
    assert request(frontend, "POST", "/sessions/channel-only/turns", {
        "request_id": "unbound", "message": "hi"})[0] == 501
    assert request(frontend, "GET", "/sessions/channel-only/messages")[0] == 501
    assert not scheduler.sessions.get("channel-only").run_ids


def test_channel_turn_retry_compares_sources_without_reopening_deleted_files(platform):
    scheduler, _runtime, _frontend, _forbidden = platform
    scheduler.sessions.create("media-retry", graph="demo", channel={"platform": "wecom"})
    source = {"name": "gone.txt", "path": str(scheduler.root / "state/channels/wecom/events/gone.txt")}
    turn, _ = scheduler.turns.create("media-retry", "same", "read", channel_input={
        "attachments": [{"name": "gone.txt", "path": "/in/channel/gone.txt"}],
        "attachment_sources": [source],
    })
    body, status = scheduler.create_turn("media-retry", "same", "read", {"attachments": [source]})
    assert status == 202 and json.loads(body)["turn"]["id"] == turn["id"]
    assert scheduler.create_turn("media-retry", "same", "read", {"attachments": []})[1] == 409
    changed = {**source, "name": "changed.txt"}
    assert scheduler.create_turn("media-retry", "same", "read", {"attachments": [changed]})[1] == 409
    assert len(scheduler.turns.list("media-retry")) == 1


def test_bad_channel_attachment_does_not_supersede_the_current_turn(platform):
    scheduler, _runtime, _frontend, _forbidden = platform
    scheduler.sessions.create("media-active", graph="demo", channel={"platform": "wecom"})
    turn, _ = scheduler.turns.create("media-active", "first", "keep working")
    completed = threading.Event()
    scheduler.channel_tail["media-active"] = (turn["id"], completed)
    prior_status = scheduler.turns.get("media-active", turn["id"])["status"]
    assert scheduler.create_turn("media-active", "bad", "read", {"attachments": [{}]})[1] == 400
    assert scheduler.turns.get("media-active", turn["id"])["status"] == prior_status
    assert scheduler.channel_tail["media-active"] == (turn["id"], completed)
    assert not scheduler.turns.find_request("media-active", "bad")


def test_plugin_library_reads_from_rust_and_streams_file_through_public_route(platform):
    scheduler, runtime, frontend, _forbidden = platform
    status, catalog, _headers = request(frontend, "GET", "/plugins")
    assert status == 200 and catalog["plugins"][0]["id"] == "sample"
    status, detail, _headers = request(frontend, "GET", "/plugins/sample")
    assert status == 200 and "Existing skill" in detail["instructions"]
    status, payload, headers = request(frontend, "GET", "/plugins/sample/files/skills/sample/SKILL.md")
    assert status == 200 and payload == b"plugin-file:skills/sample/SKILL.md"
    assert headers["Content-Type"] == "application/octet-stream"
    assert headers["X-Content-Type-Options"] == "nosniff"
    assert headers["Content-Security-Policy"].startswith("sandbox;")
    assert headers["Content-Disposition"].endswith("SKILL.md")
    assert any(item["path"] == "/plugins/sample/files/skills/sample/SKILL.md" for item in runtime.requests)
    assert scheduler.runtime is not None


@pytest.mark.parametrize("path", ["/plugins", "/plugins/sample", "/plugins/sample/files/notes.txt"])
def test_plugin_reads_do_not_fall_back_to_local_library_when_rust_is_unavailable(platform, path):
    _scheduler, runtime, frontend, _forbidden = platform
    runtime.unavailable = True
    status, body, _headers = request(frontend, "GET", path)
    assert status == 503
    assert "unavailable" in body["error"]


@pytest.mark.parametrize("path", [
    "/plugins/sample/files/../private.txt",
    "/plugins/sample/files/%2Fprivate.txt",
    "/plugins/sample/files/notes%5Cprivate.txt",
    "/plugins/sample%2Fother",
])
def test_plugin_unsafe_paths_are_rejected_before_upstream_request(platform, path):
    _scheduler, runtime, frontend, _forbidden = platform
    before = len(runtime.requests)
    status, _body, _headers = request(frontend, "GET", path)
    assert status == 400
    assert len(runtime.requests) == before


def test_plugin_install_keeps_existing_library_writer(platform, monkeypatch):
    scheduler, runtime, frontend, _forbidden = platform
    installed = []
    monkeypatch.setattr(scheduler.library, "install", lambda source, plugin_id, **options:
                        installed.append((source, plugin_id, options)) or "installed")
    assert request(frontend, "POST", "/plugins/install", {"source": "existing-source", "id": "installed"})[0] == 201
    assert installed == [("existing-source", "installed", {"replace_existing": False})]
    assert not any(item["path"] == "/plugins/install" for item in runtime.requests)


def test_plugin_authorization_keeps_existing_platform_route_in_rust_backend(platform, monkeypatch):
    _scheduler, runtime, frontend, _forbidden = platform
    calls = []

    def existing_authorization_route(handler, parts):
        calls.append(parts)
        handler._send(json.dumps({"authorized": True}))

    monkeypatch.setattr(Handler, "_authorize_mcp", existing_authorization_route)
    before = len(runtime.requests)
    status, response, _headers = request(frontend, "POST", "/plugins/sample/authorize/server", {})
    assert status == 200 and response == {"authorized": True}
    assert calls == [["plugins", "sample", "authorize", "server"]]
    assert len(runtime.requests) == before


def test_channel_session_cannot_discard_the_index_of_retained_rust_history(platform):
    scheduler, runtime, frontend, _forbidden = platform
    scheduler.sessions.create("conversation", graph="demo", reply_node="work")
    runtime.runs["channel-prior"] = {"graph": "demo", "active": False, "state": {
        "status": "completed", "trigger": {"source": "channel", "session": "conversation", "reply_node": "work"}}}
    assert request(frontend, "DELETE", "/sessions/conversation")[0] == 409
    assert scheduler.sessions.get("conversation").graph == "demo"
    runtime.runs.clear()
    assert request(frontend, "DELETE", "/sessions/conversation")[0] == 200


def test_channel_session_delete_rechecks_concurrent_turn_after_public_run_query(platform, monkeypatch):
    scheduler, _runtime, frontend, _forbidden = platform
    scheduler.sessions.create("conversation", graph="demo", reply_node="work")

    def changed_during_request():
        # The remote response was empty before a concurrently admitted Turn
        # completed. Its persisted index must invalidate that stale response.
        turn, _ = scheduler.turns.create("conversation", "concurrent", "hello")
        scheduler.turns.finish(turn["id"], "completed")
        return []

    monkeypatch.setattr(scheduler, "runs", changed_during_request)
    assert request(frontend, "DELETE", "/sessions/conversation")[0] == 409
    assert len(scheduler.turns.list("conversation")) == 1


@pytest.mark.parametrize("backend,url", [("unknown", "http://127.0.0.1:1"), ("rust", ""),
                                        ("rust", "file:///tmp/runtime"), ("rust", "http://user:secret@localhost"),
                                        ("rust", "http://localhost?secret=value"), ("rust", "http://localhost:invalid")])
def test_invalid_explicit_backend_configuration_is_rejected_before_service_start(monkeypatch, backend, url):
    monkeypatch.setenv("ANCHOR_RUNTIME_BACKEND", backend)
    monkeypatch.setenv("ANCHOR_RUNTIME_URL", url)
    with pytest.raises(ValueError):
        RuntimeHTTPClient.from_env()


def test_default_backend_remains_python_and_rust_network_outage_has_no_fallback(tmp_path, monkeypatch):
    monkeypatch.delenv("ANCHOR_RUNTIME_BACKEND", raising=False)
    monkeypatch.setenv("ANCHOR_RUNTIME_URL", "http://127.0.0.1:1")
    assert Scheduler(tmp_path, tmp_path / "config.json").runtime is None
    monkeypatch.setenv("ANCHOR_RUNTIME_BACKEND", "rust")
    scheduler = Scheduler(tmp_path, tmp_path / "config.json")
    assert scheduler.trigger("demo", None)[1] == 503
    assert not (tmp_path / "workspaces").exists()


def test_invalid_upstream_json_is_explicit_and_never_creates_local_graph(platform):
    scheduler, runtime, frontend, _forbidden = platform
    runtime.overrides[("POST", "/graphs")] = (200, b"invalid-json")
    assert request(frontend, "POST", "/graphs", {"name": "new", "definition": runtime.definition})[0] == 502
    assert not (scheduler.root / "workspaces" / "new").exists()


def test_upstream_auth_failure_does_not_fall_back_to_existing_local_graph(platform):
    scheduler, runtime, frontend, _forbidden = platform
    legacy_run(scheduler.root, graph="demo")
    runtime.overrides[("POST", "/trigger")] = (401, {"error": "upstream API key rejected"})
    assert request(frontend, "POST", "/trigger", {"graph": "demo"})[0] == 401
    assert len([item for item in runtime.requests if item["path"] == "/trigger"]) == 1
    assert len(list((scheduler.root / "workspaces" / "demo" / "runs").iterdir())) == 1


def test_frontend_auth_is_checked_before_forwarding_and_upstream_uses_its_own_key(platform):
    scheduler, runtime, frontend, _forbidden = platform
    scheduler.api_keys = ("frontend-key-fixture" * 2,)
    assert request(frontend, "GET", "/graphs")[0] == 401
    assert not runtime.requests
    client = HTTPConnection(*frontend.server_address, timeout=3)
    try:
        client.request("GET", "/graphs", headers={"Authorization": f"Bearer {scheduler.api_keys[0]}"})
        response = client.getresponse()
        assert response.status == 200
        response.read()
    finally:
        client.close()
    assert runtime.requests[0]["authorization"] == "Bearer upstream-fixture-key"


def test_lost_mutation_response_sends_one_request_without_python_fallback(tmp_path, monkeypatch):
    received = []

    class Interrupted(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            received.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
            self.close_connection = True

    with running_server(Interrupted) as server:
        monkeypatch.setenv("ANCHOR_RUNTIME_BACKEND", "rust")
        monkeypatch.setenv("ANCHOR_RUNTIME_URL", f"http://127.0.0.1:{server.server_port}")
        scheduler = Scheduler(tmp_path, tmp_path / "config.json")
        assert scheduler.trigger("demo", None)[1] == 503
    assert len(received) == 1
    assert not (tmp_path / "workspaces").exists()


def test_runtime_client_does_not_follow_redirects_or_forward_credentials():
    forwarded = []

    class Destination(BaseHTTPRequestHandler):
        def do_GET(self):
            forwarded.append(self.headers.get("Authorization"))
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"{}")

        def log_message(self, *args):
            pass

    with running_server(Destination) as destination:
        class Redirect(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(302)
                self.send_header("Location", f"http://127.0.0.1:{destination.server_port}/secret")
                self.end_headers()
                self.wfile.write(b'{"error":"redirect refused"}')

            def log_message(self, *args):
                pass

        with running_server(Redirect) as redirect:
            client = RuntimeHTTPClient(f"http://127.0.0.1:{redirect.server_port}", api_key="fixture-secret")
            assert client.request("GET", "/graphs")[1] == 302
    assert not forwarded
