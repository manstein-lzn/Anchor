"""The graphs in `examples/` are graphs this runtime can run.

Two things are checked, and the first is the reason the directory has a `previous/` in it at all:
everything here loads. Three files used to sit here that did not — they were written against the
schema of the runtime that was deleted, so nothing could read them and nothing noticed, because no
test had ever looked at an example.

The second is the one that matters for the rewrite they just had. An agent's instructions used to say
"the file is already in your directory", which stopped being true when an edge began carrying a
read-only pointer instead of a copy: the node never saw it, produced the same thing again, and the
loop never converged. A graph can load perfectly and still be wrong in that way, so one of them is run.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox
from anchor.simple import graph as graph_module
from anchor.simple import run as runner

EXAMPLES = Path(__file__).resolve().parents[1] / "examples" / "graphs"


@pytest.mark.parametrize("path", sorted(EXAMPLES.glob("*.json")), ids=lambda p: p.name)
def test_every_example_is_a_graph_this_runtime_can_read(path: Path):
    parsed = graph_module.parse(json.loads(path.read_text(encoding="utf-8")))

    assert parsed.entry
    assert parsed.nodes


def test_the_only_unloadable_examples_are_where_they_say_they_are():
    """The previous design's graphs are kept, and kept out of the way. If one comes back here, this
    fails — which is the whole point, because the last three sat here for months unnoticed."""
    for path in (EXAMPLES / "previous").glob("*.json"):
        with pytest.raises(ValueError):
            graph_module.parse(json.loads(path.read_text(encoding="utf-8")))


@pytest.fixture(scope="module", autouse=True)
def needs_a_sandbox():
    try:
        BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    except RuntimeError as exc:                     # a container that forbids namespaces
        pytest.skip(f"no usable sandbox on this machine: {exc}")


#: The commands `revise-loop.json`'s roles would give, written down instead of asked for. Each one
#: reads through the pointer the graph declares and fails loudly if it is not there, so a graph whose
#: instructions name the wrong place cannot pass by accident.
REVISE_SCRIPT = {
    "draft": [
        "if [ -f /in/review/review.md ]; then printf 'addresses what the review asked for\\n' > draft.md; "
        "else printf 'a first sentence\\n' > draft.md; fi",
        'anchor-done --summary "drafted"',
    ],
    "review": [
        "test -f /in/draft/draft.md || { echo 'the draft was not mounted where it was said to be'; "
        "exit 1; }; cp /in/draft/draft.md ./draft.md; "
        "if [ -f review.md ]; then anchor-route --to done --reason 'the sentence holds'; "
        "else printf 'asks for one concrete example\\n' > review.md; "
        "anchor-route --to draft --reason 'one more pass'; fi",
    ],
    "done": [
        "test -f /in/review/draft.md || { echo 'the reviewer did not carry the draft forward'; exit 1; }; "
        "cp /in/review/draft.md final.md",
        'anchor-done --summary "finished"',
    ],
}


def test_the_revise_loop_actually_revises(tmp_path):
    """Loading is not enough: the second pass has to see the first pass's review.

    The drafter is handed the reviewer read-only at `/in/review`, and its instruction is to rewrite
    `draft.md` when `review.md` is there. While the instructions said "in your directory" it was
    never there, so the drafter wrote the same sentence again, the reviewer asked again, and the two
    spent the whole round ceiling doing nothing. That failure is invisible in the run's status.
    """
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "graph.json").write_bytes((EXAMPLES / "revise-loop.json").read_bytes())
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}', encoding="utf-8")

    state = runner.run(workspace, config_path=config, model_script=REVISE_SCRIPT)
    run_dir = next((workspace / "runs").glob("*"))

    assert state.status == "finished", (state.status, state.reason, state.ceased)
    assert state.executed == ["draft", "review", "draft", "review", "done"]
    # The second draft is the first one revised, which is the whole of what the loop is for.
    assert (run_dir / "done" / "final.md").read_text(encoding="utf-8").strip() == \
        "addresses what the review asked for"
    # And the reviewer's commit carries the draft it was handed, which is how `done` can read it.
    assert (run_dir / "review" / "draft.md").is_file(), "the middle node did not carry it forward"
