"""Opt-in real-provider acceptance: kill the Pilot process mid-turn, restart, and continue.

Run from the repository root:

    ./.venv/bin/python scripts/verify_pilot_resume.py

It starts a real `anchor` server on an isolated data root, drives it over HTTP, kills it with SIGKILL
while a turn is in flight, starts it again on the same root, and then checks what the next message in
the same Session can see. The model, the tools and the file store are the real ones; nothing is
patched. Evidence — sessions, the harness file record, turn rows and the server log — is kept under
`.local/pilot-resume-<suffix>/` for independent acceptance.

Two cases, matching the two facts a crash can leave behind:

* `missing`  the tool call started and never returned: the next turn must be told the result is
             unknown, and must not present a result nobody saw;
* `recorded` the tool call returned before the kill: the next turn must be able to read that result
             back out of the record and repeat it verbatim.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
from http.client import HTTPConnection
from pathlib import Path
from uuid import uuid4

REPO = Path(__file__).resolve().parents[1]
LONG_ANSWER = "然后写一篇 600 字左右的分析，说明接下来应该怎么继续。"


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def request(port: int, method: str, path: str, body: dict | None = None, expected: int = 200):
    client = HTTPConnection("127.0.0.1", port, timeout=180)
    try:
        client.request(method, path, json.dumps(body) if body is not None else None,
                       {"Content-Type": "application/json"})
        response = client.getresponse()
        payload = response.read().decode()
        assert response.status == expected, (method, path, response.status, payload[:400])
        return json.loads(payload) if payload else {}
    finally:
        client.close()


class Server:
    """One `anchor` process, started from this checkout with an isolated data root."""

    def __init__(self, root: Path, config: Path, log: Path):
        self.root, self.config, self.log = root, config, log
        self.port = free_port()
        self.process: subprocess.Popen | None = None

    def start(self) -> None:
        handle = self.log.open("a", encoding="utf-8")
        handle.write(f"\n===== start {time.strftime('%H:%M:%S')} on {self.port}\n")
        handle.flush()
        self.process = subprocess.Popen(
            [sys.executable, "-m", "anchor", "--root", str(self.root), "--config", str(self.config),
             "--host", "127.0.0.1", "--port", str(self.port)],
            cwd=REPO, stdout=handle, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            try:
                request(self.port, "GET", "/sessions")
                return
            except (OSError, AssertionError):
                time.sleep(0.1)
        raise AssertionError(f"the server never became ready; see {self.log}")

    def kill(self) -> None:
        assert self.process is not None
        os.kill(self.process.pid, signal.SIGKILL)
        self.process.wait(timeout=30)
        assert self.process.returncode == -9, self.process.returncode
        self.process = None

    def stop(self) -> None:
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=30)
        self.process = None


def wait_for(predicate, what: str, timeout: float = 180):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.01)
    raise AssertionError(f"timed out waiting for {what}")


#: The data root the file record is being read from; set once in `main`.
ROOT_OF_RECORD: Path = Path(".local")


def step_runs(root: Path) -> list[Path]:
    base = root / "state" / "pilot-steps"
    return sorted(base.iterdir()) if base.is_dir() else []


def events_of(run: Path) -> list[dict]:
    path = run / "events.jsonl"
    if not path.is_file():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def new_run(appeared_after: set[str]) -> Path:
    """The file record the next turn creates, watched by name so an earlier run cannot be mistaken."""
    return wait_for(lambda: next((item for item in step_runs(ROOT_OF_RECORD)
                                  if item.name not in appeared_after), None),
                    "the next turn's file record")


def wait_for_event(run: Path, kind: str, tool_name: str | None = None, after: int = 1):
    """The `after`-th (1-based) matching event, so a later request can be waited for specifically."""
    def found():
        events = [event for event in events_of(run)
                  if event["kind"] == kind and (tool_name is None or event.get("tool_name") == tool_name)]
        return events[after - 1] if len(events) >= after else None
    return wait_for(found, f"{kind}/{tool_name} #{after} in {run.name}")


def turn_started(server: Server, session: str, message: str) -> dict:
    body = {"request_id": str(uuid4()), "message": message}
    return request(server.port, "POST", f"/sessions/{session}/turns", body, 202)["turn"]


def turn_settled(server: Server, session: str, turn_id: str) -> dict:
    def settled():
        for item in request(server.port, "GET", f"/sessions/{session}/turns")["turns"]:
            if item["id"] == turn_id and item["status"] != "running":
                return item
        return None
    return wait_for(settled, f"turn {turn_id} to reach a terminal state")


def converse(server: Server, session: str, message: str) -> tuple[dict, str]:
    """Send one message, wait for the turn to settle, and read the reply it left behind."""
    turn = turn_started(server, session, message)
    settled = turn_settled(server, session, turn["id"])
    reply = ""
    if settled["status"] == "completed":
        messages = request(server.port, "GET", f"/sessions/{session}/messages")["messages"]
        reply = messages[-1]["text"] if messages and messages[-1]["role"] == "assistant" else ""
    return settled, reply


def prepare_run(server: Server, root: Path) -> str:
    """One real Graph Run whose id the `recorded` case can have the model read back."""
    definition = {
        "entry": "note", "objective": "resume acceptance",
        "ops": {"note": {"run": "printf 'resume-check\\n' > note.txt", "writes": ["note.txt"]}},
        "nodes": [{"id": "note", "op": "note"}], "edges": [],
    }
    request(server.port, "POST", "/sessions", {"id": "prepare"}, 201)
    request(server.port, "POST", "/graphs", {"name": "resume-demo", "definition": definition}, 200)
    settled, reply = converse(
        server, "prepare", "请调用 graph_run 启动 resume-demo（objective 写 resume acceptance），"
                           "只调用一次；工具返回后一句话报告 run 标识。")
    assert settled["status"] == "completed", settled
    session = request(server.port, "GET", "/sessions/prepare")["session"]
    assert len(session["run_ids"]) == 1, session
    run_id = session["run_ids"][0]
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        state = request(server.port, "GET", f"/runs/{run_id}")["state"]
        if state["status"] != "running":
            break
        time.sleep(0.2)
    assert state["status"] == "finished", state
    assert (root / "workspaces/resume-demo/runs" / run_id / "note/note.txt").read_text() == "resume-check\n"
    print(f"PASS prepare: graph_create and graph_run ran without a separate approval, run {run_id}",
          flush=True)
    return run_id


def case_missing(server: Server, root: Path) -> None:
    """The process dies inside a tool: the next turn must be told the outcome is unknown."""
    block = root / "workspaces/big-run/runs/bigrun/big/block.txt"
    block.parent.mkdir(parents=True, exist_ok=True)
    (block.parents[3] / "graph.json").write_text(
        json.dumps({"entry": "big", "nodes": [{"id": "big", "op": "big"}],
                    "ops": {"big": {"run": "true"}}, "edges": []}), encoding="utf-8")
    (block.parent.parent / "run.json").write_text(
        json.dumps({"run": "bigrun", "status": "finished", "objective": "kill window"}), encoding="utf-8")
    with block.open("w", encoding="utf-8") as handle:
        chunk = "evidence line\n" * 40_000
        for _ in range(600):                       # ~380 MB: reading it takes long enough to be killed
            handle.write(chunk)
    request(server.port, "POST", "/sessions", {"id": "missing"}, 201)
    session = "missing"
    before = {item.name for item in step_runs(root)}
    turn = turn_started(server, session,
                        "请调用 artifact_read，参数 run='bigrun', node='big', path='block.txt'。"
                        "工具返回后，用一句话说明你读到了什么。只调用一次，不要做别的事。")
    run = new_run(before)
    wait_for_event(run, "tool_call_started", "artifact_read")
    server.kill()
    effects = _effects(run)
    started = [item for item in effects if item["tool_name"] == "artifact_read"]
    assert started and started[-1]["status"] == "started", effects
    print(f"PASS kill: the process died with artifact_read started and no terminal record "
          f"({run.name}, turn {turn['id']})", flush=True)

    server.start()
    settled = turn_settled(server, session, turn["id"])
    assert settled["status"] == "interrupted", settled
    assert request(server.port, "GET", f"/sessions/{session}")["session"]["status"] == "interrupted"
    settled, reply = converse(server, session,
                              "上一个进程在工具执行中终止了。请先核查现场（可以用 run_list 等工具），"
                              "然后用一句话说明那次 artifact_read 的结果是否还在；"
                              "如果结果不在了，回答里必须包含 INTERRUPTED 这个词。")
    assert settled["status"] == "completed", settled
    assert "INTERRUPTED" in reply.upper(), reply
    print(f"PASS missing: the new turn loaded the interrupted record and reported it. "
          f"Reply: {reply.strip()[:200]}", flush=True)


def case_recorded(server: Server, root: Path, run_id: str) -> None:
    """The tool returned before the kill: the next turn must be able to read that result back."""
    request(server.port, "POST", "/sessions", {"id": "recorded"}, 201)
    before = {item.name for item in step_runs(root)}
    turn = turn_started(server, "recorded",
                        "请调用 run_list（只调用一次）。工具返回后先写一行 RESULT:，"
                        f"把你看到的第一个 run 标识原样写在后面。{LONG_ANSWER}")
    run = new_run(before)
    wait_for_event(run, "tool_call_completed", "run_list")
    # The tool result is in the record once the boundary snapshot after it exists; wait for the next
    # model request, which only starts after that snapshot was written.
    wait_for_event(run, "model_request_started", None, after=2)
    server.kill()
    effects = _effects(run)
    settled = [item for item in effects if item["tool_name"] == "run_list"]
    assert settled and settled[-1]["status"] == "completed", effects
    print(f"PASS kill: run_list completed and was recorded before the process died "
          f"({run.name}, turn {turn['id']})", flush=True)

    server.start()
    state = turn_settled(server, "recorded", turn["id"])
    assert state["status"] == "interrupted", state
    before = {item.name for item in step_runs(root)}
    settled, reply = converse(server, "recorded",
                              "上一个进程在写回答时被终止了，你没有看到它的结尾。"
                              "你之前调用 run_list 得到的那个结果里，第一个 run 标识是什么？"
                              "如果记录里没有那个结果，就直接说明结果已经丢失，不要重新调用 run_list。")
    assert settled["status"] == "completed", settled
    assert run_id in reply, f"the recorded run id is missing from the reply: {reply[:400]}"
    # The only place that id exists is the carried-over record: the resumed turn must not have
    # looked it up again.
    resumed = new_run(before)
    called = [event.get("tool_name") for event in events_of(resumed) if event["kind"] == "tool_call_started"]
    assert "run_list" not in called, f"the run id was looked up again instead of read back: {called}"
    print(f"PASS recorded: the new turn read the saved tool result back ({run_id}). "
          f"Reply: {reply.strip()[:200]}", flush=True)


def _effects(run: Path) -> list[dict]:
    path = run / "tool_effects.jsonl"
    if not path.is_file():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=Path(".local/runtime.json"))
    parser.add_argument("--only", choices=["missing", "recorded"], default=None)
    args = parser.parse_args()
    Path(".local").mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="pilot-resume-", dir=".local")).resolve()
    global ROOT_OF_RECORD
    ROOT_OF_RECORD = root
    server = Server(root, args.config.resolve(), root / "server.log")
    print(f"Evidence: {root}", flush=True)
    try:
        server.start()
        run_id = prepare_run(server, root)
        if args.only != "recorded":
            case_missing(server, root)
        if args.only != "missing":
            case_recorded(server, root, run_id)
        print("PASS resume acceptance: killed mid-turn, restarted, continued in the same Session",
              flush=True)
    finally:
        server.stop()
        print(f"Evidence kept at {root}", flush=True)


if __name__ == "__main__":
    main()
