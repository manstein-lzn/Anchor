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


def test_deep_research_review_can_return_to_the_work_that_is_missing():
    graph = graph_module.parse(json.loads(
        (EXAMPLES / "deep-academic-research.json").read_text(encoding="utf-8")))
    assert set(graph.routes("review-gate")) == {"synthesize", "investigate", "frame", "report"}


def _commits_text(workspace: Path) -> list[str]:
    import subprocess
    completed = subprocess.run(["git", "-C", str(workspace), "log", "--format=%s"],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    return [line for line in completed.stdout.splitlines() if line.strip()]


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
        "exit 1; }; "
        "if [ -f review.md ]; then anchor-route --to done --reason 'the sentence holds'; "
        "else printf 'asks for one concrete example\\n' > review.md; "
        "anchor-route --to draft --reason 'one more pass'; fi",
    ],
    "done": [
        # The reviewer wrote only the review. The draft is here because it is behind what `done`
        # was given, which is the thing that used to need the reviewer to copy it forward by hand.
        "test -f /in/draft/draft.md || { echo 'the draft was out of reach'; exit 1; }; "
        "cp /in/draft/draft.md final.md",
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
    # The reviewer copied nothing: its workspace is its own work, and `done` reached the draft
    # anyway, which is the whole of what used to need the middle node to carry it along.
    assert sorted(item.name for item in (run_dir / "review").iterdir() if item.is_file()) == \
        ["review.md"], "the reviewer carried something forward instead of pointing at it"


#: The commands `academic-gated.json`'s roles would give. The first pass deliberately leaves out the
#: `## References` section, so the op in the middle has something real to refuse — and refusing is
#: the point: a program decides whether the work is done, and the writer cannot argue with it.
GATED_SCRIPT = {
    "plan": ["printf 'one question, two databases\\n' > plan.md",
             'anchor-done --summary "planned it"'],
    "gather": ["printf '1. A paper\\n' > sources.md && printf '1. says a thing\\n' > notes.md",
               'anchor-done --summary "gathered it"'],
    "write": [
        # On the second pass the check's note is among what it was given, so it writes the paper the
        # check asked for. Nothing about that decision is a model's: the file is either there or not.
        "if [ -f /in/structure/check.txt ]; then "
        "printf '# Cost models\\n\\n## Summary\\n\\nx\\n\\n## References\\n\\n1. A paper\\n' > paper.md; "
        "else printf '# Cost models\\n\\n## Summary\\n\\nx\\n' > paper.md; fi",
        'anchor-done --summary "wrote the paper"',
    ],
}


def test_a_gate_of_deterministic_checks_drives_an_agent_loop(tmp_path):
    """An op decides whether the work is done, and the loop goes round because of that decision.

    This is what a second kind of node is for. The writer cannot talk its way past the check: the
    check is `grep`, its verdict is an exit code, and the branch it takes is a real one. The parts
    that need judgement are agents and the part that must not be is not.
    """
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "graph.json").write_bytes((EXAMPLES / "academic-gated.json").read_bytes())
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}', encoding="utf-8")

    state = runner.run(workspace, config_path=config, model_script=GATED_SCRIPT)
    run_dir = next((workspace / "runs").glob("*"))

    assert state.status == "finished", (state.status, state.error, state.ceased)
    assert state.executed == ["plan", "gather", "write", "structure", "write", "structure", "publish"]
    # The op's note is the reason the loop went round, and it is a file the writer could read.
    assert (run_dir / "structure" / "check.txt").read_text(encoding="utf-8") == \
        "title and references present\n"
    # The gate's own commits say what it decided, in its own words — the reason it gave when it
    # routed, which is the same thing an agent's commit message is.
    assert _commits_text(run_dir / "structure") == [
        "structure is sound", "structure check failed", "start"]
    # And the deliverable was assembled by an op out of the checked manuscript, not out of any store.
    assert "## References" in (run_dir / "publish" / "final.md").read_text(encoding="utf-8")


DEEP_RESEARCH_SCRIPT = {
    "frame": [
        "if [ -f /in/challenge/critique.md ]; then "
        "grep -q 'reframe' /in/challenge/critique.md && "
        "test -f /in/investigate/research.md || exit 1; "
        "printf 'revised after evidence\n' > framing.md; "
        "else printf 'initial question\n' > framing.md; fi",
        'anchor-done --summary "framed the question"',
    ],
    "investigate": [
        "test -f /in/frame/framing.md || exit 1; "
        "if [ -f /in/challenge/critique.md ]; then "
        "printf 'tested a counterexample\n' >> research.md; "
        "elif grep -q revised /in/frame/framing.md; then "
        "printf 'followed revised frame\n' >> research.md; "
        "else printf 'mapped the field\n' > research.md; fi; "
        "printf 'retrieved source\n' > sources.md",
        'anchor-done --summary "updated the evidence"',
    ],
    "challenge": [
        "test -f /in/frame/framing.md && test -f /in/investigate/research.md && "
        "test -f /in/investigate/sources.md || exit 1; "
        "if [ ! -f critique.md ]; then "
        "printf 'first critique\nDECISION: investigate\n' > critique.md; "
        "else grep -q 'tested a counterexample' /in/investigate/research.md || exit 1; "
        "printf 'revised understanding after counterexample\nDECISION: synthesize\n' > critique.md; fi",
        'anchor-done --summary "challenged the interpretation"',
    ],
    "synthesize": [
        "test -f /in/frame/framing.md && test -f /in/investigate/research.md && "
        "test -f /in/investigate/sources.md && test -f /in/challenge/critique.md && "
        "if [ -f /in/review/review.md ]; then test -f /in/review-gate/review-gate.md || exit 1; "
        "printf '# Paper\\n## Abstract\\nA revised explanation.\\n## Introduction\\nQuestion.\\n' > answer.md; "
        "else printf '# Paper\\n## Abstract\\nAn initial explanation.\\n## Introduction\\nQuestion.\\n' > answer.md; fi; "
        "printf '## Survey Methodology\\nSources.\\n## Mechanisms\\nEvidence.\\n' >> answer.md; "
        "printf '## Comparative Analysis\\nContrast.\\n## Open Problems\\nLimits.\\n' >> answer.md; "
        "printf '## Threats to Validity\\nSampling.\\n## Conclusion\\nBounded answer.\\n' >> answer.md; "
        "printf '## References\\n[1] Source.\\n' >> answer.md",
        'anchor-done --summary "wrote a paper"',
    ],
    "review": [
        "test -f /in/synthesize/answer.md || exit 1; "
        "if grep -q 'revised explanation' /in/synthesize/answer.md; then "
        "printf 'DECISION: pass\\n' > review.md; "
        "else printf 'DECISION: writing\\n' > review.md; fi",
        'anchor-done --summary "reviewed the paper"',
    ],
}


def test_deep_research_requires_feedback_and_paper_review(tmp_path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "graph.json").write_bytes((EXAMPLES / "deep-academic-research.json").read_bytes())
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}', encoding="utf-8")

    state = runner.run(workspace, config_path=config, model_script=DEEP_RESEARCH_SCRIPT)
    run_dir = next((workspace / "runs").glob("*"))

    assert state.status == "finished", (state.status, state.error, state.ceased)
    assert state.executed == ["frame", "investigate", "challenge", "feedback", "investigate",
                              "challenge", "feedback", "synthesize", "review", "review-gate",
                              "synthesize", "review", "review-gate", "report"]
    paper = (run_dir / "report" / "paper.md").read_text(encoding="utf-8")
    assert "revised explanation" in paper
    assert "## References" in paper


def test_deep_research_reframes_when_evidence_overturns_question(tmp_path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "graph.json").write_bytes((EXAMPLES / "deep-academic-research.json").read_bytes())
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}', encoding="utf-8")
    script = {**DEEP_RESEARCH_SCRIPT, "challenge": [
        "if [ ! -f critique.md ]; then "
        "printf 'reframe the question\\nDECISION: frame\\n' > critique.md; "
        "else printf 'reframed question survived the counterexample\\n' > critique.md; "
        "printf 'DECISION: synthesize\\n' >> critique.md; fi",
        'anchor-done --summary "challenged the frame"',
    ]}

    state = runner.run(workspace, config_path=config, model_script=script)
    run_dir = next((workspace / "runs").glob("*"))

    assert state.status == "finished", (state.status, state.error, state.ceased)
    assert state.executed[:8] == ["frame", "investigate", "challenge", "feedback",
                                  "frame", "investigate", "challenge", "feedback"]
    assert (run_dir / "frame" / "framing.md").read_text(encoding="utf-8") == \
        "revised after evidence\n"


@pytest.mark.parametrize("decision,target", [("research", "investigate"), ("frame", "frame")])
def test_paper_feedback_reaches_research_and_converges_beyond_old_ceiling(tmp_path, decision, target):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "graph.json").write_bytes((EXAMPLES / "deep-academic-research.json").read_bytes())
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}')
    script = {**DEEP_RESEARCH_SCRIPT,
        "frame": [
            "if [ -f /in/review/review.md ]; then "
            "test -f /in/review-gate/review-gate.md || exit 1; "
            "cp /in/review/review.md received.md; fi; printf 'question\\n' > framing.md",
            'anchor-done --summary "framed"'],
        "investigate": [
            "test -f /in/frame/framing.md || exit 1; "
            "if [ -f /in/review/review.md ]; then "
            "test -f /in/synthesize/answer.md || exit 1; "
            "cp /in/review/review.md received.md; fi; "
            "printf 'new evidence\\n' >> research.md; printf 'source\\n' > sources.md",
            'anchor-done --summary "investigated"'],
        "challenge": ["printf 'DECISION: synthesize\\n' > critique.md",
                      'anchor-done --summary "checked evidence"'],
        "review": [
            "n=0; if [ -f count ]; then n=$(cat count); fi; n=$((n+1)); echo $n > count; "
            "if [ $n -le 7 ]; then "
            f"printf 'resolve evidence issue %s\\nDECISION: {decision}\\n' $n > review.md; "
            "else printf 'DECISION: pass\\n' > review.md; fi",
            'anchor-done --summary "reviewed"']}
    state = runner.run(workspace, config_path=config, model_script=script)
    run_dir = next((workspace / "runs").iterdir())
    assert state.status == "finished", (state.reason, state.error)
    assert state.passes["review"] == 8
    assert "resolve evidence issue 7" in (run_dir / target / "received.md").read_text()
    assert (run_dir / "report" / "paper.md").is_file()
