"""Pausing and stopping a run, asked between nodes.

A node in flight is inside a sandbox command or a model call, and nothing outside can reach into it.
So a pause or a stop lands when the node that is running finishes — which is a real limitation and is
why it is the first thing the docstrings say, rather than something an operator discovers by pressing
a button that appears not to work.

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


def _scheduler(tmp_path: Path, steps: int = 4) -> Scheduler:
    """A graph slow enough to catch in the middle, and free: one op per step, each sleeping."""
    names = [f"step{i}" for i in range(steps)]
    graph = {
        "entry": names[0],
        "objective": "a run that takes long enough to be interrupted",
        "ops": {name: {"run": f"printf '{name}\\n' > {name}.txt && sleep 1.5 && echo '{name} done'",
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


def test_stopping_lands_between_nodes_and_is_not_resumed_on_restart(tmp_path):
    """`stopped` is terminal: a restart must not pick it up, which is the whole difference from
    pausing. The node that was running when the request arrived finished first — that is stated, not
    discovered."""
    scheduler = _scheduler(tmp_path)
    _, status = scheduler.trigger("slow", None)
    assert status == 202
    run_id = scheduler.running["slow"]

    time.sleep(2.2)                       # let the first node finish and the second start
    response, code = scheduler.control_run(run_id, "stop")
    assert code == 202, response
    _settle(scheduler)

    state = _state(scheduler, run_id)
    assert state["status"] == "stopped" and state["reason"] == "asked"
    assert 1 <= len(state["executed"]) < 4, "stopped in the middle, not at either end"


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
