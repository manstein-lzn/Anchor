"""The two ways this runtime could report success for work it did not do.

Neither needs a model. The scheduler is the thing under test, so the node is replaced by a stub that
does exactly what a test says and nothing else — the point is what the run records, not what a model
would have written.
"""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from anchor.simple import run as runner
from anchor.simple.run import InputCollision, _seed


# -- seeding: two inputs claiming one path ----------------------------------------------------


def _tree(root: Path, files: dict[str, str]) -> Path:
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return root


def test_two_inputs_claiming_the_same_path_are_refused(tmp_path):
    """The node would otherwise work from whichever was copied last, and submit anyway."""
    a = _tree(tmp_path / "a", {"notes.md": "from a", "data/shared.json": '{"from": "a"}'})
    b = _tree(tmp_path / "b", {"notes.md": "from b", "data/shared.json": '{"from": "b"}'})

    with pytest.raises(InputCollision) as caught:
        _seed(tmp_path / "target", [("a", a), ("b", b)])

    message = str(caught.value)
    assert "notes.md" in message and "data/shared.json" in message
    assert "a" in message and "b" in message
    assert not (tmp_path / "target" / "notes.md").exists(), "nothing is copied before the refusal"


def test_two_inputs_holding_the_same_bytes_are_not_a_collision(tmp_path):
    """A node carries its inputs forward, so two branches sharing an ancestor hold the same file.

    Refusing that would reject the pass-through the revise loop is built on; only different contents
    mean something would actually be lost.
    """
    a = _tree(tmp_path / "a", {"base.md": "the same", "a.md": "from a"})
    b = _tree(tmp_path / "b", {"base.md": "the same", "b.md": "from b"})

    target = tmp_path / "target"
    _seed(target, [("a", a), ("b", b)])

    assert (target / "base.md").read_text() == "the same"
    assert (target / "a.md").exists() and (target / "b.md").exists()


def test_inputs_that_do_not_collide_still_merge_flat(tmp_path):
    """A node's directory is its inputs plus its own work, side by side — not one directory each.

    The revise loop depends on this: `draft.md` and `review.md` have to arrive at the same level, or
    nesting would deepen by one directory every round.
    """
    a = _tree(tmp_path / "a", {"draft.md": "draft", "data/one.json": "1"})
    b = _tree(tmp_path / "b", {"review.md": "review", "data/two.json": "2"})

    target = tmp_path / "target"
    _seed(target, [("a", a), ("b", b)])

    assert (target / "draft.md").read_text() == "draft"
    assert (target / "review.md").read_text() == "review"
    assert (target / "data" / "one.json").read_text() == "1"
    assert (target / "data" / "two.json").read_text() == "2"


# -- the scheduler, with a stub node -----------------------------------------------------------


class _StubAgent:
    """Writes what the test says, then reports how it got out."""

    def __init__(self, directory: Path, writes: dict[str, str], status: str, route: str | None):
        self.directory = directory
        self.writes = writes
        self.status = status
        self.env = SimpleNamespace(route=route)

    def run(self, task: str) -> dict:
        return self._finish()

    def resume(self, messages: list) -> dict:
        return self._finish()

    def _finish(self) -> dict:
        _tree(self.directory, self.writes)
        return {"submission": "done", "exit_status": self.status}


def _workspace(tmp_path: Path, graph: dict) -> Path:
    workspace = tmp_path / "workspace"
    workspace.mkdir(parents=True, exist_ok=True)
    (workspace / "graph.json").write_text(json.dumps(graph), encoding="utf-8")
    return workspace


def _stub_nodes(monkeypatch, behaviour):
    """Replace the model-backed node with one whose behaviour the test dictates."""
    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))

    def fake_agent_for(graph, node_id, directory, models, secret_file, config_path):
        writes, status, route = behaviour(node_id)
        return _StubAgent(Path(directory), writes, status, route)

    monkeypatch.setattr(runner, "_agent_for", fake_agent_for)


def _agent_graph() -> dict:
    return {
        "entry": "a",
        "objective": "test",
        "agents": {"w": {"model": "models.academic"}},
        "nodes": [{"id": "a", "agent": "w"}, {"id": "b", "agent": "w"}, {"id": "c", "agent": "w"}],
        "edges": [],
    }


def test_a_node_stopped_by_the_round_ceiling_is_not_finished(tmp_path, monkeypatch):
    """`max_rounds` turning a pass away means the graph's intent was not carried out.

    A run cut short here used to be reported as `finished`, with the only trace of it a line on
    stdout — the silent stop this runtime exists to stop making.
    """
    graph = {
        "entry": "spin",
        "max_rounds": 2,
        "objective": "test",
        "agents": {"w": {"model": "models.academic"}},
        "nodes": [{"id": "spin", "agent": "w"}],
        "edges": [{"from": "spin", "to": "spin"}],
    }
    workspace = _workspace(tmp_path, graph)
    # Every pass routes to itself, so the ceiling is the only thing that can end this run.
    _stub_nodes(monkeypatch, lambda node_id: ({"spin.txt": node_id}, "Submitted", "spin"))

    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    assert state.status == "stopped", "a truncated run is not a finished one"
    assert state.reason == "max_rounds"
    assert state.ceased == ["spin@2"]
    saved = json.loads(next((workspace / "runs").glob("*/run.json")).read_text())
    assert saved["status"] == "stopped" and saved["reason"] == "max_rounds"
    assert saved["ceased"] == ["spin@2"], "the reason is on disk, not only in stdout"


def test_a_node_that_routes_to_itself_runs_again(tmp_path, monkeypatch):
    """A self-edge has to look newer than the run that wrote it, or the loop is quietly dropped.

    `_record` used to stamp the decision with the sequence its own execution started at, so the edge
    looked older than the node it pointed back to. The node ran once, nothing was ready, and the run
    reported `finished` — a loop the graph asked for and did not get.
    """
    graph = {
        "entry": "spin",
        "max_rounds": 3,
        "objective": "test",
        "agents": {"w": {"model": "models.academic"}},
        "nodes": [{"id": "spin", "agent": "w"}],
        "edges": [{"from": "spin", "to": "spin"}],
    }
    workspace = _workspace(tmp_path, graph)
    _stub_nodes(monkeypatch, lambda node_id: ({"spin.txt": node_id}, "Submitted", "spin"))

    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    assert state.passes["spin"] == 3, "the ceiling is what stopped it, so it ran three times"
    assert state.executed == ["spin"] * 3


def test_an_input_collision_fails_the_run_and_says_why(tmp_path, monkeypatch, capsys):
    """Two branches feeding one node with the same filename is a graph error, not a node failure.

    It is caught when the node is seeded, recorded in `run.json`, and returned as a status instead of
    raised as a traceback — a resume cannot get past it, because the same two directories would be
    seeded again.
    """
    graph = {
        "entry": "a",
        "objective": "test",
        "agents": {"w": {"model": "models.academic"}},
        "nodes": [{"id": "a", "agent": "w"}, {"id": "b", "agent": "w"}, {"id": "c", "agent": "w"}],
        # Both `a` and `b` have no selected input of their own, and both point at `c`.
        "edges": [{"from": "a", "to": "c"}, {"from": "b", "to": "c"}],
    }
    workspace = _workspace(tmp_path, graph)
    _stub_nodes(monkeypatch, lambda node_id: ({"notes.md": f"from {node_id}"}, "Submitted", None))

    state = runner.run(workspace, config_path=tmp_path / "unused.json")
    capsys.readouterr()

    assert state.status == "failed"
    assert state.reason == "input_collision"
    assert "notes.md" in state.error and "a" in state.error and "b" in state.error
    saved = json.loads(next((workspace / "runs").glob("*/run.json")).read_text())
    assert saved["status"] == "failed" and saved["reason"] == "input_collision"


def test_a_run_that_merely_runs_out_of_nodes_is_finished(tmp_path, monkeypatch):
    """The ordinary ending still says `finished`, so the new distinction means something."""
    graph = _agent_graph()
    graph["edges"] = [{"from": "a", "to": "b"}]
    graph["nodes"] = [{"id": "a", "agent": "w"}, {"id": "b", "agent": "w"}]
    workspace = _workspace(tmp_path, graph)
    _stub_nodes(monkeypatch, lambda node_id: ({f"{node_id}.txt": node_id}, "Submitted", None))

    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    assert state.status == "finished"
    assert state.reason == "" and state.ceased == []


def test_a_node_that_does_not_submit_still_fails(tmp_path, monkeypatch):
    """The pre-existing invariant, kept honest: text without an action is not a completion."""
    graph = _agent_graph()
    graph["nodes"] = [{"id": "a", "agent": "w"}]
    workspace = _workspace(tmp_path, graph)
    _stub_nodes(monkeypatch, lambda node_id: ({}, "Exited", None))

    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    assert state.status == "failed"


def test_a_module_runs_as_directories_named_for_its_scope(tmp_path, monkeypatch):
    """The scope prefix is a directory name, which is why the runtime needs no change for this.

    A node is `use/a`, so its directory is `runs/<run>/use/a/` and the filesystem mirrors what the
    author drew. Nothing in the scheduler knows a module was involved.
    """
    graph = {
        "entry": "in",
        "objective": "test",
        "agents": {"w": {"model": "models.academic"}},
        "graphs": {"mod": {"entry": "a", "exit": "b",
                           "nodes": [{"id": "a", "agent": "w"}, {"id": "b", "agent": "w"}],
                           "edges": [{"from": "a", "to": "b"}]}},
        "nodes": [{"id": "in", "agent": "w"},
                  {"id": "use", "graph": "mod"},
                  {"id": "out", "agent": "w"}],
        "edges": [{"from": "in", "to": "use"}, {"from": "use", "to": "out"}],
    }
    workspace = _workspace(tmp_path, graph)
    handed: list[str] = []

    def fake_agent_for(_graph, node_id, directory, _models, _secret, _config):
        handed.append(str(Path(directory).relative_to(workspace)))
        return _StubAgent(Path(directory), {f"{node_id.replace('/', '_')}.md": node_id},
                          "Submitted", None)

    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", fake_agent_for)

    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    run_dir = next((workspace / "runs").glob("*"))
    assert state.status == "finished"
    assert state.executed == ["in", "use/a", "use/b", "out"]
    # What the agent was handed is a path that mirrors the scope, so the trace the real agent writes
    # beside it (`agent.py`, `<tree>.trace.jsonl`) lands outside the node's own directory.
    assert handed == [f"runs/{run_dir.name}/in", f"runs/{run_dir.name}/use/a",
                      f"runs/{run_dir.name}/use/b", f"runs/{run_dir.name}/out"]
    for node in ("in", "use/a", "use/b", "out"):
        assert (run_dir / node).is_dir(), f"{node} should have its own directory"
    # And the run records the graph it actually read, modules already inlined.
    written = json.loads((run_dir / "graph.json").read_text(encoding="utf-8"))
    assert [item["id"] for item in written["nodes"]] == ["in", "use/a", "use/b", "out"]
    assert {"from": "use/b", "to": "out"} in written["edges"]
