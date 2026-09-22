"""Running one node with PydanticAI, and answering whether it can keep Anchor's semantics.

What this file is for is a question, not a feature: **can a general agent framework preserve the five
things this runtime is built on** — one bash tool, serial execution, explicit submission that stops the
pass, routing the Graph still owns, and a sandbox that is really the boundary? It answers by being run
against the real sandbox, the real completion CLIs and the real mounts, driven by a deterministic model
so the answer costs nothing and does not depend on a provider.

**`bash` is an ordinary function tool, and the stopping is done in two places rather than by the
framework.** That took a wrong turn to arrive at and the turn is worth recording, because the wrong
version passed its own tests:

    An *output* tool is the framework's own way to end a run from a tool — its successful return is the
    result, and `end_strategy='early'` stubs the rest of the response. It satisfies "a submission stops
    the pass" exactly, and it records every ordinary command as a `RetryPromptPart`, because an output
    tool that does not return has to raise. `pydantic-ai-harness`'s `ClearToolResults` — the capability
    a long node depends on to stop its context growing — finds what to clear through `iter_tool_pairs`,
    which matches `ToolReturnPart` alone. So that version would have made the context machinery blind to
    every command's output, and package 3 would have found it.

    A function tool records a normal `ToolReturnPart`, and then nothing ends the run — which is the part
    that looked fatal. It is not: **the framework dispatching a call is not the same as the call having
    to do anything.** So

      1. an ordinary command returns its observation as a normal tool result, which is what the record
         and the context machinery expect;
      2. a recognised submission is stored on the node's wiring, and the command that carried it does
         not need the run to end there;
      3. a later call in the same response **is dispatched and does not run** — the tool checks whether
         the node has already finished and returns without touching the sandbox, which is what the
         acceptance matrix asks for: the *effect* must not happen, not the dispatch;
      4. and after the tool batch, the public iteration boundary is where the pass ends: the adapter
         leaves `agent.iter` there and never asks the model again.

    Four small pieces, no private API, and all three requirements hold at once — one tool, the effect
    stopped at the submission, and a record the harness can read. The measurements are in
    `AGENT_NODE_PLAN_01_RESULT.md` §5.9.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any

from pydantic_ai import Agent, ModelRequestNode, RunContext, UsageLimits
from pydantic_ai.messages import ModelMessagesTypeAdapter
from pydantic_ai.models.wrapper import WrapperModel
from pydantic_ai.exceptions import ModelRetry, UsageLimitExceeded

from anchor.node import BUDGET_EXHAUSTED, COMPLETED, FAILED, NodeOutcome, NodeRequest
from anchor.runtime.execenv import Executed, NodeSandbox

#: The two commands that finish a node, and the exact strings they print. Both are the runtime's
#: existing protocol, not this adapter's: `done/__main__.py` and `route/__main__.py` print them, and
#: the mini path recognises the same two.
#: How much of one command's output the record keeps. Generous, because an ordinary command's output
#: is the evidence a failure is read from — and bounded, because a node can read a large file.
TAIL_OUTPUT = 20000

DONE_SENTINEL = "COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT"
ROUTE_SENTINEL = "ANCHOR_ROUTE:"

#: What every node is told, whoever wrote its role. Supplied by the runner and not the caller: it is
#: the contract of running inside one directory with a shell, and a contract each node may reword is
#: not one.
RULES = """\
You act by calling the bash tool you have been given, in a directory of your own.

Each response must contain at least one bash call. It is run, you are shown the output, and you call
the next one. A response with no call is rejected and you will be asked again, so never answer with a
description of what you intend to do — do it.

What you were given is mounted read-only and cannot be written to. Your own directory is kept between
passes and is the only thing the nodes after you will see, so the deliverable has to be a file in it
rather than only something you say.

You finish by running one of the completion commands and nothing after it. Saying that you are done is
not finishing, and neither is any other command: the loop reads the exact output of those commands and
nothing else.
"""


class _CountingModel(WrapperModel):
    """A model that counts how many times it was asked.

    The adapter cannot read this off the framework afterwards: `usage` lives on a result, and on the
    two paths where the count matters most — a failure and a spent budget — there is no result. The
    number is also not the command count. One request can carry three commands, and a request that
    only answers with text carries none, so reporting commands gets both cases wrong in opposite
    directions: a plain-text turn reported zero requests, and a three-command turn reported three.

    Counted where it happens, so every path reports the same thing.
    """

    requests: int = 0

    async def request(self, messages: Any, model_settings: Any, model_request_parameters: Any) -> Any:
        self.requests += 1
        return await super().request(messages, model_settings, model_request_parameters)


@dataclass
class _Done:
    """A submission the node made, kept so the pass can end at a boundary the framework offers."""

    submission: str
    route: str | None
    command: str


@dataclass
class _Wiring:
    """What the tool needs to run a command: the sandbox, and the ways out."""

    sandbox: NodeSandbox
    routes: tuple[str, ...]
    #: Set by the command that submitted. Its presence is what stops everything after it — the later
    #: calls in the same response, and the pass itself.
    done: _Done | None = None
    commands: int = 0
    #: (command, exit code, first line, full output) in the order they were dispatched, for the
    #: record and for the tests that assert ordering rather than believing a claim about it. A skipped
    #: call is in here too, with no exit code — it was dispatched and it did not run, and both halves
    #: of that are worth keeping.
    ran: list[tuple] = None                      # type: ignore[assignment]

    def __post_init__(self) -> None:
        if self.ran is None:
            self.ran = []


def _first_line(output: str) -> str:
    return next((line.strip() for line in output.lstrip().splitlines() if line.strip()), "")


def _after_first(output: str) -> str:
    lines = output.lstrip().splitlines()
    for at, line in enumerate(lines):
        if line.strip():
            return "\n".join(lines[at + 1:]).strip()
    return ""


def read_completion(ran: Executed, routes: tuple[str, ...]) -> tuple[str, str, str | None]:
    """Read the completion protocol out of one command's result.

    Returns `(kind, message, route)` where kind is `'done'`, `'routed'`, `'refused'` or `''`.

    **A command that did not succeed cannot finish a node.** A non-zero exit or a timeout carrying the
    marker is refused, and that check is the whole reason this is written out rather than shared with
    the mini path: `SandboxEnvironment._check_finished` tests neither for a route, so a `anchor-route`
    that exited non-zero still routes there. Changing that is not this package's business — the
    difference is recorded instead, and this side does it the way the protocol says.

    The marker has to be the **first non-empty line**, exactly. A mention of it halfway through a
    command's output is text the node printed, not a completion.
    """
    first = _first_line(ran.output)
    if ran.timed_out:
        return ("refused", "the command timed out, so its output cannot finish this node", None) \
            if first.startswith((DONE_SENTINEL, ROUTE_SENTINEL)) else ("", "", None)
    if ran.returncode != 0:
        return ("refused", f"the command exited {ran.returncode}, so its output cannot finish this "
                           f"node", None) \
            if first.startswith((DONE_SENTINEL, ROUTE_SENTINEL)) else ("", "", None)

    if first == DONE_SENTINEL:
        if len(routes) > 1:
            # Told, not merely refused: the next turn should be a correction, not a guess.
            return ("refused", f"this node has more than one way out and does not finish this way. "
                               f"Name one: anchor-route --to <{'|'.join(routes)}> --reason \"…\"",
                    None)
        return ("done", _after_first(ran.output), None)
    if first.startswith(ROUTE_SENTINEL):
        target = first.split(":", 1)[1].strip()
        if target not in routes:
            return ("refused", f"{target!r} is not a way out of this node. Choose one of: "
                               f"{', '.join(routes)}", None)
        return ("routed", _after_first(ran.output), target)
    return ("", "", None)


def _observation(ran: Executed, extra: str = "") -> str:
    """A command's result as the model should see it."""
    parts = [f"<returncode>{ran.returncode}</returncode>",
             "<output>", ran.output.rstrip("\n"), "</output>"]
    if ran.timed_out:
        parts.insert(1, "<timeout>the command was stopped for running too long</timeout>")
    if extra:
        parts.append(extra)
    return "\n".join(parts)


def one_line(text: str, width: int = 70) -> str:
    flat = " ".join(text.split())
    return flat if len(flat) <= width else flat[:width] + "…"


def _bash(ctx: RunContext[_Wiring], command: str) -> str:
    """The one tool the model has. Ordinary commands return; a submission is remembered.

    **The guard is the point.** A later call in the same response is dispatched by the framework and
    must not reach the sandbox: once the node has submitted, everything after it in that response is
    something the node did not ask for and must not do. The framework's own way of stopping a batch
    exists only for output tools, and an output tool's records are the wrong shape for the context
    machinery — so the check is here, where it costs one comparison and no framework cooperation, and
    what the skipped call reports is a result rather than a failure.
    """
    wiring = ctx.deps
    if wiring.done is not None:
        # Recorded as well as refused. The framework's history stops at the node that would carry
        # this batch's results, so what a skipped call was is only in the record if it is put there.
        wiring.ran.append((command, None, "skipped: the node had already submitted", ""))
        return (f"<skipped>not run: this node finished in this same response, at "
                f"`{one_line(wiring.done.command)}`. Nothing after a submission executes.</skipped>")

    ran = wiring.sandbox.run(command)
    wiring.commands += 1
    wiring.ran.append((command, ran.returncode, _first_line(ran.output), ran.output))

    kind, message, route = read_completion(ran, wiring.routes)
    if kind == "done":
        wiring.done = _Done(submission=message, route=None, command=command)
    elif kind == "routed":
        wiring.done = _Done(submission=message, route=route, command=command)
    # A refusal and an ordinary result travel the same way — back to the model as a plain tool result,
    # which is also what the harness can read and clear. `ModelRetry` is used for neither: it would
    # attach correction instructions to a command's ordinary output and record it as a retry.
    return _observation(ran, message if kind == "refused" else "")


def build_agent(model: Any, *, instructions: str = "",
                max_retries: int = 60) -> Agent[_Wiring, str]:
    """One Agent per execution, with one tool and no room for a second.

    `output_type=str` with a validator that always refuses is not a completion mechanism — the tool
    holds that — it is the answer to *a model that replies with words instead of acting*. The rules say
    a response without a tool call is rejected, and the framework is what makes that true: the validator
    turns a text answer into a retry, so the node is asked again rather than ending with a sentence and
    no work. Bounded by `retries`, set from the request budget so a node that only talks stops at the
    same ceiling as one that only works.
    """
    agent: Agent[_Wiring, str] = Agent(
        model,
        # **Not `end_strategy='early'`.** Under it a function tool runs only when every *output* tool
        # has failed — and there is no output tool here, so nothing would ever run. `'early'` is the
        # right setting for the design this one replaced, where the tool *was* the output tool; with an
        # ordinary tool the default is the one that runs it.
        deps_type=_Wiring,
        output_type=str,
        retries=max_retries,
        instructions=f"{instructions.strip()}\n\n{RULES}".strip(),
    )
    # One tool, and a barrier: the protocol says the commands in one response run in order, and this is
    # what makes that the framework's behaviour rather than the sandbox's good luck. It also makes the
    # skip guard decisive — without it a later call could already be in the sandbox when the submission
    # is recognised.
    # Registered through the decorator rather than by passing the function, because that is the form
    # the overloads match. `tool` and not `tool_plain`: the tool needs the node's wiring, which arrives
    # on the run context.
    agent.tool(name="bash", description="Execute a bash command", sequential=True)(_bash)

    @agent.output_validator
    def _not_words(value: str) -> str:
        raise ModelRetry(
            "You answered with words. This node is finished only by running one of the completion "
            "commands, and it acts by calling the bash tool — call it. Saying that you are done is "
            "not finishing.")

    return agent


def _write_trace(path: Path | None, messages: list[Any], wiring: _Wiring | None,
                 outcome: NodeOutcome) -> str:
    """The record: what the model was told, what it called, what came back, and why it stopped.

    Written whole rather than projected. A readable summary would be nicer and would throw away the
    tool-call pairing a later package needs for compaction; reading it is a separate problem with a
    separate place to solve it.
    """
    if path is None:
        return ""
    path.parent.mkdir(parents=True, exist_ok=True)
    # Through the library's own adapter rather than a hand-rolled encoder: the messages are a tagged
    # union whose exact shape is the framework's business, and a shape guessed here is a shape that
    # breaks on an upgrade without saying so.
    encoded = ModelMessagesTypeAdapter.dump_python(messages, mode="json") if messages else []
    with path.open("w", encoding="utf-8") as handle:
        for message in encoded:
            handle.write(json.dumps(message, ensure_ascii=False, default=str) + "\n")
        # The commands are written out whole, and not left to the framework's history. A pass that
        # submits leaves the loop at the node *after* its last batch — the node whose running is what
        # would put that batch's results into `all_messages` — so the last batch, which is the one
        # carrying the submission, is not in the messages at all. The adapter ran those commands and
        # knows exactly what they did; the record would be missing the most important part of the pass
        # without this.
        handle.write(json.dumps({
            "role": "exit", "content": outcome.reason or outcome.submission,
            "extra": {"status": outcome.status, "route": outcome.route,
                      "submission": outcome.submission, "model_requests": outcome.model_requests,
                      "commands": [] if wiring is None else
                                  [{"command": c, "returncode": r, "first_line": f,
                                    "output": (o or "")[:TAIL_OUTPUT]}
                                   for c, r, f, o in wiring.ran]},
        }, ensure_ascii=False) + "\n")
    return str(path)


def _files(workspace: Path) -> tuple[str, ...]:
    if not workspace.is_dir():
        return ()
    return tuple(sorted(
        str(item.relative_to(workspace)) for item in workspace.rglob("*")
        if item.is_file() and ".git" not in item.relative_to(workspace).parts))


async def run_node(request: NodeRequest, *, model: Any) -> NodeOutcome:
    """Run one node to a result. The entry point the Graph would call.

    The model is a parameter rather than something built here: which provider, and whether there is a
    provider at all, is the caller's business, and a runner that built its own would be a runner that
    cannot be tested without one.

    Driven through `agent.iter` rather than `agent.run`, because the pass has to end at a place the
    framework offers rather than where a framework feature decides. Two things need that boundary: a
    pass that submitted must not ask the model again, and a pass that failed must still have its
    conversation — `AgentRun.all_messages()` is readable from the handler for an exception, and a
    result, which is where `run` keeps them, does not exist on that path.
    """
    counted = _CountingModel(model)
    agent = build_agent(counted, instructions=request.instructions,
                        max_retries=request.max_requests)
    limits = UsageLimits(request_limit=request.max_requests)
    wiring: _Wiring | None = None
    run: Any = None
    messages: list[Any] = []
    try:
        wiring = _Wiring(sandbox=NodeSandbox(
            tree=request.workspace, node_id=request.roles or request.execution_id,
            network=request.network, timeout_seconds=request.timeout_seconds,
            routes=request.routes, inputs=request.inputs), routes=request.routes)
        # Inside the try. A sandbox that cannot start is a failed execution and the contract has a
        # status for it; raising out of the entry point would leave the caller with an exception where
        # it was promised a result.
        wiring.sandbox.require_working()

        async with agent.iter(request.task, deps=wiring, usage_limits=limits) as run:
            async for node in run:
                # **The boundary after the tools, not the tools themselves.** A node is yielded when it
                # is entered, so at `CallToolsNode` the commands have not run yet and nothing has been
                # submitted; breaking there does nothing and the pass goes on to ask the model again —
                # measured, by a third request appearing in the record. The node that asks the model is
                # exactly the one to leave on: a submission is already in hand, so asking again is the
                # thing this is here to prevent.
                if isinstance(node, ModelRequestNode) and wiring.done is not None:
                    break

        messages = list(run.all_messages())
        done = wiring.done
        if done is None:
            # The loop ended without a submission. The validator turns a spoken answer into a retry,
            # so this is a pass that stopped for a reason that is not a completion — and calling it
            # one is the failure this whole design exists to refuse.
            raise RuntimeError("the run ended without a submission")
        outcome = NodeOutcome(
            status=COMPLETED, submission=done.submission, route=done.route,
            model_requests=counted.requests, files=_files(request.workspace))
    except UsageLimitExceeded as exc:
        # Out of turns, nothing submitted. Not a failure of the work, and not a route: the graph must
        # not move on the strength of a pass that did not happen.
        messages = list(run.all_messages()) if run is not None else []
        outcome = NodeOutcome(
            status=BUDGET_EXHAUSTED, model_requests=counted.requests,
            reason=f"out of requests after {counted.requests} model requests: {exc}",
            files=_files(request.workspace))
    except Exception as exc:                      # noqa: BLE001 - every failure is a recorded status
        messages = list(run.all_messages()) if run is not None else []
        outcome = NodeOutcome(
            status=FAILED, model_requests=counted.requests,
            reason=f"{type(exc).__name__}: {exc}", files=_files(request.workspace))

    return replace(outcome, trace_ref=_write_trace(request.trace, messages, wiring, outcome))
