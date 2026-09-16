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
        "cp /in/draft/draft.md ./draft.md; "
        "if [ -f review.md ]; then "
        "anchor-route --to done --reason 'the sentence is fine now'; "
        "else printf 'asks for one concrete example\\n' > review.md; "
        "anchor-route --to draft --reason 'one more pass'; fi",
    ],
    "done": [
        "test -f /in/review/draft.md || { echo 'review did not carry the draft forward'; exit 1; }; "
        "cp /in/review/draft.md final.md",
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
    # And what a node produced is its own: `review` holds the critique and the draft it carried
    # forward, and nothing else — not a copy of everything before it.
    assert sorted(item.name for item in (run_dir / "review").iterdir() if item.is_file()) == \
        ["draft.md", "review.md"]
    assert sorted(item.name for item in (run_dir / "done").iterdir() if item.is_file()) == \
        ["final.md"]
