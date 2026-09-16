"""An op is a node too, and only the deciding is different.

Same sandbox, same read-only pointers, same commit per pass, same record. What changes is one thing:
a program decides instead of a model, and its exit code is taken as the verdict — a model can talk
itself into believing it has finished, a command cannot.

None of this needs a provider, so all of it runs for nothing. That is deliberate: the point of an op
is that much of a graph can be work that cannot be talked out of being wrong, and testing it should
not cost anything either.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox
from anchor.simple import run as runner


@pytest.fixture(scope="module", autouse=True)
def needs_a_sandbox():
    try:
        BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    except RuntimeError as exc:                     # a container that forbids namespaces
        pytest.skip(f"no usable sandbox on this machine: {exc}")


def _run(tmp_path, graph: dict, script: dict | None = None):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "graph.json").write_text(json.dumps(graph), encoding="utf-8")
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}', encoding="utf-8")
    state = runner.run(workspace, config_path=config, model_script=script)
    return state, next((workspace / "runs").glob("*"))


def _commits(workspace: Path) -> list[str]:
    completed = subprocess.run(["git", "-C", str(workspace), "log", "--format=%s"],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    return [line for line in completed.stdout.splitlines() if line.strip()]


def test_a_graph_of_nothing_but_ops_runs(tmp_path):
    """Two commands, and the second reads the first's output where it is rather than being handed it.

    Nothing here is a model, and the graph still works: the read-only pointer, the commit and the
    record are the runtime's, not the agent's.
    """
    graph = {
        "entry": "make", "objective": "two commands",
        "ops": {
            # Printing is how an op says what it did: its output is the summary, exactly as the line
            # after `anchor-done` is an agent's.
            "make": {"run": "printf 'a note\\n' > notes.md && echo 'wrote notes.md'",
                     "writes": ["notes.md"]},
            # Fails loudly if the pointer is not where the interface says it will be.
            "check": {"run": "test -s /in/make/notes.md && echo 'the note is there'",
                      "reads": ["notes.md"], "writes": []},
        },
        "nodes": [{"id": "make", "op": "make"}, {"id": "check", "op": "check"}],
        "edges": [{"from": "make", "to": "check"}],
    }

    state, run_dir = _run(tmp_path, graph)

    assert state.status == "finished", (state.status, state.error)
    assert state.executed == ["make", "check"]
    assert (run_dir / "make" / "notes.md").read_text(encoding="utf-8") == "a note\n"
    assert state.nodes["make"]["submission"] == "wrote notes.md"
    assert _commits(run_dir / "make") == ["wrote notes.md", "start"]
    assert state.nodes["check"]["submitted"] is True


def test_a_command_that_fails_fails_the_pass(tmp_path):
    """Not a budget exit: a check that did not pass is a failure, and a resume must not pick it up."""
    graph = {
        "entry": "check", "objective": "a check that fails",
        "ops": {"check": {"run": "echo 'the references do not resolve'; exit 1"}},
        "nodes": [{"id": "check", "op": "check"}],
        "edges": [],
    }

    state, _ = _run(tmp_path, graph)

    assert state.status == "failed"
    assert "did not submit" in state.error
    assert state.nodes["check"]["exit_status"] == "Failed"
    assert "the references do not resolve" in state.nodes["check"]["submission"]


def test_a_command_that_is_not_there_says_which_one(tmp_path):
    """A bare 127 is not a diagnosis, and this is the failure an author hits most often."""
    graph = {
        "entry": "check", "objective": "a missing command",
        "ops": {"check": {"run": "anchor-verify-that-does-not-exist --refs sources.md"}},
        "nodes": [{"id": "check", "op": "check"}],
        "edges": [],
    }

    state, _ = _run(tmp_path, graph)

    assert state.status == "failed"
    assert state.nodes["check"]["exit_status"] == "CommandNotFound"
    assert "not on the sandbox PATH" in state.nodes["check"]["submission"]


def test_an_op_that_chooses_has_to_choose(tmp_path):
    """Same rule as an agent's: a node with more than one way out names one, or the pass fails."""
    graph = {
        "entry": "check", "objective": "an op that does not route",
        "ops": {"check": {"run": "true && echo 'checked'"},
                "end": {"run": "true"}},
        "nodes": [{"id": "check", "op": "check"}, {"id": "yes", "op": "end"},
                  {"id": "no", "op": "end"}],
        "edges": [{"from": "check", "to": "yes"}, {"from": "check", "to": "no"}],
    }

    state, _ = _run(tmp_path, graph)

    assert state.status == "failed"
    assert "did not route" in state.nodes["check"]["submission"]


def test_an_op_routes_by_its_exit_code(tmp_path):
    """How a deterministic check becomes a gate: the branch is taken by a real check, not by a
    model's judgement about one."""
    graph = {
        "entry": "make", "objective": "a gate",
        "ops": {
            "make": {"run": "printf 'a note\\n' > notes.md && echo 'made it'",
                     "writes": ["notes.md"]},
            "check": {"run": "test -s /in/make/notes.md "
                             "&& anchor-route --to good || anchor-route --to bad",
                      "reads": ["notes.md"]},
            "good": {"run": "printf 'passed\\n' > verdict.md && echo passed",
                     "writes": ["verdict.md"]},
            "bad": {"run": "printf 'failed\\n' > verdict.md && echo failed",
                    "writes": ["verdict.md"]},
        },
        "nodes": [{"id": "make", "op": "make"}, {"id": "check", "op": "check"},
                  {"id": "good", "op": "good"}, {"id": "bad", "op": "bad"}],
        # Two ways out of the gate and no join. A join would wait on an edge the branch not taken
        # never decides, which is a property of graphs rather than of ops.
        "edges": [{"from": "make", "to": "check"}, {"from": "check", "to": "good"},
                  {"from": "check", "to": "bad"}],
    }

    state, run_dir = _run(tmp_path, graph)

    assert state.status == "finished", (state.status, state.error, state.ceased)
    assert state.executed == ["make", "check", "good"]
    assert state.nodes["check"]["route"] == "good"
    assert (run_dir / "good" / "verdict.md").read_text(encoding="utf-8") == "passed\n"


def test_an_agent_and_an_op_are_the_same_kind_of_thing_to_the_graph(tmp_path):
    """The claim the design rests on: two kinds of node, one interface.

    A scripted agent writes a draft; an op checks it and routes on the result. Neither knows what the
    other is — they share the workspace, the pointer, the commit and the completion contract.
    """
    graph = {
        "entry": "write", "objective": "one of each",
        "agents": {"writer": {"model": "models.academic", "writes": ["paper.md"]}},
        "ops": {
            "has-title": {"run": "grep -q '^# ' /in/write/paper.md "
                                 "&& anchor-route --to ship || exit 1",
                          "reads": ["paper.md"]},
            "ship": {"run": "test -s /in/write/paper.md && echo shipped",
                     "reads": ["paper.md"]},
        },
        "nodes": [{"id": "write", "agent": "writer"}, {"id": "verify", "op": "has-title"},
                  {"id": "ship", "op": "ship"}, {"id": "redo", "op": "ship"}],
        "edges": [{"from": "write", "to": "verify"}, {"from": "verify", "to": "ship"},
                  {"from": "verify", "to": "redo"}],
    }
    script = {"write": ["printf '# Cost models\\n\\nA sentence.\\n' > paper.md",
                        'anchor-done --summary "wrote the paper"']}

    state, run_dir = _run(tmp_path, graph, script)

    assert state.status == "finished", (state.status, state.error, state.ceased)
    assert state.executed == ["write", "verify", "ship"]
    assert state.nodes["verify"]["route"] == "ship"
    # The op read the paper through a pointer, so its own workspace never held it.
    assert not (run_dir / "verify" / "paper.md").exists()
    assert _commits(run_dir / "write") == ["wrote the paper", "start"]
