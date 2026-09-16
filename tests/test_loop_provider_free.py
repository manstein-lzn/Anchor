"""A real loop, end to end, with no provider.

The loop, the sandbox, the mounts, the commits and the record are all the real ones; only the model is
a script. That is what makes a run here evidence about the runtime rather than about a model — and it
costs nothing, which matters when the alternative is paying a provider to find out whether a scheduler
change broke the loop.

It is also the shape that used to be missing: everything else here uses a stubbed agent, so the real
sandbox flags, the real `_freeze`, the real pinned views and the real `anchor-done` contract were never
exercised together — and a bug that made every command fail took two thousand model turns to surface
because of exactly that gap.

It also states a consequence of the pointer design that a graph has to be written around. A node's
output is what it wrote and nothing else, so an artefact does not travel along a chain by itself: the
node in the middle has to carry it forward, or the node after it has no pointer to it. The script below
does that explicitly, and fails loudly rather than quietly if the thing it expects is not there.
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


GRAPH = {
    "entry": "draft",
    "max_rounds": 4,
    "objective": "用一句话说明编译器为什么需要代价模型。",
    "agents": {
        "drafter": {"model": "models.academic",
                    "instructions": "Write one sentence into `draft.md`."},
        "critic": {"model": "models.academic",
                   "instructions": "Carry `draft.md` forward, write `critique.md`, then route."},
        "editor": {"model": "models.academic",
                   "instructions": "Copy the settled draft into `final.md`."},
    },
    "nodes": [{"id": "draft", "agent": "drafter"},
              {"id": "review", "agent": "critic"},
              {"id": "done", "agent": "editor"}],
    "edges": [{"from": "draft", "to": "review"},
              {"from": "review", "to": "draft"},
              {"from": "review", "to": "done"}],
}

SENTENCE = "compilers need cost models to choose plans"

#: Each node's turns. What a node remembers between passes is its own workspace, exactly as it would
#: be for a model: `review` finds `review.md` on its second pass and that is why it leaves the other
#: way. Every read through a pointer is guarded, so a missing mount fails the run instead of quietly
#: producing a different one.
SCRIPT = {
    "draft": [
        f"printf '{SENTENCE}\\n' > draft.md",
        'anchor-done --summary "wrote draft.md"',
    ],
    "review": [
        "test -f /in/draft/draft.md || { echo 'the pointer to draft was not there'; exit 1; }; "
        "if [ -f review.md ]; then "
        "anchor-route --to done --reason 'the sentence is fine now'; "
        "else printf 'asks for one concrete example\\n' > review.md; "
        "anchor-route --to draft --reason 'one more pass'; fi",
    ],
    "done": [
        # Nothing was carried: the reviewer wrote only the review, and the draft is reachable
        # behind the work this node was given.
        "test -f /in/draft/draft.md || { echo 'the draft was out of reach'; exit 1; }; "
        "cp /in/draft/draft.md final.md",
        'anchor-done --summary "produced final.md"',
    ],
}


def _commits(workspace: Path) -> list[str]:
    completed = subprocess.run(["git", "-C", str(workspace), "log", "--format=%s"],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    return [line for line in completed.stdout.splitlines() if line.strip()]


def _run(tmp_path, models: str = "[]"):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "graph.json").write_text(json.dumps(GRAPH), encoding="utf-8")
    # A real profile file, so nothing about the run is stubbed. Nothing reads it either: a scripted
    # node needs no model and no secret, which is the point.
    config = tmp_path / "runtime.json"
    config.write_text(json.dumps({"models": json.loads(models)}), encoding="utf-8")
    return runner.run(workspace, config_path=config, model_script=SCRIPT), workspace


def test_a_loop_runs_end_to_end_without_a_provider(tmp_path):
    state, workspace = _run(tmp_path, models=json.dumps([
        {"ref": "models.academic", "model": "unused", "base_url": "http://127.0.0.1:1",
         "secret_ref": "UNUSED"}]))
    run_dir = next((workspace / "runs").glob("*"))

    assert state.status == "finished", state.error
    assert state.executed == ["draft", "review", "draft", "review", "done"], \
        "the loop should have gone round once and then left"
    assert state.passes == {"draft": 2, "review": 2, "done": 1}

    # A real command in a real sandbox read the settled draft through a pointer and produced a file.
    assert (run_dir / "done" / "final.md").read_text(encoding="utf-8").strip() == SENTENCE

    # One workspace per node, kept across its passes. The second pass of `review` found `review.md`
    # and that is the only reason it chose the other way out.
    assert (run_dir / "review" / "review.md").is_file(), "the workspace did not persist"
    # A commit per pass, in each node's own repository, with the node's own summary as the message.
    assert _commits(run_dir / "draft") == ["wrote draft.md", "wrote draft.md", "start"]
    assert _commits(run_dir / "review") == ["the sentence is fine now", "one more pass", "start"]
    assert _commits(run_dir / "done") == ["produced final.md", "start"]
    # The pointer is pinned, so `draft`'s two passes were handed over as two different commits.
    assert state.nodes["review"]["inputs"] == (("draft", state.nodes["draft"]["commit"]),), \
        "the record has to say which commit this pass read"
    assert len(list((run_dir / ".views").glob("draft-*"))) == 2, \
        "a view per commit, or the first pass would have been read through the second's"
    # And what a node produced is its own: `review` holds its critique and nothing else. It did not
    # carry the draft, and `done` reached the draft anyway.
    assert sorted(item.name for item in (run_dir / "review").iterdir() if item.is_file()) == \
        ["review.md"]
    assert sorted(item.name for item in (run_dir / "done").iterdir() if item.is_file()) == \
        ["final.md"]


# -- a loop inside a loop, and a module looped over by its parent ---------------------------------


def _nested(module_rounds: int, module_node_rounds: int | None = None) -> dict:
    """`review` routes back into the whole `refine` module, whose own nodes form a loop.

    Counting is per level: `refine.max_rounds` bounds its nodes' rounds on each visit, and the
    `work` node's bounds how many times this graph may visit it at all.
    """
    work: dict = {"id": "work", "graph": "refine"}
    if module_node_rounds is not None:
        work["max_rounds"] = module_node_rounds
    return {
        "entry": "plan",
        "max_rounds": 6,
        "objective": "一个模块里有循环，外层又循环回这个模块。",
        "agents": {name: {"model": "models.academic", "instructions": name}
                   for name in ("planner", "drafter", "critic", "editor", "shipper")},
        "graphs": {"refine": {
            "entry": "draft", "exit": "done", "max_rounds": module_rounds,
            "nodes": [{"id": "draft", "agent": "drafter"},
                      {"id": "check", "agent": "critic"},
                      {"id": "done", "agent": "editor"}],
            "edges": [{"from": "draft", "to": "check"}, {"from": "check", "to": "draft"},
                      {"from": "check", "to": "done"}]}},
        "nodes": [{"id": "plan", "agent": "planner"}, work,
                  {"id": "review", "agent": "critic"}, {"id": "ship", "agent": "shipper"}],
        "edges": [{"from": "plan", "to": "work"}, {"from": "work", "to": "review"},
                  {"from": "review", "to": "work"}, {"from": "review", "to": "ship"}],
    }


NESTED_SCRIPT = {
    "plan": ["printf 'plan\\n' > plan.md", 'anchor-done --summary "planned"'],
    # The first pass writes v1 and the second v2, which is what gives the inner loop something to
    # stop on — and both read the upstream through its pointer, not out of their own directory.
    "work/draft": [
        "if [ -f draft.md ]; then printf 'v2\\n' > draft.md; else printf 'v1\\n' > draft.md; fi",
        'anchor-done --summary "drafted"'],
    "work/check": [
        "grep -q v2 /in/work/draft/draft.md 2>/dev/null && "
        "anchor-route --to work/done --reason 'good' || "
        "anchor-route --to work/draft --reason 'again'"],
    "work/done": ["test -f /in/work/draft/draft.md || exit 1; "
                  "cp /in/work/draft/draft.md final.md", 'anchor-done --summary "refined"'],
    # Its own workspace is what tells it which visit it is on, exactly as it would for a model.
    "review": ["if [ -f asked ]; then anchor-route --to ship --reason 'accepted'; "
               "else touch asked; anchor-route --to work/draft --reason 'once more'; fi"],
    "ship": ['anchor-done --summary "shipped"'],
}


def _nested_run(tmp_path, graph: dict):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "graph.json").write_text(json.dumps(graph), encoding="utf-8")
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}', encoding="utf-8")
    state = runner.run(workspace, config_path=config, model_script=NESTED_SCRIPT)
    return state, next((workspace / "runs").glob("*"))


def test_a_loop_inside_a_loop_runs_to_the_end(tmp_path):
    """The outer loop re-enters the module, and the module's own bound is per visit.

    With the module's ceiling at 2 and an inner loop that needs both rounds, an outer loop that comes
    back would previously find the module's entry already turned away: the outer loop spent the inner
    loop's budget, `done` never ran, and the run stopped with `ship` skipped. Counting per level is
    what stops one loop paying for another.
    """
    state, run_dir = _nested_run(tmp_path, _nested(module_rounds=2))

    assert state.status == "finished", (state.status, state.reason, state.ceased)
    assert state.ceased == [], "no ceiling should have been reached"
    assert state.executed == ["plan", "work/draft", "work/check", "work/draft", "work/check",
                              "work/done", "review", "work/draft", "work/check", "work/done",
                              "review", "ship"]
    # Three runs, but never a third round within one visit: the visit it belongs to restarted.
    assert state.runs["work/draft"] == 3
    assert state.passes["work/draft"] == 1, "the last visit's own counting, not the total"
    assert state.activations["work"] == 2
    # One conversation per run, named by the run rather than the round, or the third would have
    # overwritten the first's.
    assert len(list(run_dir.glob("work/draft*.trace.jsonl"))) == 3
    assert len(_commits(run_dir / "work" / "draft")) == 4, "start, then one commit per run"


def test_a_module_can_run_out_of_entries(tmp_path):
    """The other ceiling: how many times the parent may visit a module at all.

    At the level above, a module is a node, so it has a `max_rounds` like any other — and refusing is
    the same refusal, so the module produces no exit and nothing after it becomes ready.
    """
    state, _ = _nested_run(tmp_path, _nested(module_rounds=2, module_node_rounds=1))

    assert state.status == "stopped" and state.reason == "max_rounds"
    assert state.ceased == ["work@1"], "named by the module, not by a node inside it"
    assert state.activations["work"] == 1
    assert "ship" not in state.executed, "the run stopped at the module, as a ceiling does"


def test_the_outer_ceiling_and_the_inner_one_are_separate(tmp_path):
    """Raising the parent's visits lets the same module go round again, with the same inner bound."""
    state, _ = _nested_run(tmp_path, _nested(module_rounds=2, module_node_rounds=3))

    assert state.status == "finished", (state.status, state.ceased)
    assert state.activations["work"] == 2
    assert state.runs["work/draft"] == 3
