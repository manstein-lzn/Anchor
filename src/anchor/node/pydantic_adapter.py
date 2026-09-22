"""Running one node with PydanticAI, and answering whether it can keep Anchor's semantics.

What this file is for is a question, not a feature: **can a general agent framework preserve the five
things this runtime is built on** — one bash tool, serial execution, explicit submission that stops the
pass, routing the Graph still owns, and a sandbox that is really the boundary? It answers by being run
against the real sandbox, the real completion CLIs and the real mounts, driven by a deterministic model
so the answer costs nothing and does not depend on a provider.

**One tool, and it is the output tool.** PydanticAI distinguishes an *output* tool — whose successful
return becomes the run's final result — from a *function* tool, which merely reports back. Anchor's
protocol needs both at once: the completion is a bash command (`anchor-done`), so the tool the model
calls is `bash`, and the moment it recognises a completion the run must end. Registering `bash` as the
output tool is what makes that one act instead of two, and `end_strategy='early'` is what makes the
remaining calls in the same response never run.

**What that costs, and it is worth stating plainly.** An output tool ends the run the moment it
*returns*, so every command that is not a completion must not return. The only exit the framework leaves
from an output tool is `ModelRetry`, so that is what an ordinary command raises — which means **an
ordinary `ls` reaches the model as a retry prompt rather than as a plain tool result**, with whatever
correction framing the framework attaches to one. `ToolFailed` reads the way this should read — a failed
result with no correction instructions — and it does **not** work here: raised from an output tool it
escapes the run and fails the whole execution. That was measured, not assumed, and it is recorded in
`AGENT_NODE_PLAN_01_RESULT.md` as the one place the framework's shape shows through.

The retry budget is therefore the request budget: every non-completion spends one, so it is set from
`max_requests` rather than left at the framework's default of one.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from pydantic import BaseModel
from pydantic_ai import Agent, RunContext, ToolOutput, UsageLimits
from pydantic_ai.messages import ModelMessagesTypeAdapter
from pydantic_ai.exceptions import ModelRetry, UsageLimitExceeded

from anchor.node import BUDGET_EXHAUSTED, COMPLETED, FAILED, NodeOutcome, NodeRequest
from anchor.runtime.execenv import Executed, NodeSandbox

#: The two commands that finish a node, and the exact strings they print. Both are the runtime's
#: existing protocol, not this adapter's: `done/__main__.py` and `route/__main__.py` print them, and
#: the mini path recognises the same two.
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


class Submission(BaseModel):
    """What the node produced, as the output the run ends with.

    The tool's declared return type, so PydanticAI knows what a successful `bash` means. Only a
    completion ever constructs one; everything else raises.
    """

    submission: str = ""
    route: str | None = None


@dataclass
class _Wiring:
    """What the tool needs to run a command: the sandbox, and the ways out."""

    sandbox: NodeSandbox
    routes: tuple[str, ...]
    commands: int = 0
    #: (command, exit code, first line of output) in the order they ran, for the record and for the
    #: tests that assert ordering rather than believing a claim about it.
    ran: list[tuple[str, int, str]] = None      # type: ignore[assignment]

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


def _bash(ctx: RunContext[_Wiring], command: str) -> Submission:
    """The one tool the model has. Returning ends the run; raising asks it again.

    There is no third behaviour available from an output tool, which is why an ordinary command and a
    refused completion travel the same way and differ only in what they say.
    """
    wiring = ctx.deps
    ran = wiring.sandbox.run(command)
    wiring.commands += 1
    wiring.ran.append((command, ran.returncode, _first_line(ran.output)))

    kind, message, route = read_completion(ran, wiring.routes)
    if kind == "done":
        return Submission(submission=message)
    if kind == "routed":
        assert route is not None
        return Submission(submission=message, route=route)
    # Either an ordinary command's result, or a completion this node may not accept — a marker on a
    # command that failed, a `done` where the node has to choose, a target that is not a way out. All
    # three are things for the model to see and act on, so all three go back to it.
    raise ModelRetry(_observation(ran, message))


def build_agent(model: Any, *, instructions: str = "",
                max_retries: int = 60) -> Agent[_Wiring, Submission]:
    """One Agent per execution, with one tool and no room for a second.

    `end_strategy='early'` is the load-bearing argument. Under it, output calls run in the order the
    model emitted them and stop at the first success, and **the calls after that success never run** —
    which is what makes "the completion command is the last thing that happens in a response" true
    rather than hoped for. The default in v2 is `'graceful'`, under which the rest of the response
    still executes.
    """
    return Agent(
        model,
        output_type=ToolOutput(_bash, name="bash", description="Execute a bash command",
                               # A barrier, so two calls in one response cannot overlap. The protocol
                               # says they run in order, and `True` is what makes that the framework's
                               # behaviour rather than the sandbox's good luck.
                               sequential=True,
                               # Every non-completion spends one, so this is the request budget —
                               # not the framework's default of one, which stops a node on its
                               # second command.
                               max_retries=max_retries),
        end_strategy="early",
        deps_type=_Wiring,
        instructions=f"{instructions.strip()}\n\n{RULES}".strip(),
    )


def _write_trace(path: Path | None, messages: list[Any], wiring: _Wiring, outcome: NodeOutcome) -> str:
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
        handle.write(json.dumps({
            "role": "exit", "content": outcome.reason or outcome.submission,
            "extra": {"status": outcome.status, "route": outcome.route,
                      "submission": outcome.submission, "model_requests": outcome.model_requests,
                      "commands": [{"command": c, "returncode": r, "first_line": f}
                                   for c, r, f in wiring.ran]},
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
    """
    wiring = _Wiring(sandbox=NodeSandbox(
        tree=request.workspace, node_id=request.roles or request.execution_id,
        network=request.network, timeout_seconds=request.timeout_seconds,
        routes=request.routes, inputs=request.inputs), routes=request.routes)
    wiring.sandbox.require_working()

    agent = build_agent(model, instructions=request.instructions,
                        max_retries=request.max_requests)
    limits = UsageLimits(request_limit=request.max_requests)
    messages: list[Any] = []
    try:
        result = await agent.run(request.task, deps=wiring, usage_limits=limits)
        messages = list(result.all_messages())
        produced = result.output
        outcome = NodeOutcome(
            status=COMPLETED, submission=produced.submission, route=produced.route,
            model_requests=int(result.usage.requests), files=_files(request.workspace))
    except UsageLimitExceeded as exc:
        # Out of turns, nothing submitted. Not a failure of the work, and not a route: the graph must
        # not move on the strength of a pass that did not happen.
        outcome = NodeOutcome(
            status=BUDGET_EXHAUSTED, model_requests=wiring.commands,
            reason=f"out of requests after {wiring.commands} commands: {exc}",
            files=_files(request.workspace))
    except Exception as exc:                      # noqa: BLE001 - every failure is a recorded status
        outcome = NodeOutcome(
            status=FAILED, model_requests=wiring.commands,
            reason=f"{type(exc).__name__}: {exc}", files=_files(request.workspace))

    trace = _write_trace(request.trace, messages, wiring, outcome)
    return NodeOutcome(**{**outcome.__dict__, "trace_ref": trace})
