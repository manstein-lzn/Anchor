"""Run one agent node: decide what a previous attempt left, then walk the harness loop.

`run_agent_node` is the only entry point the Graph is allowed to call (ADR-062). Two halves: what this
attempt starts from — a verdict on the previous one, the budget it has left, the history to continue —
and then one pass of `agent.iter` with the capability stack assembled in the order the invariants fix.

**Nothing here is a framework type.** The caller passes a model and capabilities; the caller gets a
`NodeOutcome`. A `Verdict`, a `RecoveryRef` and a store handle do not leave this module.

**Exceptions are statuses.** Every failure path in `run_agent_node` returns `failed` or `uncertain`
with a reason: the Graph is promised a result, and a runner that can raise out of its entry point has
made the caller responsible for guessing a scheduler state from an exception type.
"""

from __future__ import annotations

import asyncio
import json
import os
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any

from pydantic_ai import ModelRequestNode, UsageLimits
from pydantic_ai.messages import ModelMessagesTypeAdapter, ModelRequest
from pydantic_ai.exceptions import UsageLimitExceeded

from anchor.node import (BUDGET_EXHAUSTED, COMPLETED, FAILED, UNCERTAIN, NodeOutcome,
                         NodeRequest)
from anchor.node.agent_runtime import _CountingModel, _Wiring, build_agent
from anchor.node.recovery import budget_path, load_budget, open_store
from anchor.runtime.execenv import NodeSandbox

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
                     ref: Any, *, spent: int, ran: str = "") -> str:
    """The token for the attempt that just finished, so a caller can ask about it later.

    Handed out even when the attempt failed: what a caller needs in order to ask "can this be picked up"
    is a name, and making it invent one would put the store's shape into the Graph. The framework's run
    id is read back from the store rather than guessed, because the capability derives its own when it is
    not given one and a guessed id would name a run that does not exist.
    """
    from anchor.node.recovery import Budget, RecoveryRef, save_budget
    # **Nothing is added up here.** The allowance was written down as each request was made, so the file
    # already holds the total; adding this attempt's count again double-counted it — measured, by a budget
    # of eight from four requests against a starting two.
    if ref is not None and store is not None:
        # Continuing: the attempt that just finished is a **new** run, so the reference has to name it
        # rather than the one it was given. When the caller supplied its own persistence the adapter
        # never chose that id, so it is read back from the store — the newest run for this logical node,
        # which is the one that just ended.
        newest = ""
        if not ran:
            try:
                runs = [item for item in await store.list_runs() if item.agent_name == ref.node]
                newest = sorted(runs, key=lambda item: item.started_at)[-1].run_id if runs else ""
            except Exception:                                 # noqa: BLE001 - naming is best effort
                newest = ""
        budget = load_budget(Path(ref.store))
        save_budget(Path(ref.store), budget)
        return RecoveryRef(node=ref.node, run=ran or newest or ref.run, store=ref.store,
                           workspace=ref.workspace
                           or str(Path(request.workspace).resolve()),
                           budget=budget).encode()
    if recovery_store is None:
        return ""
    control = Path(recovery_store)
    store = open_store(control)
    runs = [item for item in await store.list_runs() if item.agent_name == request.node_key]
    if not runs:
        return ""
    newest = sorted(runs, key=lambda item: item.started_at)[-1]
    # Model and summary requests already charged the file before they were sent. The fallback is for
    # a caller whose model did not have a control directory to charge during execution.
    budget = load_budget(control) if budget_path(control).exists() else Budget(
        requests_used=spent, requests_allowed=request.max_requests)
    save_budget(control, budget)
    return RecoveryRef(node=request.node_key, run=newest.run_id, store=str(control),
                       workspace=str(Path(request.workspace).resolve()),
                       budget=budget).encode()


def _write_trace(path: Path | None, messages: list[Any], wiring: _Wiring | None,
                 outcome: NodeOutcome | None = None, formed: Any = None) -> str:
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
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("w", encoding="utf-8") as handle:
        for message in encoded:
            handle.write(json.dumps(message, ensure_ascii=False, default=str) + "\n")
        # The commands are written out whole, and not left to the framework's history. A pass that
        # submits leaves the loop at the node *after* its last batch — the node whose running is what
        # would put that batch's results into `all_messages` — so the last batch, which is the one
        # carrying the submission, is not in the messages at all. The adapter ran those commands and
        # knows exactly what they did; the record would be missing the most important part of the pass
        # without this.
        if outcome is not None:
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
    os.replace(temporary, path)
    return str(path)


def _files(workspace: Path) -> tuple[str, ...]:
    if not workspace.is_dir():
        return ()
    return tuple(sorted(
        str(item.relative_to(workspace)) for item in workspace.rglob("*")
        if item.is_file() and ".git" not in item.relative_to(workspace).parts))


@dataclass
class _Started:
    """What this attempt starts from.

    Either an `outcome` — the attempt stops here, because a previous one cannot be safely continued — or
    the state the run needs: whose history to continue, which framework run to record under, what has
    already been spent. A dataclass rather than a tuple because eight positional values is a thing
    nobody reads correctly, and this is the code that decides what a caller receives.
    """

    outcome: NodeOutcome | None = None
    resumed: list[Any] = field(default_factory=list)
    history: list[Any] | None = None
    conversation: str = ""
    #: The **total** allowance for the whole logical execution, and what earlier processes spent of it.
    #: Two cumulative figures, so what is left is a subtraction and never a figure of its own — mixing a
    #: remaining count with a cumulative one is how `remaining=0` turned into a fresh allowance.
    spending: int | None = None
    already: int = 0
    this_run: str = ""
    ref: Any = None
    store: Any = None


async def _what_a_previous_attempt_left(request: NodeRequest, capabilities: tuple[Any, ...],
                                       recovery_store: Path | None) -> _Started:
    """Decide what a previous attempt left behind, **before a model is built**.

    A token that cannot be resolved stops the attempt rather than becoming a fresh run of the task: a
    fresh run would repeat whatever the previous attempt managed to do and call it progress (§38). The
    adapter decides here and the Graph never sees why — it gets a status and a reason.
    """
    # **The store's spelling of this node, and every id built from it.** A node called `work/draft`
    # is refused by the framework's step store, which interpolates an identifier into a path; the one
    # name that is legal and stable is `request.node_key`, and both the conversation and the run ids
    # below are that name and not `execution_id` — two spellings in one store is a node that cannot
    # find its own last attempt.
    started = _Started(conversation=request.node_key, spending=request.max_requests)
    # **What earlier processes already spent.** Counted against this attempt too, so the cap is a cap on
    # the whole logical execution rather than a fresh allowance per process — a node killed three times
    # would otherwise spend its budget three times over.
    if recovery_store is not None:
        on_disk = load_budget(Path(recovery_store))
        started.already = on_disk.requests_used
        # **The smaller allowance wins**, and the larger spending. A reference must not be able to hand
        # back an allowance the control directory already says was used, and a directory that has no
        # allowance of its own takes the caller's.
        started.spending = min((value for value in
                               (request.max_requests, on_disk.requests_allowed)
                               if value is not None), default=None)
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
            started.ref = RecoveryRef.decode(request.recovery)
        except Exception as exc:                              # noqa: BLE001 - a status, not a raise
            return _Started(outcome=NodeOutcome(
                status=FAILED, reason=f"the recovery reference is not usable: {exc}",
                model_requests=0, files=_files(request.workspace)))
        try:
            started.store = open_store(Path(started.ref.store))
            verdict = await assess(started.store, started.ref)
        except Exception as exc:                              # noqa: BLE001 - a status, not a raise
            # The entry point promised a result; a store it cannot read is a failed attempt with a
            # reason, not an exception for the caller to catch.
            return _Started(outcome=NodeOutcome(
                status=FAILED,
                reason=f"the recovery records cannot be read: {type(exc).__name__}: {exc}",
                model_requests=0, files=_files(request.workspace), recovery=request.recovery))
        # **Silence on this path, deliberately.** An uncertain attempt asks the model nothing and runs
        # no command: the effect may already have happened, and doing anything with that uncertainty
        # other than reporting it is how a side effect happens twice.
        if verdict.action == "uncertain":
            return _Started(outcome=NodeOutcome(
                status=UNCERTAIN, reason=verdict.because, model_requests=0,
                files=_files(request.workspace), recovery=request.recovery))
        if verdict.action == "invalid":
            return _Started(outcome=NodeOutcome(
                status=FAILED, reason=verdict.because, model_requests=0,
                files=_files(request.workspace), recovery=request.recovery))
        # **It already submitted.** Nothing is run and no model is asked: the result is recorded, and
        # the one action that must never happen twice is the submission.
        #
        # Read from the **fact the protocol wrote**, not from the history's text. The history holds the
        # observation the model was shown, and the marker is in it whether or not the protocol accepted
        # it — a command that printed the marker and exited 1, which is refused on the normal path, was
        # reported here as a finished node with the refusal as its submission.
        if verdict.action == "finished":
            from anchor.node.recovery import already_finished
            submission, route = already_finished(Path(started.ref.store), started.ref.node)
            if not submission:
                return _Started(outcome=NodeOutcome(
                    status=FAILED,
                    reason="a completion is recorded for this node but cannot be read back — refusing "
                           "rather than reporting a result that is not there",
                    model_requests=0, files=_files(request.workspace),
                    recovery=request.recovery))
            return _Started(outcome=NodeOutcome(
                status=COMPLETED, submission=submission, route=route, model_requests=0,
                files=_files(request.workspace), recovery=request.recovery))
        # `replayable` means nothing entered a tool, so the attempt is the first one in effect.
        if verdict.action == "continuable":
            started.history = await continued_messages(started.store, started.ref)
            # The reference's own figures are merged into the same two cumulative numbers: larger
            # spending, smaller allowance. Checked **again** here because a reference can arrive with a
            # spent budget even when the directory looked fine a moment ago.
            merged = started.ref.budget.at_most(load_budget(Path(started.ref.store)))
            started.already = max(started.already, merged.requests_used)
            started.spending = min((value for value in
                                   (started.spending, merged.requests_allowed)
                                   if value is not None), default=None)
        if started.store is not None and not has_persistence:
            started.conversation = started.ref.node
            # **A continuation is a new run**, so the attempt that ends up in the store is this one and
            # not the one the reference named. Reporting the old id would make the next caller assess
            # the *previous* attempt — which is settled and already submitted — and start the work over.
            started.this_run = await _next_run_id(started.store, started.ref.node)
            started.resumed.append(_StepPersistence(store=started.store, agent_name=started.ref.node,
                                                   run_id=started.this_run))

    elif recovery_store is not None and not has_persistence:
        # A fresh attempt still records itself, so that a later process can be told about it — and so
        # that the second attempt's run id does not collide with the first's. The framework refuses a
        # repeated `run_id` outright, which is what made this necessary rather than convenient.
        from pydantic_ai_harness import StepPersistence
        started.store = open_store(Path(recovery_store))
        started.resumed.append(StepPersistence(store=started.store, agent_name=request.node_key,
                                               run_id=await _next_run_id(started.store,
                                                                         request.node_key)))

    return started


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
    started = await _what_a_previous_attempt_left(request, capabilities, recovery_store)
    if started.outcome is not None:
        return started.outcome
    resumed, already = started.resumed, started.already
    history, conversation = started.history, started.conversation
    ref, store, spending = started.ref, started.store, started.spending
    this_run = started.this_run

    # **Spent is spent.** The allowance is a total for the logical execution, so what is left is the
    # subtraction and there is no minimum: a spent budget means this attempt makes no request at all
    # rather than one more. The old expression both gave a minimum of one and, when the remaining figure
    # reached zero, replaced the whole allowance with a fresh one — measured, by a budget of 8/8 that
    # came back 9/8 with a completed status.
    remaining = max(spending - already, 0) if spending is not None else None
    if remaining is not None and remaining <= 0:
        return NodeOutcome(
            status=BUDGET_EXHAUSTED, model_requests=0, files=_files(request.workspace),
            reason=f"the allowance for this node is spent ({already}/{spending} requests) — no request "
                   f"was made and none will be, because a budget that can be exceeded is not a budget",
            recovery=request.recovery)

    counted = _CountingModel(model)
    # **A caller that passes only a reference still gets its spending recorded.** The reference names the
    # control directory; requiring the caller to pass the path as well meant an attempt made through a
    # reference alone spent requests nobody wrote down.
    where = recovery_store if recovery_store is not None else (
        started.ref.store if started.ref is not None else None)
    if where is not None:
        # Whatever was already spent counts against this attempt too, so the cap is a cap on the whole
        # logical execution and not on each process it happens to run in.
        counted.control = where
        counted.allowed = spending
        counted.requests = already
    agent = build_agent(counted, instructions=request.instructions,
                        # Malformed tool/output retries are separate from healthy research turns.
                        max_retries=spending if spending is not None else 3,
                        capabilities=(*capabilities, *resumed))
    limits = UsageLimits(request_limit=remaining)

    wiring: _Wiring | None = None
    run: Any = None
    messages: list[Any] = []
    #: The last batch's already-formed results, read off the node the pass left on.
    formed: Any = None
    current = asyncio.current_task()

    async def stop_when_asked() -> None:
        while request.cancelled is not None and not request.cancelled():
            await asyncio.sleep(0.1)
        if current is not None:
            current.cancel()

    watcher = asyncio.create_task(stop_when_asked()) if request.cancelled is not None else None
    try:
        wiring = _Wiring(sandbox=NodeSandbox(
            tree=request.workspace, node_id=request.roles or request.execution_id,
            network=request.network, timeout_seconds=request.timeout_seconds,
            routes=request.routes, inputs=request.inputs, cancelled=request.cancelled),
            routes=request.routes,
            control=recovery_store, execution_id=request.node_key,
            framework_run=this_run or (ref.run if ref is not None else ""))
        # Inside the try. A sandbox that cannot start is a failed execution and the contract has a
        # status for it; raising out of the entry point would leave the caller with an exception where
        # it was promised a result.
        wiring.sandbox.require_working()

        async with agent.iter(request.task, deps=wiring, usage_limits=limits,
                              message_history=history,
                              conversation_id=conversation) as run:
            async for node in run:
                if request.trace is not None:
                    pending = (node.request if isinstance(node, ModelRequestNode) and
                               any(getattr(part, "part_kind", "") in ("tool-return", "retry-prompt")
                                   for part in node.request.parts) else None)
                    _write_trace(request.trace, list(run.all_messages()), wiring, formed=pending)
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
            model_requests=counted.requests - already, files=_files(request.workspace))
    except asyncio.CancelledError:
        messages = list(run.all_messages()) if run is not None else []
        outcome = NodeOutcome(status=FAILED, reason="stopped on request",
                              model_requests=counted.requests - already,
                              files=_files(request.workspace))
    except UsageLimitExceeded as exc:
        # Out of turns, nothing submitted. Not a failure of the work, and not a route: the graph must
        # not move on the strength of a pass that did not happen.
        messages = list(run.all_messages()) if run is not None else []
        outcome = NodeOutcome(
            status=BUDGET_EXHAUSTED, model_requests=counted.requests - already,
            reason=f"out of requests after {counted.requests} model requests: {exc}",
            files=_files(request.workspace))
    except Exception as exc:                      # noqa: BLE001 - every failure is a recorded status
        messages = list(run.all_messages()) if run is not None else []
        outcome = NodeOutcome(
            status=FAILED, model_requests=counted.requests - already,
            reason=f"{type(exc).__name__}: {exc}", files=_files(request.workspace))
    finally:
        if watcher is not None:
            watcher.cancel()

    return replace(outcome, trace_ref=_write_trace(request.trace, messages, wiring, outcome, formed),
                   recovery=await _reference(recovery_store, request, store, ref,
                                             spent=outcome.model_requests, ran=this_run))


#: The name ADR-062 freezes: `run_agent_node` is the only harness entry point the Graph may call. It is
#: `run_node` under the frozen name — kept as an alias rather than a rename so the verified packages,
#: the tests and `scripts/recovery_windows.py` keep working without a four-file churn that buys nothing.
run_agent_node = run_node
