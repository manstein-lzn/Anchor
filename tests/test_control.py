"""Pausing between nodes and stopping the current node.

Driven through the scheduler directly rather than over HTTP: the mechanism is the thing under test,
and a test that needed a port would be a test of the port.
"""

from __future__ import annotations

import json
import time
from pathlib import Path

import pytest

from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox
from anchor.serve import Scheduler


@pytest.fixture(scope="module", autouse=True)
def needs_a_sandbox():
    try:
        BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    except RuntimeError as exc:                     # a container that forbids namespaces
        pytest.skip(f"no usable sandbox on this machine: {exc}")


def _scheduler(tmp_path: Path, steps: int = 4, delay: float = 1.5) -> Scheduler:
    """A graph slow enough to catch in the middle, and free: one op per step, each sleeping."""
    names = [f"step{i}" for i in range(steps)]
    graph = {
        "entry": names[0],
        "objective": "a run that takes long enough to be interrupted",
        "ops": {name: {"run": f"printf '{name}\\n' > {name}.txt && sleep {delay} && echo '{name} done'",
                       "writes": [f"{name}.txt"]} for name in names},
        "nodes": [{"id": name, "op": name} for name in names],
        "edges": [{"from": names[i], "to": names[i + 1]} for i in range(steps - 1)],
    }
    workspace = tmp_path / "workspaces" / "slow"
    workspace.mkdir(parents=True)
    (workspace / "graph.json").write_text(json.dumps(graph), encoding="utf-8")
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}', encoding="utf-8")
    return Scheduler(tmp_path, config)


def _settle(scheduler: Scheduler, timeout: float = 30.0) -> None:
    """Wait for whatever is running to stop running."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        with scheduler.lock:
            if not scheduler.running:
                return
        time.sleep(0.1)
    raise AssertionError("the run did not settle")


def _state(scheduler: Scheduler, run_id: str) -> dict:
    workspace = scheduler.workspace("slow")
    return json.loads((workspace / "runs" / run_id / "run.json").read_text(encoding="utf-8"))


def test_stopping_cancels_the_current_command_and_is_terminal(tmp_path):
    scheduler = _scheduler(tmp_path, delay=30)
    _, status = scheduler.trigger("slow", None)
    assert status == 202
    run_id = scheduler.running["slow"]

    first = scheduler.workspace("slow") / "runs" / run_id / "step0" / "step0.txt"
    deadline = time.monotonic() + 5
    while not first.exists() and time.monotonic() < deadline:
        time.sleep(0.05)
    assert first.exists(), "the command did not begin"
    response, code = scheduler.control_run(run_id, "stop")
    assert code == 202, response
    _settle(scheduler, timeout=3)

    state = _state(scheduler, run_id)
    assert state["status"] == "stopped" and state["reason"] == "asked"
    assert state["executed"] == [], "the cancelled node was recorded as completed"


def test_pausing_leaves_the_run_where_it_was_and_resuming_carries_on(tmp_path):
    """A paused run is not finished and not running: it keeps the edges it decided and the nodes it
    ran, so continuing picks up from there rather than starting over."""
    scheduler = _scheduler(tmp_path)
    scheduler.trigger("slow", None)
    run_id = scheduler.running["slow"]

    time.sleep(2.2)
    scheduler.control_run(run_id, "pause")
    _settle(scheduler)
    paused = _state(scheduler, run_id)

    assert paused["status"] == "paused" and paused["reason"] == "asked"
    ran = list(paused["executed"])
    assert 0 < len(ran) < 4

    response, code = scheduler.control_run(run_id, "resume")
    assert code == 202, response
    _settle(scheduler)
    finished = _state(scheduler, run_id)

    assert finished["status"] == "finished"
    assert finished["executed"][:len(ran)] == ran, "it carried on rather than starting over"
    assert len(finished["executed"]) == 4


def test_a_control_for_a_run_that_is_not_running_is_refused(tmp_path):
    """Not silently accepted: whoever asked has to be able to tell that nothing was asked of
    anything."""
    scheduler = _scheduler(tmp_path, steps=1)

    response, code = scheduler.control_run("no-such-run", "stop")

    assert code == 409 and "not running" in response
    assert scheduler.control_run("no-such-run", "sideways")[1] == 400


def test_a_run_that_paused_and_carried_on_does_not_keep_saying_it_was_asked(tmp_path):
    """Why the last attempt stopped is not why this one might.

    A pause records `reason: asked` so the run says why it left off. Nothing cleared it, so a run that
    paused, resumed and finished still said `asked` — and the view reads that as "stopped on request",
    which is the opposite of what happened.
    """
    scheduler = _scheduler(tmp_path, steps=3)
    scheduler.trigger("slow", None)
    run_id = scheduler.running["slow"]

    time.sleep(1.8)
    scheduler.control_run(run_id, "pause")
    _settle(scheduler)
    assert _state(scheduler, run_id)["status"] == "paused"

    scheduler.control_run(run_id, "resume")
    _settle(scheduler)
    finished = _state(scheduler, run_id)

    assert finished["status"] == "finished"
    assert finished["reason"] == "", "it finished, and it was not asked to stop"
