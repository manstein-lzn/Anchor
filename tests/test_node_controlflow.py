"""The acceptance matrix for the PydanticAI node, run against the real thing.

Only the model is replaced. The sandbox is bubblewrap, the completion commands are the installed
console scripts, the mounts are real read-only binds and the files are real files — because the
question this package answers is whether a framework can keep Anchor's execution semantics, and a test
with a stubbed sandbox would answer a question about the stub.

Every model call is a `FunctionModel`, so nothing here reaches a provider and the whole file is free.
"""

from __future__ import annotations

import asyncio
import json
import subprocess
import sys
from pathlib import Path

import pytest

pydantic_ai = pytest.importorskip("pydantic_ai", reason="the optional adapter dependency is not installed")

from pydantic_ai.messages import ModelResponse, TextPart, ToolCallPart          # noqa: E402
from pydantic_ai.models.function import FunctionModel                          # noqa: E402

from anchor.node import BUDGET_EXHAUSTED, COMPLETED, FAILED, NodeRequest, NodeOutcome  # noqa: E402
from anchor.node.pydantic_adapter import (                                      # noqa: E402
    DONE_SENTINEL, ROUTE_SENTINEL, read_completion, run_node,
)
from anchor.runtime.execenv import Executed, NodeSandbox                        # noqa: E402
from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox                   # noqa: E402


@pytest.fixture(scope="module", autouse=True)
def needs_a_sandbox():
    try:
        BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    except RuntimeError as exc:                     # a container that forbids namespaces
        pytest.skip(f"no usable sandbox on this machine: {exc}")


# ── the deterministic model ──────────────────────────────────────────────────────────────────────

def model_from(*turns, on_extra=None) -> FunctionModel:
    """One entry per model request, in order.

    An entry is a list of commands, emitted as bash calls in one response, or a string, emitted as
    plain text. `on_extra` answers requests past the end, which is how a test proves the loop stopped
    rather than being asked again.
    """
    def fn(messages, info):
        served = sum(1 for message in messages if getattr(message, "kind", "") == "response")
        if served >= len(turns):
            if on_extra is not None:
                return on_extra(served)
            return ModelResponse(parts=[TextPart(content="nothing further")])
        entry = turns[served]
        if isinstance(entry, str):
            return ModelResponse(parts=[TextPart(content=entry)])
        return ModelResponse(parts=[ToolCallPart(tool_name="bash", args={"command": command})
                                    for command in entry])
    return FunctionModel(fn)


def request(workspace: Path, **overrides) -> NodeRequest:
    fields = {"execution_id": "exec-1", "task": "do the thing", "workspace": workspace}
    fields.update(overrides)
    return NodeRequest(**fields)


def ran(workspace: Path, *, routes: tuple[str, ...] = ()) -> NodeOutcome:
    return asyncio.run(run_node(request(workspace, routes=routes), model=model_from()))


# ── A1 · one bash tool, a real artefact, and a submission that came from the CLI ─────────────────

def test_a1_one_bash_tool_writes_a_file_and_submits(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    model = model_from(["printf 'from the sandbox\\n' > out.txt"],
                       ['anchor-done --summary "wrote out.txt"'])

    outcome = asyncio.run(run_node(request(workspace), model=model))

    assert outcome.status == COMPLETED
    assert (workspace / "out.txt").read_text(encoding="utf-8") == "from the sandbox\n", \
        "the artefact is real: bubblewrap wrote it"
    assert outcome.submission == "wrote out.txt", "the summary came from the CLI, not the model"
    assert outcome.model_requests == 2, "two commands, two model requests, no more"
    assert outcome.files == ("out.txt",)


def test_a1_the_model_is_offered_exactly_one_tool(tmp_path):
    """Not "one plus a finish tool": the completion is a bash command, and a second tool would be a
    second way out that the protocol does not have."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    seen: dict[str, list[str]] = {}

    def watching(messages, info):
        seen["function_tools"] = [tool.name for tool in info.function_tools]
        seen["output_tools"] = [tool.name for tool in (info.output_tools or [])]
        return ModelResponse(parts=[TextPart(content="nothing")])

    asyncio.run(run_node(request(workspace, max_requests=2), model=FunctionModel(watching)))

    assert seen["function_tools"] == [], "a function tool would be a second tool the model can see"
    assert seen["output_tools"] == ["bash"], f"found {seen['output_tools']}"


# ── A2 · plain text is not a submission, and the loop can recover ────────────────────────────────

def test_a2_plain_text_does_not_submit_and_the_node_can_still_finish(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    # Says it is done, in words. Then does it.
    model = model_from("I have finished the task.",
                       ['anchor-done --summary "actually wrote it"'])

    outcome = asyncio.run(run_node(request(workspace), model=model))

    assert outcome.status == COMPLETED, "a text answer must not end the node by itself"
    assert outcome.submission == "actually wrote it"


def test_a2_saying_it_is_done_repeatedly_has_a_finite_end(tmp_path):
    """A node that only ever talks must not loop for ever, and must not be reported as done."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    model = model_from(*["I am done"] * 50)          # never calls the tool

    outcome = asyncio.run(run_node(request(workspace, max_requests=6), model=model))

    assert outcome.status != COMPLETED
    assert outcome.route is None, "a node that never finished must not be schedulable"
    assert outcome.reason


# ── A3 · the submission is the end ───────────────────────────────────────────────────────────────

def test_a3_the_model_is_never_asked_again_after_a_submission(tmp_path):
    """The strongest form: the model raises if it is called at all past the completion, so the run
    succeeding is the assertion that it was not."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    asked: list[int] = []

    def no_further(served: int) -> ModelResponse:
        asked.append(served)
        raise AssertionError(f"the model was asked again after the submission (request {served + 1})")

    model = model_from(["printf 'x\\n' > out.txt"],
                       ['anchor-done --summary "done"'], on_extra=no_further)

    outcome = asyncio.run(run_node(request(workspace), model=model))

    assert outcome.status == COMPLETED, outcome.reason
    assert asked == [], "the model was asked past the completion"
    assert outcome.model_requests == 2


# ── A4 · several commands in one response, and a completion among them ───────────────────────────

def test_a4_commands_after_a_completion_do_not_run(tmp_path):
    """Observed as an effect on disk, not read off a declaration. The declaration is in the adapter;
    this is the behaviour, and only the behaviour counts."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    model = model_from(["printf 'before\\n' > before.txt",
                        'anchor-done --summary "finished in the middle"',
                        "printf 'after\\n' > after.txt"])

    outcome = asyncio.run(run_node(request(workspace), model=model))

    assert outcome.status == COMPLETED
    assert (workspace / "before.txt").is_file(), "the command before the completion must run"
    assert not (workspace / "after.txt").exists(), \
        "a command after the completion ran — the response did not stop at the submission"
    assert outcome.model_requests == 1, "one response, one request"


def test_a4_the_order_commands_ran_in_is_the_order_they_were_emitted(tmp_path):
    """A barrier, observed: each command appends its name, so the file is the order."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    model = model_from(["printf 'a\\n' >> order.txt", "printf 'b\\n' >> order.txt",
                        "printf 'c\\n' >> order.txt", 'anchor-done --summary "ordered"'])

    outcome = asyncio.run(run_node(request(workspace), model=model))

    assert outcome.status == COMPLETED
    assert (workspace / "order.txt").read_text(encoding="utf-8") == "a\nb\nc\n"


# ── A5 · a node with more than one way out ───────────────────────────────────────────────────────

def test_a5_with_several_ways_out_done_is_refused_and_route_finishes(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    # The real CLI: the target is validated against ANCHOR_ROUTES inside the sandbox, so a marker
    # printed by hand would not be testing the protocol the node actually has.
    model = model_from(['anchor-done --summary "trying the ordinary way"'],
                       ['anchor-route --to right --reason "because that is where it goes"'])

    outcome = asyncio.run(run_node(request(workspace, routes=("left", "right")), model=model))

    assert outcome.status == COMPLETED
    assert outcome.route == "right"
    assert outcome.submission == "because that is where it goes", "the reason is the summary"


def test_a5_with_one_way_out_done_finishes_and_names_no_route(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    model = model_from(['anchor-done --summary "the only way out follows"'])

    outcome = asyncio.run(run_node(request(workspace, routes=("only",)), model=model))

    assert outcome.status == COMPLETED and outcome.route is None


# ── A6 · a target that is not a way out ──────────────────────────────────────────────────────────

def test_a6_an_unknown_target_is_refused_and_can_be_corrected(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    model = model_from(['anchor-route --to nowhere --reason "not a way out"'],
                       ['anchor-route --to left --reason "after being told"'])

    outcome = asyncio.run(run_node(request(workspace, routes=("left", "right")), model=model))

    assert outcome.status == COMPLETED
    assert outcome.route == "left", "the first target must not have been accepted"


# ── A7 · markers that are not completions ────────────────────────────────────────────────────────

def test_a7_a_marker_that_is_not_the_first_line_is_not_a_completion(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    model = model_from([f'printf "talking about {DONE_SENTINEL}\\n"'],
                       ['anchor-done --summary "the real one"'])

    outcome = asyncio.run(run_node(request(workspace), model=model))

    assert outcome.status == COMPLETED and outcome.submission == "the real one"


def test_a7_a_marker_on_a_command_that_failed_is_refused(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    model = model_from(['echo "' + DONE_SENTINEL + '"; exit 3'],
                       ['anchor-done --summary "the one that worked"'])

    outcome = asyncio.run(run_node(request(workspace), model=model))

    assert outcome.status == COMPLETED and outcome.submission == "the one that worked"


def test_a7_a_route_on_a_command_that_failed_is_refused(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    # The marker is printed by the real command, and then the command fails. A route printed by a
    # command that did not succeed must not be accepted — the check the mini path does not make.
    model = model_from(['anchor-route --to left --reason "printed, then failed"; exit 1'],
                       ['anchor-route --to left --reason "from a command that succeeded"'])

    outcome = asyncio.run(run_node(request(workspace, routes=("left", "right")), model=model))

    assert outcome.status == COMPLETED
    assert outcome.submission == "from a command that succeeded", \
        "a route printed by a command that then failed must not have been accepted"


@pytest.mark.parametrize("output,code,timed_out,expected", [
    (DONE_SENTINEL, 0, False, "done"),
    (DONE_SENTINEL + "\nsummary", 0, True, "refused"),
    (ROUTE_SENTINEL + " left", 127, False, "refused"),
    ("x\n" + DONE_SENTINEL, 0, False, ""),
    ("COMPLETE_TASK_AND_SUBMIT", 0, False, ""),
    (f"prefix {DONE_SENTINEL}", 0, False, ""),
    ("", 0, False, ""),
])
def test_a7_read_completion_is_exact(output, code, timed_out, expected):
    """The protocol, at its edges, without a sandbox in the way."""
    # `done` is the ordinary finish, and an ordinary finish needs at most one way out.
    routes = ("left",) if (output == DONE_SENTINEL and code == 0 and not timed_out) else ("left", "right")
    kind, _, _ = read_completion(Executed(output=output, returncode=code, timed_out=timed_out), routes)
    assert kind == expected


def test_a7_done_is_refused_where_the_node_has_to_choose():
    kind, message, _ = read_completion(Executed(output=DONE_SENTINEL, returncode=0),
                                       ("left", "right"))
    assert kind == "refused" and "left" in message and "right" in message


# ── A8 · an ordinary failure, and no framework-level retry ───────────────────────────────────────

def test_a8_a_failing_command_is_visible_and_is_not_run_again(tmp_path):
    """A counter file proves it: run twice, it would say two."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    model = model_from(["echo run >> count.txt; echo 'it broke' >&2; exit 7"],
                       ['anchor-done --summary "after the failure"'])

    outcome = asyncio.run(run_node(
        request(workspace, trace=tmp_path / "trace.jsonl"), model=model))

    assert outcome.status == COMPLETED
    assert (workspace / "count.txt").read_text(encoding="utf-8") == "run\n", \
        "the command ran twice — the framework retried a side effect"
    trace = Path(outcome.trace_ref).read_text(encoding="utf-8")
    assert "7" in trace and "it broke" in trace, "the return code and stderr must reach the record"


def test_a8_a_timed_out_command_is_refused_as_a_completion(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    model = model_from(["sleep 30; echo '" + DONE_SENTINEL + "'", 'anchor-done --summary "later"'])

    outcome = asyncio.run(run_node(request(workspace, timeout_seconds=1.0), model=model))

    assert outcome.status == COMPLETED and outcome.submission == "later", \
        "a completion from a command that timed out must not have been accepted"


# ── A9 · the sandbox is really the boundary ──────────────────────────────────────────────────────

def test_a9_its_own_workspace_is_writable_and_what_it_was_given_is_not(tmp_path):
    workspace = tmp_path / "ws"
    given = tmp_path / "given"
    workspace.mkdir()
    given.mkdir()
    (given / "theirs.md").write_text("do not change me\n", encoding="utf-8")
    before = (given / "theirs.md").read_bytes()

    model = model_from([
        "printf 'mine\\n' > mine.txt && echo wrote",
        "echo tampered >> /in/upstream/theirs.md 2>&1 || echo refused",
        'anchor-done --summary "checked the boundary"',
    ])
    outcome = asyncio.run(run_node(
        request(workspace, inputs=((str(given), "/in/upstream"),)), model=model))

    assert outcome.status == COMPLETED
    assert (workspace / "mine.txt").is_file()
    assert (given / "theirs.md").read_bytes() == before, "a read-only mount was written through"


def test_a9_the_history_is_readable_and_not_rewritable(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    subprocess.run(["git", "init", "-q", str(workspace)], check=True)
    subprocess.run(["git", "-C", str(workspace), "-c", "user.name=A", "-c", "user.email=a@b",
                    "commit", "-q", "--allow-empty", "-m", "start"], check=True)
    head = subprocess.run(["git", "-C", str(workspace), "rev-parse", "HEAD"],
                          capture_output=True, text=True).stdout.strip()

    model = model_from(["git -C /workspace log --oneline | head -1",
                        "echo tampered >> /workspace/.git/config 2>&1 || echo refused",
                        'anchor-done --summary "read the history"'])
    outcome = asyncio.run(run_node(
        request(workspace, trace=tmp_path / "trace.jsonl"), model=model))

    assert outcome.status == COMPLETED, outcome.reason
    trace = Path(outcome.trace_ref).read_text(encoding="utf-8")
    assert head[:7] in trace, "the node could not read its own history"
    assert "refused" in trace, "the node could write to its own history"


def test_a9_network_is_off_and_that_is_the_isolation_the_sandbox_provides(tmp_path):
    """Checked against the sandbox's own probe rather than against reaching the internet: whether a
    container has a route out is not something this package should be asserting."""
    wiring = NodeSandbox(tree=tmp_path, node_id="probe", network=False, timeout_seconds=30.0)
    offline = wiring.sandbox.run(wiring.spec("getent hosts example.com || echo offline"))

    online = NodeSandbox(tree=tmp_path, node_id="probe", network=True, timeout_seconds=30.0)
    with_net = online.sandbox.run(online.spec("getent hosts example.com || echo offline"))

    assert offline.returncode == 0
    if "offline" in with_net.stdout:
        pytest.skip("this machine has no route out, so the two cannot be told apart")
    assert "offline" in offline.stdout, "network=False did not isolate the sandbox"


# ── A10 · out of budget ──────────────────────────────────────────────────────────────────────────

def test_a10_running_out_of_requests_is_its_own_status_and_never_a_route(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    # Six commands, a budget of three: it cannot get there.
    model = model_from(*[[f"echo step {n} >> log.txt"] for n in range(6)])

    outcome = asyncio.run(run_node(request(workspace, max_requests=3), model=model))

    assert outcome.status == BUDGET_EXHAUSTED, outcome.reason
    assert outcome.route is None, "a pass that did not finish must not be schedulable"
    assert outcome.submission == ""
    assert (workspace / "log.txt").read_text(encoding="utf-8").count("step") == 3, \
        "it did the work it was allowed and no more"


def test_a10_the_budget_does_not_buy_one_more_request(tmp_path):
    """The count is exact: four requests with a budget of four, never five."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    asked: list[int] = []

    def counted(served: int) -> ModelResponse:
        asked.append(served)
        return ModelResponse(parts=[TextPart(content="still thinking")])

    model = model_from(*[[f"echo {n}"] for n in range(20)], on_extra=counted)

    outcome = asyncio.run(run_node(request(workspace, max_requests=4), model=model))

    assert outcome.status == BUDGET_EXHAUSTED
    assert len(asked) == 0, f"the model was asked past the budget: {len(asked)} extra"


# ── A11 · two executions do not leak into each other ─────────────────────────────────────────────

def test_a11_a_second_execution_does_not_see_the_first_one_s_completion(tmp_path):
    """The workspace is kept on purpose; the conversation, the route and the completion are not."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    first = model_from(["printf 'kept\\n' > kept.txt",
                        'anchor-route --to right --reason "first execution"'])
    got = asyncio.run(run_node(request(workspace, routes=("left", "right")), model=first))
    assert (got.status, got.route) == (COMPLETED, "right")

    # The second is given a different set of ways out and must not inherit the first's route.
    second = model_from(["printf 'more\\n' > more.txt"],
                       ['anchor-done --summary "second execution"'])
    again = asyncio.run(run_node(
        NodeRequest(execution_id="exec-2", task="again", workspace=workspace, routes=("only",)),
        model=second))

    assert (again.status, again.route) == (COMPLETED, None), \
        "the first execution's route leaked into the second"
    assert again.submission == "second execution"
    assert (workspace / "kept.txt").is_file(), "the workspace is kept between executions by design"
    assert again.model_requests == 2, "the second started its own conversation"


# ── A12 · a real graph, with the adapter in the agent steps and nothing else replaced ────────────

class AdapterAgent:
    """The bridge the Graph's own factory seam takes, so a real run can use the new entry point.

    Deliberately thin: it turns a `_Step`'s already-resolved mounts into a `NodeRequest` and calls
    `run_node`. It does not touch Git, the input views, the sandbox or the op path — those stay the
    runtime's, which is the only way this test says anything about integrating the two.
    """

    def __init__(self, workspace: Path, node_id: str, inputs, trace, model, routes):
        self.node_id = node_id
        self.model = model
        self.routes = routes
        # `_Given` is the scheduler's pinned pointer; the contract takes (host, mount) pairs, so the
        # bridge flattens it — one small translation, and the only thing about the Graph this file
        # knows.
        self.request = NodeRequest(
            execution_id=node_id, task="", workspace=Path(workspace),
            inputs=tuple(bind for item in inputs for bind in item.binds()),
            routes=tuple(routes), trace=Path(trace) if trace else None)
        self.env = type("Env", (), {"route": None})()

    def run(self, task: str) -> dict:
        self.request = replace_request(self.request, task=task)
        outcome = asyncio.run(run_node(self.request, model=self.model))
        self.env.route = outcome.route
        self.outcome = outcome
        return {"submission": outcome.submission,
                "exit_status": "Submitted" if outcome.status == COMPLETED else outcome.status}

    def resume(self, messages: list) -> dict:
        # Not this package's business, and saying so is better than a bridge that pretends.
        raise AssertionError("this package does not implement resuming a node")


def replace_request(request: NodeRequest, **fields) -> NodeRequest:
    from dataclasses import replace
    return replace(request, **fields)


def test_a12_a_real_graph_runs_agent_op_agent_through_the_new_entry_point(tmp_path, monkeypatch):
    """The whole path: a real scheduler, a real Git freeze, real read-only input views, a real op, and
    an agent step that is the adapter. Nothing but the model is a stand-in."""
    from anchor.simple import graph as graph_module
    from anchor.simple import run as runner

    graph = {
        "entry": "first",
        "objective": "one agent, one op, one agent",
        # Each role declares what it writes, because the loader checks that everything a node says it
        # reads is something it can actually be handed — and it caught the first draft of this graph,
        # which is the check doing its job rather than an obstacle.
        "agents": {"writer": {"model": "models.academic", "writes": ["draft.md"]},
                   "reader": {"model": "models.academic", "reads": ["size.txt"],
                              "writes": ["upstream-size.txt"]}},
        "ops": {"note": {"run": "wc -c < /in/first/draft.md > size.txt && echo counted",
                         "reads": ["draft.md"], "writes": ["size.txt"]}},
        "nodes": [{"id": "first", "agent": "writer"}, {"id": "middle", "op": "note"},
                  {"id": "last", "agent": "reader"}],
        "edges": [{"from": "first", "to": "middle"}, {"from": "middle", "to": "last"}],
    }
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "graph.json").write_text(json.dumps(graph), encoding="utf-8")
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}', encoding="utf-8")

    models: dict[str, object] = {
        # Writes a draft, then finishes.
        "first": model_from(["printf 'the draft\\n' > draft.md"],
                            ['anchor-done --summary "wrote the draft"']),
        # Must read what it was given through the pointer, and must finish through the CLI.
        "last": model_from(["cat /in/middle/size.txt > upstream-size.txt && cat draft.txt "
                            ">> upstream-size.txt 2>/dev/null; echo read it"],
                           ['anchor-done --summary "read what I was given"']),
    }
    calls: list[str] = []
    real = runner._agent_for

    def factory(graph, node_id, directory, models_, secret, config, inputs=(), trace=None,
                script=None):
        # **Only the agent path.** An op is dispatched by the real factory, so the run's op really is
        # the runtime's op — a bridge that took those over would be testing itself.
        if graph.nodes[node_id].op:
            return real(graph, node_id, directory, models_, secret, config, inputs=inputs,
                        trace=trace, script=script)
        calls.append(node_id)
        return AdapterAgent(Path(directory), node_id, inputs, trace, models[node_id],
                            graph_module.Graph.routes(graph, node_id))

    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", factory)

    state = runner.run(workspace, config_path=config)
    run_dir = next((workspace / "runs").glob("*"))

    assert state.status == "finished", (state.status, state.error, state.ceased)
    assert state.executed == ["first", "middle", "last"]
    # The op ran for real, and never asked a model — it is not even in the calls the factory saw.
    assert "middle" not in calls, "the op went through the model path"
    assert (run_dir / "middle" / "size.txt").read_text(encoding="utf-8").strip() == "10"
    # The last agent read the op's output through the read-only mount and wrote it into its own file.
    assert (run_dir / "last" / "upstream-size.txt").read_text(encoding="utf-8").strip() == "10"
    # Real Git freeze, by the runtime and not by the bridge.
    for node in ("first", "middle", "last"):
        assert state.nodes[node]["commit"], f"{node} was not committed"
        log = subprocess.run(["git", "-C", str(run_dir / node), "log", "--format=%s"],
                             capture_output=True, text=True).stdout
        assert "start" in log
    # And the op's own record is its real output, not a model's words.
    assert state.nodes["middle"]["submission"] == "counted"


# ── A13 · the default path, without the optional dependency installed ────────────────────────────

def test_a13_the_default_path_runs_with_pydantic_ai_unimportable(tmp_path):
    """Blocked at the import machinery, in a fresh interpreter, so this is the real thing rather than
    a promise about imports.

    The point of a second node runner is that the first one does not depend on it. An import added in
    the wrong place would be invisible until somebody installed Anchor without the extra.
    """
    workspace = tmp_path / "ws"
    workspace.mkdir()
    (workspace / "graph.json").write_text(json.dumps({
        "entry": "only", "objective": "an op, which needs no model at all",
        "ops": {"only": {"run": "printf 'ran\\n' > ran.txt", "writes": ["ran.txt"]}},
        "nodes": [{"id": "only", "op": "only"}], "edges": [],
    }), encoding="utf-8")
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}', encoding="utf-8")

    script = f"""
import sys

class Block:
    def find_module(self, name, path=None):
        return self if name.split('.')[0] in ('pydantic_ai', 'pydantic_ai_harness') else None
    def find_spec(self, name, path=None, target=None):
        if name.split('.')[0] in ('pydantic_ai', 'pydantic_ai_harness'):
            raise ImportError(f"{{name}} is not installed here")
        return None
    def load_module(self, name):
        raise ImportError(name)

sys.meta_path.insert(0, Block())
for gone in [m for m in sys.modules if m.split('.')[0] in ('pydantic_ai', 'pydantic_ai_harness')]:
    del sys.modules[gone]

from anchor.simple import run as runner
from anchor.simple import agent as agent_module
from anchor.runtime import execenv

state = runner.run({str(workspace)!r}, config_path={str(config)!r})
print('STATUS', state.status, state.executed)
"""
    done = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, cwd="/root/Anchor")

    assert done.returncode == 0, done.stderr[-2000:]
    assert "STATUS finished ['only']" in done.stdout, done.stdout
    assert (workspace / "runs").is_dir()


def test_a13_nothing_on_the_default_path_imports_the_adapter():
    """A static check as well as the dynamic one above: the adapter is imported where it is used and
    not by a module the runtime always loads."""
    default = ["src/anchor/simple/run.py", "src/anchor/simple/agent.py",
               "src/anchor/simple/graph.py", "src/anchor/runtime/execenv.py",
               "src/anchor/serve.py"]

    for name in default:
        text = (Path("/root/Anchor") / name).read_text(encoding="utf-8")
        assert "pydantic_ai" not in text, f"{name} names the optional dependency"
        assert "anchor.node" not in text, f"{name} imports the adapter's package"


# ── the reviewer's findings, as regressions ──────────────────────────────────────────────────────

def test_the_request_count_is_the_model_request_count_and_not_the_command_count(tmp_path):
    """Reported wrong on both failure paths, and in opposite directions.

    The count came from `wiring.commands`, which is how many *commands* ran. One request can carry
    three commands — reported as three — and a request that only answers with text carries none —
    reported as zero, on a run that had asked once. Package 3's cumulative budget would have been
    built on a number that is wrong whenever a pass does not end in a submission.
    """
    workspace = tmp_path / "ws"
    workspace.mkdir()

    # Three commands in one response: one request.
    three = asyncio.run(run_node(
        request(workspace, max_requests=1),
        model=model_from(["echo a", "echo b", "echo c"])))
    assert three.status == BUDGET_EXHAUSTED
    assert three.model_requests == 1, f"three commands in one response reported {three.model_requests}"

    # A single request that only answers with text: one request, not none.
    text = asyncio.run(run_node(
        request(workspace, max_requests=1), model=model_from("I am finished")))
    assert text.status != COMPLETED
    assert text.model_requests == 1, f"one text answer reported {text.model_requests}"


def test_the_conversation_is_in_the_record_when_the_pass_does_not_finish(tmp_path):
    """The record used to be one exit line on every failure, because the messages were only taken off
    a result and a failed pass has no result. That is the evidence a failure is diagnosed from, and it
    was not there."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    trace = tmp_path / "trace.jsonl"

    # One command per response, so the budget is what stops it. Many commands in one response spend
    # the *output retry* budget instead and end in the framework's own error — a separate finding,
    # asserted in its own test below.
    outcome = asyncio.run(run_node(
        request(workspace, max_requests=2, trace=trace),
        model=model_from(["echo one"], ["echo two"], ["echo three"])))

    assert outcome.status == BUDGET_EXHAUSTED, outcome.reason
    lines = [json.loads(line) for line in trace.read_text(encoding="utf-8").splitlines()]
    kinds = [line.get("kind") or line.get("role") for line in lines]
    assert "response" in kinds and kinds.count("request") >= 1, \
        f"the conversation is not in the record: {kinds}"
    assert kinds[-1] == "exit"
    # And the exit line still carries what a failure is read for.
    assert lines[-1]["extra"]["commands"], "the commands that ran are not in the record"


def test_a_sandbox_that_cannot_start_is_a_failed_result_and_not_an_exception(tmp_path, monkeypatch):
    """Construction and the working probe used to sit outside the exception handling, so a sandbox
    that could not start raised out of the entry point — where the contract promises a result."""
    from anchor.runtime import execenv

    def refuse(self) -> None:
        raise RuntimeError("this machine will not make a namespace")

    monkeypatch.setattr(execenv.NodeSandbox, "require_working", refuse)
    workspace = tmp_path / "ws"
    workspace.mkdir()

    outcome = asyncio.run(run_node(request(workspace), model=model_from("never asked")))

    assert outcome.status == FAILED, outcome.status
    assert "namespace" in outcome.reason
    assert outcome.route is None
    assert outcome.model_requests == 0, "the model was asked before the sandbox was known to work"


def test_a12b_the_route_the_adapter_returns_drives_a_real_branch(tmp_path, monkeypatch):
    """A straight line does not test routing. This one has a fork, and which arm runs is decided by
    what the adapter handed back — not by anything stubbed."""
    from anchor.simple import graph as graph_module
    from anchor.simple import run as runner

    graph = {
        "entry": "decide",
        "objective": "one agent chooses between two arms",
        "agents": {"w": {"model": "models.academic", "writes": ["note.md"]}},
        "ops": {"left": {"run": "printf 'went left\\n' > arm.txt && echo left",
                         "writes": ["arm.txt"]},
                "right": {"run": "printf 'went right\\n' > arm.txt && echo right",
                          "writes": ["arm.txt"]}},
        "nodes": [{"id": "decide", "agent": "w"}, {"id": "left", "op": "left"},
                  {"id": "right", "op": "right"}],
        "edges": [{"from": "decide", "to": "left"}, {"from": "decide", "to": "right"}],
    }
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "graph.json").write_text(json.dumps(graph), encoding="utf-8")
    config = tmp_path / "runtime.json"
    config.write_text('{"models": []}', encoding="utf-8")

    # The adapter is told both ways out and chooses `right`, through the real anchor-route.
    model = model_from(["printf 'note\\n' > note.md"],
                       ['anchor-route --to right --reason "the right arm is the one"'])
    real = runner._agent_for

    def factory(g, node_id, directory, models_, secret, cfg, inputs=(), trace=None, script=None):
        if g.nodes[node_id].op:
            return real(g, node_id, directory, models_, secret, cfg, inputs=inputs, trace=trace,
                        script=script)
        return AdapterAgent(Path(directory), node_id, inputs, trace, model,
                            graph_module.Graph.routes(g, node_id))

    monkeypatch.setattr(runner, "_config", lambda path: ({}, None))
    monkeypatch.setattr(runner, "_agent_for", factory)

    state = runner.run(workspace, config_path=config)
    run_dir = next((workspace / "runs").glob("*"))

    assert state.status == "finished", (state.status, state.error, state.ceased)
    assert state.executed == ["decide", "right"], f"the branch taken was {state.executed}"
    assert state.skipped == ["left"]
    assert (run_dir / "right" / "arm.txt").read_text(encoding="utf-8") == "went right\n"
    assert state.nodes["decide"]["route"] == "right", "the route the adapter returned is the record's"


# ── the compatibility question the reviewer said must be settled before freezing ──────────────────

def test_the_harness_compaction_cannot_see_an_ordinary_observation(tmp_path):
    """**The finding this package would otherwise have left for package 3 to discover.**

    `pydantic-ai-harness`'s `ClearToolResults` — the capability that exists to stop a long node's
    context growing without bound — finds what to clear through `iter_tool_pairs`, and that matches
    `ToolReturnPart` alone. Every ordinary command in this adapter is recorded as a `RetryPromptPart`,
    because an output tool that does not return must raise. So the compaction would clear the one
    submission and none of the output it was built to reclaim: **the tool record mechanism does not
    fit the context mechanism**, and that is a design problem rather than a wording difference.

    Asserted against the harness's own pairing function rather than a copy of its rule, so this fails
    if either side changes.
    """
    shared = pytest.importorskip("pydantic_ai_harness.compaction._shared",
                                 reason="the harness is not installed here")
    from pydantic_ai.messages import (ModelRequest, ModelResponse, RetryPromptPart,
                                      ToolCallPart, ToolReturnPart)

    call = ToolCallPart(tool_name="bash", args={"command": "ls"},
                        tool_call_id="call-1")

    # A call lives in a ModelResponse and its result in the ModelRequest that follows — which is
    # where the harness looks for both.
    ordinary = [ModelResponse(parts=[call]),
                ModelRequest(parts=[RetryPromptPart(content="<output>a file</output>",
                                                    tool_name="bash", tool_call_id="call-1")])]
    submission = [ModelResponse(parts=[call]),
                  ModelRequest(parts=[ToolReturnPart(tool_name="bash", content="done",
                                                     tool_call_id="call-1")])]

    assert shared.iter_tool_pairs(submission), "a normal tool return is a pair the harness can clear"
    assert not shared.iter_tool_pairs(ordinary), (
        "an ordinary observation is a pair after all — then the adapter's record does fit the harness, "
        "and this test's premise is wrong")


def test_what_an_ordinary_command_is_recorded_as(tmp_path):
    """The premise of the test above, taken from a real run rather than from reading the adapter."""
    workspace = tmp_path / "ws"
    workspace.mkdir()

    outcome = asyncio.run(run_node(
        request(workspace, max_requests=1, trace=tmp_path / "t.jsonl"),
        model=model_from(["echo hello"])))

    lines = [json.loads(line) for line in Path(outcome.trace_ref).read_text().splitlines()]
    kinds = [part.get("part_kind")
             for line in lines for part in (line.get("parts") or [])]
    assert "tool-call" in kinds, "the command is not in the record"
    assert "retry-prompt" in kinds, f"an ordinary command was not recorded as a retry: {kinds}"
    assert "tool-return" not in kinds, (
        "an ordinary command produced a ToolReturnPart — then the adapter changed and the harness "
        "could clear it after all")
