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
from pydantic_ai.messages import ModelMessagesTypeAdapter, ModelRequest
from pydantic_ai.models.wrapper import WrapperModel
from pydantic_ai.exceptions import ModelRetry, UsageLimitExceeded

from anchor.node import (BUDGET_EXHAUSTED, COMPLETED, FAILED, UNCERTAIN, NodeOutcome,
                         NodeRequest)
from anchor.node.recovery import budget_path, load_budget, open_store
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


async def _next_run_id(store: Any, agent_name: str) -> str:
    """A run id that has not been used for this logical node before.

    The framework refuses a repeated `run_id` outright — `run_id is single-shot; pass conversation_id=`
    — so the sequence is carried by the conversation and each attempt is numbered. Derived from what the
    store already holds rather than from a counter in memory, because the whole point is that a *new
    process* has to be able to continue without colliding with the attempts before it.
    """
    used = [item.run_id for item in await store.list_runs() if item.agent_name == agent_name]
    return f"{agent_name}-a{len(used) + 1}"


async def _reference(recovery_store: Path | None, request: NodeRequest, store: Any,
                     ref: Any, *, spent: int) -> str:
    """The token for the attempt that just finished, so a caller can ask about it later.

    Handed out even when the attempt failed: what a caller needs in order to ask "can this be picked up"
    is a name, and making it invent one would put the store's shape into the Graph. The framework's run
    id is read back from the store rather than guessed, because the capability derives its own when it is
    not given one and a guessed id would name a run that does not exist.
    """
    from anchor.node.recovery import Budget, RecoveryRef, save_budget
    if ref is not None and store is not None:
        # Continuing an attempt: same run, and the allowance it had is now smaller by what was spent.
        budget = ref.budget.after(spent)
        save_budget(Path(ref.store), budget)
        return RecoveryRef(node=request.execution_id, run=ref.run, store=ref.store,
                           budget=budget).encode()
    if recovery_store is None:
        return ""
    control = Path(recovery_store)
    store = open_store(control)
    runs = [item for item in await store.list_runs() if item.agent_name == request.execution_id]
    if not runs:
        return ""
    newest = sorted(runs, key=lambda item: item.started_at)[-1]
    # **The allowance carries over, it does not restart.** The framework is explicit that it does not
    # restore retry counters, so what was spent is kept beside the run — a node killed three times would
    # otherwise spend its whole budget three times.
    budget = load_budget(control).after(spent) if budget_path(control).exists() else Budget(
        requests_used=spent, requests_allowed=request.max_requests)
    save_budget(control, budget)
    return RecoveryRef(node=request.execution_id, run=newest.run_id, store=str(control),
                       budget=budget).encode()


def _write_trace(path: Path | None, messages: list[Any], wiring: _Wiring | None,
                 outcome: NodeOutcome, formed: Any = None) -> str:
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
    # **The batch the loop left on.** A pass that submits leaves at the node whose *running* would put
    # its last batch into the message history — but that node is constructed with the results already
    # in it, so they are readable without running it and without asking the model anything. Read, not
    # re-executed: this is the framework's own `ToolReturnPart`s, with their `tool_call_id`s, so the
    # record can be correlated and the batch is complete.
    recorded = list(messages)
    if formed is not None:
        parts = list(getattr(formed, "parts", ()) or ())
        if parts:
            recorded.append(ModelRequest(parts=parts))
    encoded = ModelMessagesTypeAdapter.dump_python(recorded, mode="json") if recorded else []
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
                      # The full output, uncapped. The sandbox already bounds what one command may
                      # produce; a second, smaller bound here would drop evidence that was inside the
                      # bound it was allowed — silently, which is the part that made it a defect.
                      "commands": [] if wiring is None else
                                  [{"command": c, "returncode": r, "first_line": f, "output": o}
                                   for c, r, f, o in wiring.ran]},
        }, ensure_ascii=False) + "\n")
    return str(path)


def _files(workspace: Path) -> tuple[str, ...]:
    if not workspace.is_dir():
        return ()
    return tuple(sorted(
        str(item.relative_to(workspace)) for item in workspace.rglob("*")
        if item.is_file() and ".git" not in item.relative_to(workspace).parts))


async def run_node(request: NodeRequest, *, model: Any,
                   capabilities: tuple[Any, ...] = (),
                   recovery_store: Path | None = None) -> NodeOutcome:
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
    # ── What a previous attempt left, before anything is started. ──
    #
    # **A token that cannot be resolved stops the attempt before a model is built.** That is the whole
    # point of §38: a reference that does not check out must not become a fresh run of the task, because
    # a fresh run would repeat whatever the previous attempt managed to do and call it progress. The
    # adapter decides here, and the Graph never sees why — it gets a status and a reason.
    resumed: list[Any] = []
    history: list[Any] | None = None
    ref = None
    store = None
    spending = request.max_requests
    #: The **logical** node execution, stable across attempts, and the framework's `conversation_id`.
    #: A retry is a *new* `Agent.run` — the framework refuses to reuse a `run_id`, measured — so the
    #: sequence is the conversation and each attempt gets its own run.
    conversation = request.execution_id
    #: Whether the caller has already arranged persistence. **Where it sits among the capabilities
    #: decides which side of a write a hook lands on**, and the two directions are not the same — a
    #: `before_*` hook registered after it sees the framework's `started` write, while an `after_*` hook
    #: registered before it sees the terminal write. A caller testing a specific boundary has to place
    #: it; the adapter's job is to not add a second one.
    from pydantic_ai_harness import StepPersistence as _StepPersistence
    has_persistence = any(isinstance(item, _StepPersistence) for item in capabilities)

    if request.recovery:
        from anchor.node.recovery import RecoveryRef, assess, continued_messages
        try:
            ref = RecoveryRef.decode(request.recovery)
        except Exception as exc:                              # noqa: BLE001 - a status, not a raise
            return NodeOutcome(status=FAILED, reason=f"the recovery reference is not usable: {exc}",
                               model_requests=0, files=_files(request.workspace))
        store = open_store(Path(ref.store))
        verdict = await assess(store, ref)
        # **Silence on this path, deliberately.** An uncertain attempt asks the model nothing and runs
        # no command: the effect may already have happened, and doing anything with that uncertainty
        # other than reporting it is how a side effect happens twice.
        if verdict.action == "uncertain":
            return NodeOutcome(status=UNCERTAIN, reason=verdict.because, model_requests=0,
                               files=_files(request.workspace), recovery=request.recovery)
        if verdict.action == "invalid":
            return NodeOutcome(status=FAILED, reason=verdict.because, model_requests=0,
                               files=_files(request.workspace), recovery=request.recovery)
        # `replayable` means nothing entered a tool, so the attempt is the first one in effect.
        if verdict.action == "continuable":
            history = await continued_messages(store, ref)
            spending = max(ref.budget.remaining, 0) or request.max_requests
        if store is not None and not has_persistence:
            conversation = ref.node
            resumed.append(_StepPersistence(store=store, agent_name=ref.node,
                                            run_id=await _next_run_id(store, ref.node)))

    elif recovery_store is not None and not has_persistence:
        # A fresh attempt still records itself, so that a later process can be told about it — and so
        # that the second attempt's run id does not collide with the first's. The framework refuses a
        # repeated `run_id` outright, which is what made this necessary rather than convenient.
        from pydantic_ai_harness import StepPersistence
        store = open_store(Path(recovery_store))
        resumed.append(StepPersistence(store=store, agent_name=request.execution_id,
                                       run_id=await _next_run_id(store, request.execution_id)))

    counted = _CountingModel(model)
    agent = build_agent(counted, instructions=request.instructions,
                        max_retries=spending,
                        capabilities=(*capabilities, *resumed))
    limits = UsageLimits(request_limit=spending)
    wiring: _Wiring | None = None
    run: Any = None
    messages: list[Any] = []
    #: The last batch's already-formed results, read off the node the pass left on.
    formed: Any = None
    try:
        wiring = _Wiring(sandbox=NodeSandbox(
            tree=request.workspace, node_id=request.roles or request.execution_id,
            network=request.network, timeout_seconds=request.timeout_seconds,
            routes=request.routes, inputs=request.inputs), routes=request.routes)
        # Inside the try. A sandbox that cannot start is a failed execution and the contract has a
        # status for it; raising out of the entry point would leave the caller with an exception where
        # it was promised a result.
        wiring.sandbox.require_working()

        async with agent.iter(request.task, deps=wiring, usage_limits=limits,
                              message_history=history,
                              conversation_id=conversation) as run:
            async for node in run:
                # **The boundary after the tools, not the tools themselves.** A node is yielded when it
                # is entered, so at `CallToolsNode` the commands have not run yet and nothing has been
                # submitted; breaking there does nothing and the pass goes on to ask the model again —
                # measured, by a third request appearing in the record. The node that asks the model is
                # exactly the one to leave on: a submission is already in hand, so asking again is the
                # thing this is here to prevent.
                if isinstance(node, ModelRequestNode) and wiring.done is not None:
                    formed = node.request
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

    return replace(outcome, trace_ref=_write_trace(request.trace, messages, wiring, outcome, formed),
                   recovery=await _reference(recovery_store, request, store, ref,
                                             spent=outcome.model_requests))
