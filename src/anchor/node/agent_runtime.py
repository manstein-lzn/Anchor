"""One agent node's model loop: the Agent, its single bash tool, and the completion protocol.

This is the harness half of the node runtime. What runs a node — deciding whether a previous attempt may
be continued, charging the budget, writing the recovery reference — is `adapter.py`. The split is the
one ADR-062 names, and it is a split rather than a layer: this module does not know what a `NodeRequest`
is, and the adapter does not know what `agent.iter` yields.

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

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from pydantic_ai import Agent, ModelRetry, RunContext
from pydantic_ai.models.wrapper import WrapperModel

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
    #: Where to write the allowance down **as it is spent**. A budget recorded only when an attempt
    #: ends is a budget a killed attempt never pays into — measured, by a node that had made three
    #: requests reporting one, because two of them belonged to the attempt that was killed.
    control: Any = None
    #: The **total** allowance for the logical execution, not this process's share of it. One notion,
    #: used the same way everywhere, because mixing a remaining figure with a cumulative one is how the
    #: allowance came to be raised instead of spent.
    allowed: int = 0

    async def request(self, messages: Any, model_settings: Any, model_request_parameters: Any) -> Any:
        self.requests += 1
        self._record()
        return await super().request(messages, model_settings, model_request_parameters)

    def _record(self) -> None:
        """Charge this request against the logical execution's allowance.

        **One increment, one writer.** This used to write `max(my own count, what is on disk)`, which is
        not a count of anything once a second kind of request — a summary — charges the same file: the two
        interleavings produced an under-count and an over-count in the same wiring, both measured. The
        increment lives in `recovery.charge_request` so every kind of request goes through one rule.
        """
        if self.control is None:
            return
        from anchor.node.recovery import charge_request
        charge_request(Path(str(self.control)), self.allowed)


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
    #: Where an accepted completion is recorded, and under what identity. The control directory is the
    #: node's own record, outside the workspace it can write (§17); the two ids bind the fact to this
    #: node and to the framework run that produced it.
    control: Any = None
    execution_id: str = ""
    framework_run: str = ""
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
    marker is refused, and that check is the whole reason this is written out rather than shared.
    The parser it replaced read the markers and never looked at the exit code first, so an
    `anchor-route` that died mid-write still routed there; this side does it the way the protocol says.

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
    if kind in ("done", "routed") and wiring.control is not None:
        # **Written here, where the protocol says yes.** Recovery reads this fact rather than looking for
        # the marker in a rendering of the output: before it existed, a command that printed the marker
        # and exited 1 — which this very call refuses, and tells the model so — was reported by recovery
        # as a finished node, with the refusal text as its submission.
        from datetime import datetime, timezone

        from anchor.node.recovery import CompletionFact, record_completion
        record_completion(Path(wiring.control), CompletionFact(
            node=wiring.execution_id, run=wiring.framework_run or "",
            kind=kind, submission=message, route=route, command=command,
            at=datetime.now(timezone.utc).isoformat()))
    # A refusal and an ordinary result travel the same way — back to the model as a plain tool result,
    # which is also what the harness can read and clear. `ModelRetry` is used for neither: it would
    # attach correction instructions to a command's ordinary output and record it as a retry.
    return _observation(ran, message if kind == "refused" else "")


def build_agent(model: Any, *, instructions: str = "",
                max_retries: int = 60, capabilities: tuple[Any, ...] = ()) -> Agent[_Wiring, str]:
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
        # Where a later package's capabilities attach — context compaction, step persistence. A seam
        # rather than a hook list, so what a node's runner is made of stays in one place.
        capabilities=list(capabilities),
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


