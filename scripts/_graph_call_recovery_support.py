"""Private helpers for ``scripts/rust_graph_call_recovery_smoke.py``.

Nothing here runs product logic by itself. It only prepares disposable
fixtures, drives the Rust HTTP host exactly like an external operator, injects
real process faults by killing that host, and reads durable facts back. Graph
execution, persistence, sandboxing and recovery stay inside the Rust binaries.

The module is intentionally private (leading underscore): the public entry
point, scenario matrix and evidence schema live in the smoke script.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shlex
import signal
import socket
import subprocess
import time
from dataclasses import dataclass
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


ROOT = Path(__file__).resolve().parents[1]
HOST_BINARY = ROOT / "rust" / "target" / "debug" / "anchor-runner-host"
FIXTURE_BINARY = ROOT / "rust" / "target" / "debug" / "examples" / "http_fixture"

MODEL_KEYS = ("ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL", "ANCHOR_MODEL_NAME")
# The Rust Graph-call acceptance freezes the product wire; the model name is
# only a default that the local ``.env`` may override.
WIRE_API = "responses"
DEFAULT_MODEL = "deepseek-flash"
ALLOWED_COMMANDS = "sh,cat,cp,printf,test,sleep"

PROOF_SCHEMA = 1

TERMINAL = ("completed", "failed", "aborted", "stopped")


def command(script: str) -> str:
    """One Op.run command string, matching the shape the Rust host expects."""
    return shlex.join(["sh", "-c", script])


def plugin_resource_bytes(endpoint: str) -> bytes:
    return json.dumps({
        "name": "Local child Graph fixture",
        "mcpServers": {"fixture": {"type": "http", "url": endpoint}},
    }).encode()


def plugin_digest(resource: bytes) -> str:
    return hashlib.sha256(b"plugin.json" + hashlib.sha256(resource).digest()).hexdigest()


def write_bundle(root: Path, graph: dict, plugin_resource: bytes | None = None) -> None:
    """Materialize one disposable Graph bundle (graph.json + manifest.json)."""
    root.mkdir(parents=True)
    (root / "graph.json").write_text(json.dumps(graph, ensure_ascii=False, indent=2))
    plugins = []
    if plugin_resource is not None:
        plugin = root / "plugins/fixture"
        plugin.mkdir(parents=True)
        (plugin / "plugin.json").write_bytes(plugin_resource)
        plugins.append({
            "id": "fixture",
            "digest": plugin_digest(plugin_resource),
            "resources": ["plugin.json"],
            "mcp_servers": ["fixture"],
        })
    (root / "manifest.json").write_text(
        json.dumps({"format": 1, "graph": "graph.json", "plugins": plugins})
    )


def parent_graph(
    token: str,
    child_name: str,
    result_node: str,
    expected: str,
    *,
    verify_delay_seconds: float = 0.0,
) -> dict:
    """The frozen parent loop: publish -> wait child -> verify returned file."""
    verify = f'test "$(cat /in/invoke/result/report.txt)" = {shlex.quote(expected)} '
    if verify_delay_seconds:
        verify += f"&& sleep {verify_delay_seconds:g} "
    verify += "&& cp /in/invoke/result/report.txt verified.txt"
    return {
        "objective": (
            "Publish a request file, wait for the child Graph transformation, then verify "
            "the returned result file."
        ),
        "input": {},
        "entry": "publish",
        "agents": {},
        "ops": {
            "produce": {"run": command(f"printf %s {shlex.quote(token)} > request.txt")},
            "invoke": {
                "call": {
                    "graph": child_name,
                    "mode": "wait",
                    "input_map": {"subject": "/topic"},
                    "files": [{"node": "publish", "path": "request.txt", "as": "request.txt"}],
                    "result": {"node": result_node, "files": ["report.txt"]},
                }
            },
            "verify": {"run": command(verify)},
        },
        "nodes": [
            {"id": "publish", "op": "produce"},
            {"id": "invoke", "op": "invoke"},
            {"id": "verify", "op": "verify"},
        ],
        "edges": [{"from": "publish", "to": "invoke"}, {"from": "invoke", "to": "verify"}],
    }


def op_child_graph(*, delay_seconds: float = 0.0) -> dict:
    """Provider-free child: one deterministic Op.run node, no Agent, no Plugin.

    ``report.txt`` is overwritten (idempotent), while ``attempts.log`` appends
    one byte per *execution* so the caller can honestly report how many times
    the node ran across a restart.
    """
    script = (
        'printf "%s-checked" "$(cat /in/call/request.txt)" > report.txt '
        "&& printf x >> attempts.log"
    )
    if delay_seconds:
        script += f" && sleep {delay_seconds:g}"
    return {
        "objective": "Transform the handed request file into a result file.",
        "input": {},
        "entry": "transform",
        "agents": {},
        "ops": {"transform": {"run": command(script)}},
        "nodes": [{"id": "transform", "op": "transform"}],
        "edges": [],
    }


def agent_child_graph(model: str, *, write_sleep_seconds: float = 0.0) -> dict:
    """Real-provider child: Agent + Bubblewrap + local Streamable HTTP MCP.

    Mirrors ``scripts/rust_graph_call_smoke.py``: read the handed file, call the
    ``fixture_suffix`` MCP tool, then write only the returned text. The optional
    sleep widens the window in which an external kill lands mid tool call.
    """
    sleep = f"; sleep {write_sleep_seconds:g}" if write_sleep_seconds else ""
    return {
        "objective": "Transform the handed request file through the fixture MCP tool.",
        "input": {},
        "entry": "worker",
        "agents": {
            "worker": {
                "model": model,
                "network": True,
                "wall_time_limit_seconds": 180,
                "instructions": (
                    'Use anchor_run with command=["cat","/in/call/request.txt"] to read the '
                    "request file handed to this child Run. Call the attached MCP tool "
                    "fixture-fixture_fixture_suffix with text=<the exact request file contents>. "
                    "Use anchor_run with "
                    "command=[\"sh\",\"-c\",\"printf '%s' '<returned structured text field>' > "
                    f"/workspace/report.txt{sleep}\"] to write ONLY the returned structured text "
                    "field into /workspace/report.txt (no newline, no quotes, no Markdown). "
                    "Do not invent the suffix or transform the text yourself. The mapped 'subject' "
                    "value in the Input JSON below comes from the parent's input_map. Read "
                    "/workspace/report.txt back with anchor_run, then finish with exactly one JSON "
                    'object of the form {"summary": "<...>"}: the summary states the exact '
                    "report.txt contents and includes the subject verbatim. Add no other keys and "
                    "no prose, Markdown or code fences."
                ),
            }
        },
        "ops": {},
        "nodes": [{"id": "worker", "agent": "worker", "plugins": ["fixture"]}],
        "edges": [],
    }


def read_json(path: Path) -> dict | None:
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def run_records(state_root: Path) -> dict[str, dict]:
    """Every durable Run record, keyed by Run id (atomic-renamed JSON files)."""
    directory = state_root / "runs"
    records: dict[str, dict] = {}
    if not directory.is_dir():
        return records
    for path in directory.glob("*.json"):
        record = read_json(path)
        if isinstance(record, dict) and record.get("run_id"):
            records[str(record["run_id"])] = record
    return records


def call_children(state_root: Path) -> dict[str, dict]:
    return {run_id: record for run_id, record in run_records(state_root).items()
            if run_id.startswith("call-")}


def parent_call(record: dict) -> dict | None:
    calls = [call for call in (record.get("graph_calls") or {}).values()
             if call.get("mode") == "wait"]
    if len(calls) != 1:
        return None
    return calls[0]


def node_result(record: dict, node: str) -> dict | None:
    results = (record.get("results") or {}).get(node) or []
    return results[-1] if results else None


def artifact_file(state_root: Path, commit_id: str, relative: str) -> Path:
    return state_root / "artifacts" / commit_id / "files" / relative


def child_workspace_file(workspace_root: Path, child_run_id: str, name: str) -> Path | None:
    matches = sorted((workspace_root / child_run_id).glob(f"*/{name}"))
    return matches[0] if matches else None


def mcp_call_count(proof: Path) -> int:
    path = proof / "mcp-calls.jsonl"
    if not path.is_file():
        return 0
    return sum(1 for line in path.read_text().splitlines() if line.strip())


def provider_requests(state_root: Path) -> list[tuple]:
    """Raw ``provider_calls`` rows from every child io-harness store."""
    rows: list[tuple] = []
    import sqlite3

    for store in sorted((state_root / "io-harness" / "store").glob("*.sqlite3")):
        try:
            with sqlite3.connect(f"file:{store}?mode=ro", uri=True) as connection:
                rows.extend(connection.execute(
                    "SELECT model,prompt_tokens,completion_tokens,total_tokens "
                    "FROM provider_calls ORDER BY id"
                ).fetchall())
        except sqlite3.Error:
            continue
    return rows


def wait_until(check, seconds: float, what: str):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise TimeoutError(f"timed out waiting for {what}")


def free_port() -> int:
    with socket.socket() as reserved:
        reserved.bind(("127.0.0.1", 0))
        return int(reserved.getsockname()[1])


@dataclass
class Fault:
    """A real ``SIGKILL`` of the host, taken at an observed durable boundary."""

    boundary: str
    attempts: int
    observed: dict


class Host:
    """One ``anchor-runner-host serve`` process, restartable at the same port."""

    def __init__(self, proof: Path, env: dict[str, str], port: int):
        self.proof = proof
        self.env = env
        self.base = f"http://127.0.0.1:{port}"
        self.port = port
        self.process: subprocess.Popen | None = None
        self.generation = 0

    def start(self) -> None:
        self.generation += 1
        log_path = self.proof / f"host-{self.generation}.log"
        with log_path.open("wb") as log:
            self.process = subprocess.Popen(
                [str(HOST_BINARY), "serve"],
                env=self.env,
                stdout=log,
                stderr=log,
                start_new_session=True,
            )
        wait_until(self._ready, 20, "Rust HTTP host readiness")

    def _ready(self) -> bool:
        if self.process is not None and self.process.poll() is not None:
            raise RuntimeError("Rust HTTP host exited; inspect the retained proof log")
        try:
            return self.request("GET", "/health")[0] == 200
        except (URLError, ConnectionError):
            return False

    def request(self, method: str, path: str, body=None):
        request = Request(
            self.base + path,
            method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        try:
            with urlopen(request, timeout=30) as response:
                raw = response.read()
                return response.status, json.loads(raw) if raw else None
        except HTTPError as error:
            raw = error.read()
            return error.code, json.loads(raw) if raw else None

    def expect(self, method: str, path: str, body=None, status=200):
        actual, value = self.request(method, path, body)
        if actual != status:
            raise AssertionError(f"{method} {path}: expected {status}, got {actual}: {value}")
        return value

    def detail(self, run_id: str) -> dict:
        return self.expect("GET", f"/runs/{run_id}")

    def kill(self) -> None:
        """SIGKILL the host and its sandbox children, simulating a crash."""
        if self.process is None or self.process.poll() is not None:
            self.process = None
            return
        try:
            os.killpg(os.getpgid(self.process.pid), signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.process.wait(timeout=10)
        self.process = None

    def close(self) -> None:
        self.kill()


def inject_fault(host: Host, predicate, boundary: str, timeout: float,
                 attempts: int, done=None) -> Fault | None:
    """Kill ``host`` the instant ``predicate`` first reports a durable boundary.

    ``predicate`` returns an evidence dict (truthy) or ``None``. The kill lands
    right after the observation so the reported boundary and the fault refer to
    the same durable state. ``done`` short-circuits polling once the Run can no
    longer reach the boundary (for example, it already terminated).
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if done is not None and done():
            return None
        try:
            observed = predicate()
        except (OSError, ValueError):
            observed = None
        if observed:
            host.kill()
            return Fault(boundary=boundary, attempts=attempts, observed=observed)
        time.sleep(0.001)
    return None


def start_fixture(evidence_path: Path) -> tuple[subprocess.Popen, str]:
    """Start the local-only MCP fixture and return it plus its endpoint."""
    import select

    fixture = subprocess.Popen(
        [str(FIXTURE_BINARY), str(evidence_path), "--append-evidence"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env={"PATH": os.environ.get("PATH", "/usr/bin:/bin")},
        start_new_session=True,
    )
    if fixture.stdout is None or not select.select([fixture.stdout], [], [], 15)[0]:
        terminate(fixture)
        raise RuntimeError("local MCP fixture did not become ready")
    endpoint = fixture.stdout.readline().strip()
    if not endpoint.startswith("http://127.0.0.1:"):
        terminate(fixture)
        raise RuntimeError("local MCP fixture returned an invalid endpoint")
    return fixture, endpoint


def terminate(process: subprocess.Popen | None) -> None:
    if process is None or process.poll() is not None:
        return
    try:
        os.killpg(os.getpgid(process.pid), signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(os.getpgid(process.pid), signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=5)
