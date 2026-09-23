"""What a run records, and what it refuses to call success.

Neither needs a model. The scheduler is the thing under test, so the node is replaced by a stub that
does exactly what a test says and nothing else — the point is the record, not what a model would have
written. Everything here runs git for real, because the commit is part of the record.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

from anchor.simple import run as runner


def _tree(root: Path, files: dict[str, str]) -> Path:
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return root


def _commits(workspace: Path) -> list[str]:
    """The messages in a node's history, newest first."""
    completed = subprocess.run(["git", "-C", str(workspace), "log", "--format=%s"],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    return [line for line in completed.stdout.splitlines() if line.strip()]


# -- the scheduler, with a stub node -----------------------------------------------------------


class _StubAgent:
    """Writes what the test says, then reports how it got out."""

    def __init__(self, directory: Path, writes: dict[str, str], status: str, route: str | None):
        self.directory = directory
        self.writes = writes
        self.status = status
        self.env = SimpleNamespace(route=route)

    def run(self, task: str, **_kwargs: object) -> dict:
        return self._finish()

    def resume(self) -> dict:
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

    def fake_agent_for(graph, node_id, directory, models, secret_file, config_path,
                       inputs=(), trace=None, script=None, **kwargs):
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


def _self_loop(ceiling: int) -> dict:
    return {
        "entry": "spin",
        "max_rounds": ceiling,
        "objective": "test",
        "agents": {"w": {"model": "models.academic"}},
        "nodes": [{"id": "spin", "agent": "w"}],
        "edges": [{"from": "spin", "to": "spin"}],
    }


def test_spent_node_budget_stops_without_retrying(tmp_path, monkeypatch):
    workspace = _workspace(tmp_path, _agent_graph())
    calls = []

    def behaviour(node_id):
        calls.append(node_id)
        return {}, "budget_exhausted", None

    _stub_nodes(monkeypatch, behaviour)
    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    assert calls == ["a"]
    assert state.status == "stopped" and state.reason == "budget_exhausted"
    assert state.cursor is not None and state.cursor["node"] == "a"


# -- pointers rather than copies -----------------------------------------------------------------


def test_what_a_node_is_given_is_mounted_and_not_copied(tmp_path, monkeypatch):
    """An edge carries a pointer. Nothing is copied, so a large predecessor costs nothing to hand on.

    The corollary is the one that matters for reading a run: a node's directory holds exactly what
    that node produced, so its files can be attributed to it without keeping a list of what it was
    handed. The old design copied the whole lineage in, which is how a node came to be told that its
    predecessor had written files three nodes earlier.
    """
    graph = _agent_graph()
    graph["nodes"] = [{"id": "a", "agent": "w"}, {"id": "b", "agent": "w"}]
    graph["edges"] = [{"from": "a", "to": "b"}]
    workspace = _workspace(tmp_path, graph)
    seen: dict[str, dict] = {}

    def fake_agent_for(_graph, node_id, directory, _models, _secret, _config, inputs=(), trace=None, script=None, **kwargs):
        seen[node_id] = {"directory": Path(directory), "inputs": inputs, "trace": trace}
        return _StubAgent(Path(directory), {f"{node_id}.md": node_id}, "Submitted", None)

    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", fake_agent_for)

    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    run_dir = next((workspace / "runs").glob("*"))
    assert state.status == "finished"
    assert seen["a"]["inputs"] == (), "the entry node was given nothing"
    given = seen["b"]["inputs"][0]
    assert (given.node_id, given.mount) == ("a", "/in/a")
    assert given.commit == state.nodes["a"]["commit"], "the pointer names the commit it was frozen at"
    assert given.tree != run_dir / "a", "what is mounted is a view of that commit, not the directory"
    assert (run_dir / "b" / "b.md").is_file()
    assert not (run_dir / "b" / "a.md").exists(), "the input was copied into the node's workspace"
    assert (run_dir / "a" / "a.md").is_file(), "it is still where it was, which is what is pointed at"


def test_each_pass_is_frozen_as_a_commit(tmp_path, monkeypatch):
    """The workspace is reused across passes, so the commit is what makes one pass readable later."""
    workspace = _workspace(tmp_path, _self_loop(3))
    _stub_nodes(monkeypatch, lambda node_id: ({"spin.txt": node_id}, "Submitted", "spin"))

    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    run_dir = next((workspace / "runs").glob("*"))
    assert [state.nodes["spin"]["commit"]] and state.nodes["spin"]["commit"]
    # `start` from the empty first commit, then one per pass.
    assert _commits(run_dir / "spin") == ["done", "done", "done", "start"]
    saved = json.loads((run_dir / "run.json").read_text(encoding="utf-8"))
    assert saved["nodes"]["spin"]["commit"] == state.nodes["spin"]["commit"], \
        "the run points at the commit it recorded"


def test_a_node_keeps_its_workspace_between_passes(tmp_path, monkeypatch):
    """A node revising its own work needs to still have it, and it does: the same directory.

    Nothing seeds it on the second pass. What the node wrote is simply still there, which is what the
    loop that revises a draft is built on.
    """
    workspace = _workspace(tmp_path, _self_loop(3))
    found: list[list[str]] = []

    def fake_agent_for(_graph, node_id, directory, _models, _secret, _config, inputs=(), trace=None, script=None, **kwargs):
        found.append(sorted(item.name for item in Path(directory).iterdir() if item.is_file()))
        return _StubAgent(Path(directory), {f"pass{len(found)}.md": "x"}, "Submitted", "spin")

    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", fake_agent_for)

    runner.run(workspace, config_path=tmp_path / "unused.json")

    assert found[0] == [], "the first pass starts empty"
    assert found[1] == ["pass1.md"], "the second sees what the first left, with no seeding step"
    assert found[2] == ["pass1.md", "pass2.md"]


def test_each_pass_gets_its_own_conversation(tmp_path, monkeypatch):
    """One trace per pass. Two passes appended to one file would replay as two conversations."""
    workspace = _workspace(tmp_path, _self_loop(3))
    traces: list[Path | None] = []
    controls: list[Path] = []

    def fake_agent_for(_graph, node_id, directory, _models, _secret, _config, inputs=(), trace=None, script=None, **kwargs):
        traces.append(trace)
        controls.append(kwargs["control"])
        return _StubAgent(Path(directory), {f"pass{len(traces)}.md": "x"}, "Submitted", "spin")

    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", fake_agent_for)

    runner.run(workspace, config_path=tmp_path / "unused.json")

    assert len(set(traces)) == 3, f"expected one per pass, got {traces}"
    run_dir = next((workspace / "runs").glob("*"))
    assert (run_dir / "spin.trace.jsonl") == traces[0]
    assert (run_dir / "spin-2.trace.jsonl") == traces[1]
    assert controls == [run_dir / "control" / name for name in ("spin", "spin-2", "spin-3")]


def test_a_completion_fact_is_read_from_its_control_directory(tmp_path):
    from anchor.node.recovery import CompletionFact, record_completion

    control = tmp_path / "control" / "spin"
    record_completion(control, CompletionFact(node="spin", run="spin-a1", kind="done",
                                              submission="finished", route=None,
                                              command="anchor-done", at="now"))

    assert runner._completion_of(control, "spin") == ("finished", None)


# -- the ways a run could report success for work it did not do -----------------------------------


def test_a_node_stopped_by_the_round_ceiling_is_not_finished(tmp_path, monkeypatch):
    """`max_rounds` turning a pass away means the graph's intent was not carried out.

    A run cut short here used to be reported as `finished`, with the only trace of it a line on
    stdout — the silent stop this runtime exists to stop making.
    """
    workspace = _workspace(tmp_path, _self_loop(2))
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
    workspace = _workspace(tmp_path, _self_loop(3))
    _stub_nodes(monkeypatch, lambda node_id: ({"spin.txt": node_id}, "Submitted", "spin"))

    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    assert state.passes["spin"] == 3, "the ceiling is what stopped it, so it ran three times"
    assert state.executed == ["spin"] * 3


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
    seen: dict[str, tuple] = {}

    def fake_agent_for(_graph, node_id, directory, _models, _secret, _config, inputs=(), trace=None, script=None, **kwargs):
        seen[node_id] = inputs
        return _StubAgent(Path(directory), {f"{node_id.replace('/', '_')}.md": node_id},
                          "Submitted", None)

    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", fake_agent_for)

    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    run_dir = next((workspace / "runs").glob("*"))
    assert state.status == "finished"
    assert state.executed == ["in", "use/a", "use/b", "out"]
    for node in ("in", "use/a", "use/b", "out"):
        assert (run_dir / node).is_dir(), f"{node} should have its own directory"
    # The scope travels in the pointer too, so a module's node is not confused with one of the same
    # name elsewhere in the graph.
    assert [(g.node_id, g.mount) for g in seen["use/b"] if g.direct] == [("use/a", "/in/use/a")]
    assert [(g.node_id, g.mount) for g in seen["out"] if g.direct] == [("use/b", "/in/use/b")]
    # And the run records the graph it actually read, modules already inlined.
    written = json.loads((run_dir / "graph.json").read_text(encoding="utf-8"))
    assert [item["id"] for item in written["nodes"]] == ["in", "use/a", "use/b", "out"]
    assert {"from": "use/b", "to": "out"} in written["edges"]


# -- and the claim that a run survives the process holding it -------------------------------------


class _DiedHere(RuntimeError):
    """What a killed process leaves: a cursor, a conversation, and nothing else."""


class _CrashingStub:
    """Writes a conversation, then dies. That is the shape `--resume` exists to pick up."""

    def __init__(self, directory: Path, trace: Path):
        self.directory = Path(directory)
        self.trace = Path(trace)
        self.env = SimpleNamespace(route=None)

    def run(self, task: str, **_kwargs: object) -> dict:
        self.trace.parent.mkdir(parents=True, exist_ok=True)
        self.trace.write_text(json.dumps({"role": "user", "content": task}) + "\n", encoding="utf-8")
        raise _DiedHere("the process died here")

    def resume(self) -> dict:
        raise AssertionError("a crashed attempt does not resume itself")


class _ResumingStub:
    """Continues a conversation that exists, and refuses to start over instead."""

    def __init__(self, directory: Path):
        self.directory = Path(directory)
        self.env = SimpleNamespace(route=None)

    def run(self, task: str, **_kwargs: object) -> dict:
        raise AssertionError("a resumed node must be continued, not started again")

    def resume(self) -> dict:
        # Nothing is handed in and nothing is read here: what a node continues from is its own step
        # store, and a stub that wanted the conversation would be asserting a coupling that is gone.
        _tree(self.directory, {"b.md": "finished after the restart"})
        return {"submission": "continued", "exit_status": "Submitted"}


def test_a_run_continues_where_a_dead_process_left_it(tmp_path, monkeypatch):
    """The layout changed underneath this and nothing tested it.

    One workspace per node, one trace per pass — `resume` has to find both, from a `run.json` written
    before either existed in that shape. A resume that cannot find its conversation is a run that
    silently starts the node over, which is the failure this runtime is built to refuse.
    """
    graph = _agent_graph()
    graph["nodes"] = [{"id": "a", "agent": "w"}, {"id": "b", "agent": "w"}]
    graph["edges"] = [{"from": "a", "to": "b"}]
    workspace = _workspace(tmp_path, graph)
    state = {"crashed": False}

    def fake_agent_for(_graph, node_id, directory, _models, _secret, _config, inputs=(), trace=None, script=None, **kwargs):
        if node_id == "b":
            if not state["crashed"]:
                state["crashed"] = True
                return _CrashingStub(Path(directory), trace)
            return _ResumingStub(Path(directory))
        return _StubAgent(Path(directory), {"a.md": "from a"}, "Submitted", None)

    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", fake_agent_for)

    # A crash is loud *and* durable: it reaches the caller as an exception, and `run.json` says
    # `interrupted` with a cursor, so the run can be picked up by a process that was not there.
    with pytest.raises(_DiedHere):
        runner.run(workspace, config_path=tmp_path / "unused.json")
    run_dir = next((workspace / "runs").glob("*"))
    first = json.loads((run_dir / "run.json").read_text(encoding="utf-8"))

    assert first["status"] == "interrupted", first["error"]
    assert first["cursor"]["node"] == "b" and first["cursor"]["pass"] == 1
    assert (run_dir / "b.trace.jsonl").is_file(), "the trace is where resume will look for it"

    second = runner.run(workspace, config_path=tmp_path / "unused.json", resume=run_dir)

    assert second.status == "finished", second.error
    assert second.executed == ["a", "b"]
    assert (run_dir / "b" / "b.md").read_text(encoding="utf-8") == "finished after the restart"
    assert second.nodes["b"]["commit"], "the resumed pass was frozen like any other"
    assert _commits(run_dir / "b")[0] == "continued"


def test_the_task_names_where_what_it_was_given_is_mounted():
    """The prompt is the only place a node learns the pointer exists, so it has to name the path.

    A node that does not know where its inputs are looks for them in its own workspace, finds
    nothing, and writes something anyway — which is the failure the pointer design is meant to make
    impossible to reach by accident.
    """
    from anchor.simple.run import NodeResult, _Given, _task

    graph = runner.graph_module.parse({
        "entry": "in",
        "objective": "test",
        "agents": {"w": {"model": "m"}},
        "nodes": [{"id": "in", "agent": "w"}, {"id": "out", "agent": "w"}],
        "edges": [{"from": "in", "to": "out"}],
    })
    upstream = NodeResult(node_id="in", agent="w", tree="/somewhere/in", pass_number=1,
                          submission="wrote notes", files=("notes.md",), submitted=True,
                          exit_status="Submitted", commit="abc123def456789")
    given = _Given(node_id="in", commit="abc123def456789", mount="/in/in", tree=Path("/views/in"))

    task = _task(graph, "out", "the objective", [upstream], (given,))

    assert "/in/in" in task, "the mount point has to be named"
    assert "abc123def456" in task, "and which commit it is, because that is what the pointer names"
    assert "read-only" in task, "and that it cannot be written to"
    assert "wrote notes" in task and "notes.md" in task, "and what is behind it"
    assert "git --git-dir=/in/in/.git log" in task, "and that the history is there too"
    assert "/workspace" in task, "and which directory is its own"


def test_a_pointer_is_a_commit_and_not_a_directory_that_moved_since(tmp_path, monkeypatch):
    """In a loop the same node runs again and its directory changes under whoever read it.

    A directory is live. `a@2` writes over `a@1`, so a node handed "a" would read whichever pass
    happened to be current — neither reproducible nor answerable afterwards. What `b` was handed the
    first time has to still say `v1` when the run is over and `a` says `v2`.
    """
    graph = {
        "entry": "a",
        "max_rounds": 2,
        "objective": "test",
        "agents": {"w": {"model": "models.academic"}},
        "nodes": [{"id": "a", "agent": "w"}, {"id": "b", "agent": "w"}],
        "edges": [{"from": "a", "to": "b"}, {"from": "b", "to": "a"}],
    }
    workspace = _workspace(tmp_path, graph)
    handed: list = []
    passes = {"a": 0}

    def fake_agent_for(_graph, node_id, directory, _models, _secret, _config, inputs=(), trace=None, script=None, **kwargs):
        if node_id == "b":
            handed.append(inputs[0])
            return _StubAgent(Path(directory), {"b.md": "seen"}, "Submitted", "a")
        passes["a"] += 1
        return _StubAgent(Path(directory), {"a.md": f"v{passes['a']}"}, "Submitted", "b")

    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", fake_agent_for)

    runner.run(workspace, config_path=tmp_path / "unused.json")

    run_dir = next((workspace / "runs").glob("*"))
    assert len(handed) == 2, "the loop should have gone round twice"
    assert handed[0].commit != handed[1].commit, "each pass is its own commit"
    assert (handed[0].tree / "a.md").read_text(encoding="utf-8") == "v1", \
        "what b was given first changed underneath it"
    assert (handed[1].tree / "a.md").read_text(encoding="utf-8") == "v2"
    assert (run_dir / "a" / "a.md").read_text(encoding="utf-8") == "v2", \
        "and the node's own workspace is still the live one it is standing in"


def test_a_loop_of_two_that_exceeds_its_ceiling_stops_instead_of_spinning(tmp_path, monkeypatch):
    """Settling the turned-away node's out-edges is not enough on its own.

    In a cycle of two or more the node stays "fresh" through the edge coming back into it, so it was
    chosen again, turned away again, and the run printed `stopped` forever without ending. A self-loop
    escapes this by accident — settling its own edge is what makes it unready — which is why the test
    above passed while this shape hung the suite.
    """
    graph = {
        "entry": "a",
        "max_rounds": 2,
        "objective": "test",
        "agents": {"w": {"model": "models.academic"}},
        "nodes": [{"id": "a", "agent": "w"}, {"id": "b", "agent": "w"}],
        "edges": [{"from": "a", "to": "b"}, {"from": "b", "to": "a"}],
    }
    workspace = _workspace(tmp_path, graph)
    _stub_nodes(monkeypatch, lambda node_id: ({f"{node_id}.md": "x"}, "Submitted",
                                              "b" if node_id == "a" else "a"))

    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    assert state.executed == ["a", "b", "a", "b"], "each ran to its ceiling and no further"
    assert state.ceased == ["a@2"], "the pass that was turned away is on the record"
    assert state.status == "stopped" and state.reason == "max_rounds"


# -- materializing a commit's tree ---------------------------------------------------------------


def test_a_node_that_wrote_nothing_still_has_a_view(tmp_path, monkeypatch):
    """A node that only routed is an ordinary node: empty commit, empty tree, and a real pointer.

    `git archive` answers an empty tree with an archive Python's `tarfile` refuses outright
    (`end of file header`), so a graph with a node that wrote nothing failed at the next node's
    startup — reported as a tar error about a commit that was perfectly fine.
    """
    from anchor.simple.run import _materialize

    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=A", "-c", "user.email=a@b",
                    "commit", "-q", "--allow-empty", "-m", "nothing"], check=True)
    commit = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                            capture_output=True, text=True).stdout.strip()

    view = _materialize(repo, commit, tmp_path / "view")

    assert view.is_dir()
    assert [item.name for item in view.iterdir()] == [".git"], "an empty tree, and the mount point"


def test_a_view_that_failed_to_build_is_not_reused(tmp_path):
    """The guard returns an existing view, so a half-written one under the real name is mounted as
    if it were the commit — which is the one way this can go wrong without saying so."""
    from anchor.simple.run import _materialize

    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    into = tmp_path / "view"

    for attempt in range(2):
        with pytest.raises(RuntimeError, match="cannot read commit"):
            _materialize(repo, "0" * 40, into)
        assert not into.exists(), f"attempt {attempt} left something behind for the next call to use"


def test_a_snapshot_keeps_a_symlink_a_node_left_behind(tmp_path):
    """The tree at a commit is the snapshot, and a symlink is part of it.

    `tarfile`'s strictest filter refuses a link to an absolute path, so one node leaving
    `ln -s /usr/bin/python3 .` behind made the next node fail to start — reported as a tar error about
    a link in a commit that was perfectly fine. Inside the sandbox the link resolves to the sandbox's
    own `/usr`, which is bound read-only, so keeping it grants nothing that was not already there.
    """
    from anchor.simple.run import _materialize

    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    (repo / "notes.md").write_text("kept\n", encoding="utf-8")
    (repo / "run.sh").write_text("#!/bin/sh\n", encoding="utf-8")
    (repo / "run.sh").chmod(0o755)
    os.symlink("notes.md", repo / "rel")
    os.symlink("/usr/bin/python3", repo / "abs")
    (repo / ".gitignore").write_text("scratch.txt\n", encoding="utf-8")
    (repo / "scratch.txt").write_text("not committed\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=A", "-c", "user.email=a@b",
                    "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=A", "-c", "user.email=a@b",
                    "commit", "-q", "-m", "one"], check=True)
    commit = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                            capture_output=True, text=True).stdout.strip()

    view = _materialize(repo, commit, tmp_path / "view")

    assert (view / "notes.md").read_text(encoding="utf-8") == "kept\n"
    assert os.readlink(view / "rel") == "notes.md"
    assert os.readlink(view / "abs") == "/usr/bin/python3", "an absolute link is still the snapshot"
    assert (view / "run.sh").stat().st_mode & 0o111, "the exec bit is part of it too"
    # And what the node chose not to commit is not handed on: that is the honest reading of
    # `.gitignore`, and it is what makes the pointer a snapshot rather than an approximation.
    assert not (view / "scratch.txt").exists()


# -- what a node can reach, and what it must not grow into ---------------------------------------


def _chain(edges, nodes=("a", "b", "c")):
    return {"entry": nodes[0], "objective": "test",
            "agents": {"w": {"model": "models.academic"}},
            "nodes": [{"id": n, "agent": "w"} for n in nodes],
            "edges": [{"from": s, "to": t} for s, t in edges]}


def test_a_node_reaches_the_work_behind_its_inputs(tmp_path, monkeypatch):
    """An output is only what a node left in its own workspace, so nothing carries a plan down a chain.

    Under copying, `c` would have received `a`'s file for free. Under pointers it receives a pointer
    to `b` and nothing else — so either `b` copies `a` forward, by hand and by memory, or the work
    behind an input is mounted as well. This is the second, and it is the whole reason the examples
    stopped needing those copying instructions.
    """
    workspace = _workspace(tmp_path, _chain([("a", "b"), ("b", "c")]))
    seen: dict[str, tuple] = {}

    def fake_agent_for(_g, node_id, directory, _m, _s, _c, inputs=(), trace=None, script=None, **kwargs):
        seen[node_id] = inputs
        return _StubAgent(Path(directory), {f"{node_id}.md": node_id}, "Submitted", None)

    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", fake_agent_for)

    runner.run(workspace, config_path=tmp_path / "unused.json")

    assert [g.node_id for g in seen["a"]] == []
    assert [g.node_id for g in seen["b"] if g.direct] == ["a"]
    assert [g.node_id for g in seen["c"] if g.direct] == ["b"]
    assert [g.node_id for g in seen["c"] if not g.direct] == ["a"], \
        "c has an edge from b, and b was built from a — that is what it can reach"
    # Reached rather than given: the difference is only what the prompt pushes.
    assert all(not g.direct for g in seen["c"] if g.node_id == "a")


def test_what_a_node_is_handed_does_not_grow_with_the_loop(tmp_path, monkeypatch):
    """A back edge carries the loop's current state, which has already superseded the round before.

    Following one pulls in every earlier round, so what a node is handed would grow with how long the
    run had been going rather than with the shape of the graph: measured on a loop like this one, three
    commits in the first round, nine by the third and thirty by the tenth. It is a constant here.
    """
    graph = _chain([("a", "b"), ("b", "a")], nodes=("a", "b"))
    graph["max_rounds"] = 2
    workspace = _workspace(tmp_path, graph)
    sizes: list[int] = []

    def fake_agent_for(_g, node_id, directory, _m, _s, _c, inputs=(), trace=None, script=None, **kwargs):
        sizes.append(len(inputs))
        return _StubAgent(Path(directory), {f"{node_id}.md": node_id}, "Submitted",
                          "b" if node_id == "a" else "a")

    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", fake_agent_for)

    state = runner.run(workspace, config_path=tmp_path / "unused.json")

    assert state.executed == ["a", "b", "a", "b"], "the loop should have gone round twice"
    assert sizes == [0, 1, 1, 1], f"the third and fourth passes reach no further than the second: {sizes}"
