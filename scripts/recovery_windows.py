#!/usr/bin/env python3
"""Kill a node at a chosen instant, and see what can honestly be said afterwards.

Each window is a real node, in a real bubblewrap sandbox, running a command that really writes a
counter — and the kill is a real `SIGKILL` of the process from outside it.

**The instant is chosen by the node, not guessed by the parent.** The node reaches the window, writes
one byte down a pipe, and blocks; the parent is reading that pipe and kills the moment the byte
arrives. Sleeping for "about long enough" would make every window a statement about the machine's speed
rather than about the boundary, and the same test would pass or fail depending on the load.

**Two modes, one file.** Without `--child` it runs every window and reports; with `--child` it runs one
node and is expected to be killed. The child never decides anything — it is the thing being interrupted,
and its only job is to say when it has arrived somewhere.

    scripts/recovery_windows.py                     # every window, with a bounded timeout
    scripts/recovery_windows.py --only C3           # one of them
"""

from __future__ import annotations

import argparse
import contextlib

from pydantic_ai.capabilities import AbstractCapability
import re
import asyncio
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import traceback
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

#: Where the child says it has arrived. The parent reads this and nothing else decides the moment.
READY_FD_ENV = "ANCHOR_READY_FD"


@dataclass
class Evidence:
    """What one window produced, in the terms §61 asks for.

    `killed` is `None` for the windows that are not about a kill at all — C6, C7 and C8 check what a
    recovery does and what a reference means, and calling them "never reached the barrier" would be the
    runner mistaking its own shape for a result.
    """

    window: str
    killed: bool | None
    exit_code: int | None
    barrier: str
    counter_before: int
    counter_after: int
    verdict: str
    because: str
    effects: list[list[str]] = field(default_factory=list)
    events: list[str] = field(default_factory=list)
    snapshot: str = ""
    budget: str = ""
    seconds: float = 0.0
    note: str = ""
    traceback: str = ""
    #: Where the store and the workspace are, so the evidence can be re-read and re-checked rather than
    #: trusted. §61 asks for the ledger, the snapshot and the git state; these are how to reach them.
    control: str = ""
    workspace: str = ""

    def lines(self) -> list[str]:
        out = [f"=== {self.window} ===",
               f"  killed={self.killed} exit={self.exit_code} barrier={self.barrier!r} "
               f"({self.seconds:.2f}s)",
               f"  counter: {self.counter_before} -> {self.counter_after}",
               f"  verdict: {self.verdict} — {self.because}"]
        for call_id, name, status in self.effects:
            out.append(f"    effect {call_id} {name} = {status}")
        if self.events:
            out.append("    events: " + ", ".join(self.events))
        if self.snapshot:
            out.append(f"    snapshot: {self.snapshot}")
        if self.budget:
            out.append(f"    budget: {self.budget}")
        if self.note:
            out.append(f"  note: {self.note}")
        if self.traceback:
            out.append("  child traceback:\n" + "\n".join(
                "    " + line for line in self.traceback.splitlines()[-12:]))
        return out


# ── the child ────────────────────────────────────────────────────────────────────────────────────

def _wait_for_a_kill(where: str) -> None:
    """Say we have arrived, then block until the parent kills us.

    Written as bytes down a pipe because a pipe is a handshake: the parent is blocked on the read, so
    the kill lands as soon as this returns rather than at the next time somebody looks. `signal.pause`
    blocks until a signal arrives, and the signal that arrives is `SIGKILL`, which does not return.
    """
    fd = int(os.environ.get(READY_FD_ENV, "-1"))
    if fd < 0:
        raise RuntimeError(f"{READY_FD_ENV} is not set — the child cannot signal its window")
    os.write(fd, f"{where}\n".encode("utf-8"))
    os.close(fd)
    while True:                                              # pragma: no cover - killed here
        signal.pause()


def _report_window(window: str, where: str) -> None:
    """Say a hook was reached without pausing. The parent reads the ledger at that instant and lets the
    child run on — for the windows that are questions about the framework rather than kills."""
    fd = int(os.environ.get(READY_FD_ENV, "-1"))
    if fd >= 0:
        os.write(fd, f"{window}@{where}\n".encode("utf-8"))


def _summariser(control: Path, constraint: str):
    """A deterministic summariser: it summarises **only what is in its input**, and counts itself.

    §78 asks for a double that gives a deterministic summary for the constraints actually present, and
    forbids hard-coding the right answer — so the summary is assembled out of the input's own lines and
    nothing else. Its calls are appended to a file, because a summary is a paid request and the budget
    question (B5) is whether it is counted against the same allowance as the node's own model.
    """
    from pydantic_ai.messages import ModelResponse
    from pydantic_ai.messages import TextPart
    from pydantic_ai.models.function import FunctionModel

    log = control / "summariser-calls.log"

    def summarise(messages: Any, info: Any) -> Any:
        with log.open("a", encoding="utf-8") as handle:
            handle.write("call\n")
        text = "\n".join(str(getattr(part, "content", ""))
                         for message in messages
                         for part in (getattr(message, "parts", ()) or ()))
        kept = [line.strip() for line in text.splitlines()
                if constraint and constraint in line]
        body = "SUMMARY. " + (" ".join(kept[:4]) if kept else "no constraint present in the input")
        return ModelResponse(parts=[TextPart(content=body)])

    return FunctionModel(summarise)


#: Windows whose barrier has to land **after** the framework's own write, which means registering it
#: **before** step persistence. See the note in `_run_child`: the two hook directions differ.
BEFORE_PERSISTENCE_WINDOWS = frozenset({"C1", "C4", "C5", "B1", "B2-checkpoint", "B3-terminal"})


class Barrier(AbstractCapability[Any]):
    """The capability that stops the node at the chosen boundary.

    The four tool-related hooks are not interchangeable and the boundaries they give are not the same ones
    people assume. Measured on this build:

        after_model_request   tool_call_started is NOT yet in the ledger
        before_tool_execute   tool_call_started IS in the ledger, the command has not run
        wrap_tool_execute     entered after both, so a pause after its handler has the effect done and
                              no terminal record

    Which is why C1 pauses in the first, C2 in the second, and C3 between the handler and its return.

    **The capability's position decides which side of a write a hook lands on**, because `before_*` hooks
    run in registration order and `after_*` hooks run in reverse. A case that needs to see the framework's
    own write goes first in the tuple; one that needs to see the state before it goes last.

    The compaction-shaped cases carry an extra condition: they are armed only once the record shows a
    **real** compaction, because a pause that lands in an ordinary conversation proves nothing about
    compaction — which is what the first version of B1 did.
    """

    def __init__(self, window: str, script: dict, record: Any = None,
                 control: Path | None = None) -> None:
        self.window = window
        self.script = script
        self.record = record
        self.control = control
        self.seen_models = 0
        self.seen_tools = 0
        self.since_compaction = 0
        self.models_at_compaction = 0
        #: How many settled cycles to let through before pausing. Two by default — one command's turn and
        #: the request that follows it — and a case whose kill has to land *after* a submission says so
        #: rather than hoping the boundary falls in the right place.
        self.kill_after = int(script.get("kill_after_models", 2))

    # ── compaction-shaped cases ──────────────────────────────────────────────────────────────────

    def _compacted(self) -> bool:
        """Whether a real compaction has happened, remembering where the request count stood."""
        if self.record is None or not self.record.compactions:
            return False
        if not self.models_at_compaction:
            self.models_at_compaction = self.seen_models
        return True

    def _pause_for_compaction_case(self) -> bool:
        """The model-side compaction windows, in one place so the hook stays readable."""
        if self.window == "B3-started" or self.window == "B2-before-compaction" or \
                self.window == "B3-effect":
            return False                                      # armed in the tool hooks instead
        if not self._compacted():
            return False
        if self.window == "B2-checkpoint":
            return self.seen_models >= self.models_at_compaction + 2
        if self.window == "B1":
            self.since_compaction += 1
            return self.since_compaction >= 2
        return False

    def _pause_for_compaction_tool_case(self) -> bool:
        """The tool-side compaction windows: armed only once a real compaction has happened."""
        if self.window in ("B2-before-compaction",):
            return True                                       # deliberately before any compaction
        if self.window in ("B2-after-compaction", "B3-started", "B3-effect", "B3-terminal"):
            return self._compacted()
        return False

    # ── the hooks ─────────────────────────────────────────────────────────────────────────────────

    async def after_model_request(self, ctx, *, request_context, response):
        self.seen_models += 1
        if self._pause_for_compaction_case():
            _wait_for_a_kill("compaction done, a later checkpoint is on disk")
        if self.window == "A6":
            # **After the protocol has accepted a submission, before the graph records anything.** Read
            # from the recorded **fact** rather than from the history's text: the marker is in the history
            # whether or not the protocol accepted it, which is the mistake R1 was.
            from anchor.node.recovery import read_completion_fact
            node_id = self.script.get("node", "")
            if self.control is not None and read_completion_fact(self.control, node_id) is not None:
                _wait_for_a_kill("submission persisted, the graph has not finalised")
        if self.window == "C1" and self.seen_models == 1:
            _wait_for_a_kill("after_model_request, before the tool cycle")
        if self.window == "C4" and self.seen_models >= self.kill_after:
            # The snapshot for the cycle that just settled has been written by now; what has not happened
            # is the run ending. How many cycles to let through is the case's business: a kill that has to
            # land *after* a submission needs more than the first one.
            _wait_for_a_kill(f"after {self.kill_after} settled request(s), before the run ends")
        return response

    async def before_tool_execute(self, ctx, *, call, tool_def, args):
        self.seen_tools += 1
        if self.window == "C2" and self.seen_tools == 1:
            _wait_for_a_kill("tool_call_started persisted, command not executed")
        if self.window == "B2-before-compaction":
            # Deliberately before any compaction: this is the control case, and its barrier text says so
            # rather than borrowing the post-compaction one and reading as though a summary had happened.
            _wait_for_a_kill("before any compaction: the first command has not run")
        if self.window == "B2-after-compaction" and self._pause_for_compaction_tool_case():
            _wait_for_a_kill("after a compaction, before the next command runs")
        if self._pause_for_compaction_tool_case() and self.window == "B3-started":
            _wait_for_a_kill("after a compaction: started recorded, the command has not run")
        return args                                       # must be returned; None breaks the call

    async def after_tool_execute(self, ctx, *, call, tool_def, args, result):
        # **Reached only because this capability is registered before `StepPersistence`.** Hooks run in a
        # fixed order and the framework's own runs after this one when it is registered later: measured
        # both ways, and registering it after gave a hook where the terminal record had not been written.
        # Registered first, this is the instant C5 asks about — the terminal record is in the ledger and
        # the snapshot for this cycle is not.
        if self.window == "C5" and self.seen_tools == 2:
            _wait_for_a_kill("terminal effect record written, snapshot not yet")
        if self.window == "B3-terminal" and self._pause_for_compaction_tool_case():
            _wait_for_a_kill("after a compaction: terminal record written, snapshot not yet")
        return result

    async def wrap_tool_execute(self, ctx, *, call, tool_def, args, handler):
        result = await handler(args)
        if self.window == "C3" and self.seen_tools == 2:
            # The counter command is the first call; this is the pause after *its* effect and before the
            # framework writes the terminal record for it.
            _wait_for_a_kill("side effect done, terminal record not written")
        if self.window == "B3-effect" and self._pause_for_compaction_tool_case():
            _wait_for_a_kill("after a compaction: effect done, terminal record not written")
        return result


def _barrier(window: str, script: dict, record: Any = None, control: Path | None = None) -> Any:
    """The barrier for one window. A factory so the class itself is not a closure with a dozen branches."""
    return Barrier(window, script, record, control)




def _write_outcome(control: Path, outcome: Any = None) -> None:
    """The outcome's own account, written where the parent can read it. A resumed attempt that only
    fetched history and did nothing would have nothing to put here."""
    if outcome is None:
        (control / f"outcome-{os.getpid()}.json").write_text(
            json.dumps({"status": "graph-finished"}, ensure_ascii=False), encoding="utf-8")
        return
    (control / f"outcome-{os.getpid()}.json").write_text(json.dumps({
        "status": outcome.status, "submission": outcome.submission, "route": outcome.route,
        "model_requests": outcome.model_requests, "reason": outcome.reason,
        "recovery": outcome.recovery, "files": list(outcome.files)}, ensure_ascii=False),
        encoding="utf-8")


async def _next_free_run_id(control: Path, agent_name: str) -> str:
    """The next unused run id for this logical node — the adapter would derive one, and this is the same
    rule applied where the child can see it."""
    from anchor.node.recovery import open_store
    used = [item.run_id for item in await open_store(control).list_runs()
            if item.agent_name == agent_name]
    return f"{agent_name}-a{len(used) + 1}"


def _run_graph_child(window: str, control: Path, workspace: Path, script: dict) -> None:
    """**A real graph, with the candidate Node doing the agent step.**

    The scheduler, the sandbox, the Git commits, the record and the ops are the runtime's own; only
    `_agent_for`'s answer for an **agent** node is this package's entry point. That is the seam the
    scheduler itself dispatches through, and it is the one R5 asks for: the earlier attempt ran the mini
    default path, which has no step store at all, so "no run was recorded" said nothing about the
    candidate architecture — only that that path is not wired to it.

    `run(task=...)` is called with a keyword, and the result is read for `submission`,
    `exit_status == "Submitted"`, and `route` — off both `agent.route` and `agent.env.route`, which are
    the two places `_result_of` looks.
    """
    from types import SimpleNamespace

    from pydantic_ai.messages import ModelResponse, ToolCallPart
    from pydantic_ai.models.function import FunctionModel

    from anchor.node import NodeRequest
    from anchor.node.pydantic_adapter import run_node
    from anchor.simple import run as runner

    def model(messages, info):
        seen = any(script["marker"] in str(getattr(part, "content", ""))
                   for message in messages for part in (getattr(message, "parts", ()) or ()))
        command = script["then"] if seen else script["first"]
        return ModelResponse(parts=[ToolCallPart(tool_name="bash", args={"command": command})])

    class Bridged:
        """What the scheduler is handed for an agent node, backed by `run_node`."""

        def __init__(self, node_id: str, directory: Path, given: tuple, trace: Any,
                     routes: tuple) -> None:
            self.node_id = node_id
            self.directory = Path(directory)
            self.given = given
            self.trace = trace
            self.routes = routes
            self.env = SimpleNamespace(route=None)
            self.route: str | None = None

        def run(self, task: str) -> dict:
            outcome = asyncio.run(run_node(
                NodeRequest(execution_id=self.node_id, task=task or "", workspace=self.directory,
                            inputs=tuple(bind for item in self.given for bind in item.binds()),
                            routes=self.routes, max_requests=int(script.get("max_requests", 8)),
                            trace=Path(self.trace) if self.trace else None),
                model=FunctionModel(model),
                capabilities=(_barrier(window, script, control=control),),
                recovery_store=control))
            self.route = outcome.route
            self.env.route = outcome.route
            return {"submission": outcome.submission,
                    "exit_status": "Submitted" if outcome.status == "completed" else outcome.status}

        def resume(self, messages: list) -> dict:
            raise AssertionError("this bridge does not implement resuming a node")

    real_for = runner._agent_for

    def patched(graph, node_id, directory, models, secret, config_path, inputs=(), trace=None,
                script=None):
        if graph.nodes[node_id].op:
            # An op is the runtime's own, through the real factory: only the agent step is bridged.
            return real_for(graph, node_id, directory, models, secret, config_path,
                            inputs=inputs, trace=trace, script=script)
        return Bridged(node_id, Path(directory), tuple(inputs), trace, graph.routes(node_id))

    runner._agent_for = patched
    runner.run(workspace, config_path=str(control / "runtime.json"))
    _write_outcome(control)


async def _run_child(window: str, control: Path, workspace: Path, script: dict,
                     recover: str = "") -> None:
    """One node, with the barrier attached, expected never to return for the killed windows."""
    from pydantic_ai.models.function import FunctionModel
    from pydantic_ai.messages import ModelResponse, ToolCallPart

    from anchor.node import NodeRequest
    from anchor.node.pydantic_adapter import run_node
    from anchor.node.recovery import save_budget, Budget

    if not (control / "budget.json").exists():
        # The declared allowance follows the request the child is about to make, so a case with more
        # work to do is not silently capped at the default.
        save_budget(control, Budget(requests_used=0,
                                    requests_allowed=int(script.get("max_requests", 8))))

    turn = {"n": 0}

    calls_log = control / "model-calls.log"
    seen_log = control / "model-input.jsonl"

    def model(messages, info):
        """What to do next, decided from **the history this process was handed**.

        And **counted here**, in the place the request actually arrives, appending to a file so the count
        survives the process. §39 asks for the persisted budget to be checked against an independent
        count at the model's end rather than against a number the test decided in advance.

        §77 also asks for a double driven by evidence another process can check, and not by a
        per-process counter that compaction can silently reset. So the marker is looked for in the tool
        results already in the history: a resumed process sees the completed command's output and moves
        on, and a process that sees nothing does the work — which is what a real model would do.
        """
        with calls_log.open("a", encoding="utf-8") as handle:
            handle.write("call\n")
        # **What the model actually received**, written by the model itself. `chars` is the whole
        # rendered history, so "the input was bounded" is a fact about this process rather than about
        # the configuration that was supposed to bound it; `constraint` is whether the task's own
        # distinctive text is still in there after compaction.
        rendered = "\n".join(str(getattr(part, "content", ""))
                             for message in messages
                             for part in (getattr(message, "parts", ()) or ()))
        with seen_log.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps({
                "pid": os.getpid(), "messages": len(messages), "chars": len(rendered),
                "constraint": bool(script.get("constraint")) and script["constraint"] in rendered,
                "marker": bool(script.get("original")) and script["original"] in rendered,
                # **Whether the runtime told the model its output was a fragment.** Read from the messages
                # the model was handed, because that is the only place the wording actually matters — the
                # record keeps the command's raw text, not the observation built from it.
                "cut_notice": ("was not kept whole" in rendered
                               or "could NOT be kept whole" in rendered),
            }) + "\n")
        if script.get("phases"):
            # A short, ordered set of commands chosen by what the history already shows — the same rule
            # as `staged`, applied to a handful of steps rather than a counted one.
            rendered = "\n".join(str(getattr(part, "content", ""))
                                 for message in messages
                                 for part in (getattr(message, "parts", ()) or ()))
            # **A default, not a bare `next`.** With every phase already in the history there is nothing
            # left to ask for, and a `StopIteration` raised out of a coroutine arrives as a
            # `RuntimeError` — measured, by the recovery failing with exactly that.
            command = next((item["command"] for item in script["phases"]
                            if item["until"] not in rendered), script["then"])
            return ModelResponse(parts=[ToolCallPart(tool_name="bash", args={"command": command})])
        if script.get("staged"):
            # **Driven by a monotonic stage signal**, read out of the history. The largest `STEP-n` the
            # model can see is how far the work has got — a number that compaction can shorten the
            # history around but cannot change, unlike a per-process turn counter, which restarts at one
            # in a new process and made the resumed attempt redo the first step.
            rendered = "\n".join(str(getattr(part, "content", ""))
                                 for message in messages
                                 for part in (getattr(message, "parts", ()) or ()))
            steps_seen = [int(hit) for hit in re.findall(r"STEP-(\d+)", rendered)]
            reached = max(steps_seen) if steps_seen else 0
            if reached >= int(script["until"]):
                command = script["then"]
            else:
                command = script["step"].format(n=reached + 1)
            return ModelResponse(parts=[ToolCallPart(tool_name="bash", args={"command": command})])
        if script.get("history_driven"):
            seen = any(script["marker"] in str(getattr(part, "content", ""))
                       for message in messages for part in (getattr(message, "parts", ()) or ()))
            command = script["then"] if seen else script["first"]
            return ModelResponse(parts=[ToolCallPart(tool_name="bash", args={"command": command})])
        index = turn["n"]
        turn["n"] += 1
        command = script["commands"][index] if index < len(script["commands"]) else None
        if command is None:
            return ModelResponse(parts=[ToolCallPart(
                tool_name="bash", args={"command": 'anchor-done --summary "finished"'})])
        return ModelResponse(parts=[ToolCallPart(tool_name="bash", args={"command": command})])

    # **The capability order decides which side of a write a hook lands on, and the two directions
    # are not the same.** Measured on this build:
    #
    #   `before_*` hooks run in registration order    → registered last, a barrier sees the framework's
    #                                                   `started` write already done (C2 needs this)
    #   `after_*`  hooks run in reverse order          → registered first, a barrier sees the framework's
    #                                                   terminal write already done (C5 needs this)
    #
    # `run_node` appends step persistence to whatever the caller passes, so "before persistence" is
    # "first in the caller's tuple" and "after persistence" is "last". Making this one order for every
    # window quietly moved two of them to the wrong side of a write — C2 started reporting the state
    # before `tool_call_started`, which reads as replayable when it is not.
    # The child places persistence itself so the barrier can sit on either side of it; the adapter
    # notices and does not add a second one.
    from pydantic_ai_harness import StepPersistence
    from anchor.node.recovery import open_store
    here = StepPersistence(store=open_store(control), agent_name=script["node"],
                           run_id=await _next_free_run_id(control, script["node"]))
    barrier_first = window in BEFORE_PERSISTENCE_WINDOWS
    # **The context capabilities, when the case is about them.** §57 asks for both things switched on
    # in the same execution, and a fixture that only claimed to would be measuring the plain path.
    context: tuple[Any, ...] = ()
    got: dict[str, Any] = {}
    if script.get("with_context"):
        from anchor.node.context import Budget as ContextBudget, Record, context_capabilities, remember
        record = Record(control / "kept", limit_bytes=int(script.get("record_bytes", 1000000)))
        got["record"] = record
        context = context_capabilities(
            ContextBudget(**script.get("budget", {})), record=record,
            # A summariser only when the case is about it: B2/B3/B5 and the compaction cases need the
            # summarising path, and a case without one is measuring the window fallback on purpose.
            summarizer=(_summariser(control, str(script.get("constraint", "")))
                        if script.get("summariser") else None))
        remember(context, script["task"], "", script.get("instructions", ""))

    outcome = await run_node(
        NodeRequest(execution_id=script["node"], task=script["task"], workspace=workspace,
                    max_requests=int(script.get("max_requests", 8)),
                    trace=control / "trace.jsonl", recovery=recover),
        model=FunctionModel(model),
        capabilities=((_barrier(window, script, got.get("record"), control), *context, here)
                      if barrier_first
                      else (*context, here, _barrier(window, script, got.get("record"), control))),
        recovery_store=control)
    _write_outcome(control, outcome)


# ── the parent ────────────────────────────────────────────────────────────────────────────────────


SURVIVOR_SECONDS = 8
SURVIVOR = ("printf 'started\\n' > started.log; sleep %d; "
            "printf 'SURVIVED\\n' >> survived.log; echo finished" % SURVIVOR_SECONDS)

#: The marker goes to **stdout** as well as to the log. It is the evidence a model reads back out of
#: the history to decide whether the work is done, and a marker that only reached a file left a
#: resumed process unable to tell that anything had happened — measured, by it running the command
#: nine times.
COUNTER = ("n=$(cat counter.txt 2>/dev/null || echo 0); n=$((n+1)); "
           "printf '%s\\n' \"$n\" > counter.tmp && mv counter.tmp counter.txt && "
           "printf 'EFFECT-%s\\n' \"$n\" >> effects.log && echo \"EFFECT-$n counted\"")

@dataclass
class Window:
    name: str
    commands: list[str]
    barrier: str
    expect: str


def windows() -> list[Window]:
    """The kill windows this package is asked about, with what each one should show afterwards."""
    return [
        Window("C1", [COUNTER, 'anchor-done --summary "done"'],
               "after_model_request, before the tool cycle",
               "counter=0; the tool never began, so the call can be made once"),
        Window("C2", [COUNTER, 'anchor-done --summary "done"'],
               "tool_call_started persisted, command not executed",
               "counter=0; started with no terminal state — uncertain, not replayed"),
        Window("C3", [COUNTER, 'anchor-done --summary "done"'],
               "side effect done, terminal record not written",
               "counter=1; uncertain; a second recovery must not make it 2"),
        Window("C4", [COUNTER, 'anchor-done --summary "done"'],
               "after the settled cycle, before the run ends",
               "counter=1; continuable from the settled snapshot, without redoing the work"),
        # **Two commands on purpose.** The first cycle settles and its snapshot is written; the second
        # command then completes and the kill lands before *its* snapshot. So a complete snapshot exists
        # — an older one — and the effect that just happened is not in it. That is the case the plan asks
        # for: an old snapshot must not be taken as covering a later side effect.
        Window("C5", [COUNTER, COUNTER, 'anchor-done --summary "done"'],
               "terminal effect record written, snapshot not yet",
               "counter=2; an older complete snapshot exists and does NOT cover it — uncertain"),
        Window("A6", [COUNTER, 'anchor-done --summary "done"'],
               "submission persisted, the graph has not finalised",
               "the node recovers with no model call; the graph's commit is reported missing or found"),
        Window("B2-before-compaction", [STEP, 'anchor-done --summary "done"'],
               "a kill before any compaction has happened",
               "the history is the un-compacted one and it is self-consistent"),
        Window("B2-after-compaction", [STEP, 'anchor-done --summary "done"'],
               "a kill once a compaction has happened, before its checkpoint",
               "nothing unsafe is offered as continuable; the chosen history hangs together"),
        Window("B2-checkpoint", [STEP, 'anchor-done --summary "done"'],
               "a kill after the compaction's own checkpoint",
               "the chosen history contains the summary and pairs every call with its return"),
        Window("B3-started", [STEP, 'anchor-done --summary "done"'],
               "after a compaction: a call started and not executed",
               "uncertain — the effect may or may not have happened"),
        Window("B3-effect", [STEP, 'anchor-done --summary "done"'],
               "after a compaction: the effect done and no terminal record",
               "uncertain — not replayed"),
        Window("B3-terminal", [STEP, 'anchor-done --summary "done"'],
               "after a compaction: the terminal record written and no snapshot",
               "uncertain — an older snapshot must not cover it"),
        Window("B4", [STEP, 'anchor-done --summary "done"'],
               "a large output saved, then read back by a later process",
               "the tail is there and writing is refused"),
        Window("B4-partial", [STEP, 'anchor-done --summary "done"'],
               "an output larger than the store, read back by a later process",
               "the cut is said out loud rather than left to be discovered"),
        Window("B1", [STEP, 'anchor-done --summary "done"'],
               "a real compaction, killed after it, continued by a new process",
               "the resumed input is bounded, the constraint survives, no step runs twice"),
        Window("A5", [COUNTER, 'anchor-done --summary "done"'],
               "real requests, killed and restarted until the allowance is spent",
               "the budget file agrees with the count the model itself kept"),
        Window("A4", [COUNTER, 'anchor-done --summary "done"'],
               "references that decode but do not belong, and files that are broken",
               "every one refused with a reason, and no command run"),
        Window("A3", [COUNTER, 'anchor-done --summary "done"'],
               "the real recovery entry, in sequence, over three kinds of reference",
               "uncertain does nothing; continuable continues; finished hands back its result"),
        Window("A1", [COUNTER, 'anchor-done --summary "done"'],
               "two processes, the second of which finishes the work",
               "counter stays 1 and the second process reaches a valid submission"),
        Window("C9", [SURVIVOR, 'anchor-done --summary "done"'],
               "side effect done, terminal record not written",
               "did the sandbox's own process outlive the host that started it?"),
    ]


def _counter(workspace: Path) -> int:
    path = workspace / "counter.txt"
    try:
        return int(path.read_text(encoding="utf-8").strip() or 0)
    except (OSError, ValueError):
        return 0


def _kill_at(control: Path, workspace: Path, script: dict, barrier: str,
             timeout: float, recover: str = "") -> tuple[bool | None, int | None, str, str]:
    """Spawn the child, wait for its barrier byte, and kill it there.

    Returns (was_killed, exit_code, what_the_child_said, traceback_text). The wait is a **read** on a
    pipe: no polling, no sleeping, and nothing about the machine's speed enters into when the kill lands.
    """
    read_fd, write_fd = os.pipe()
    environment = dict(os.environ, **{READY_FD_ENV: str(write_fd)})
    argv = [sys.executable, __file__, "--child", "--window", script["window"], "--control",
            str(control), "--workspace", str(workspace), "--script", json.dumps(script)]
    if recover:
        # How a budget case kills a **continuation**: the same handshake, but the child is handed the
        # reference first, so "killed and restarted, again and again" is a real loop.
        argv += ["--recover", recover]
    child = subprocess.Popen(
        argv,
        pass_fds=(write_fd,), env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True)
    os.close(write_fd)
    said = ""
    killed: bool | None = False
    try:
        ready, _, _ = select_with_timeout(read_fd, timeout)
        if ready:
            said = os.read(read_fd, 200).decode("utf-8", "replace").strip()
            # **A byte, or nothing.** End of file also makes the pipe readable — and reading that as a
            # signal would report a kill at a barrier the child never reached, which is how four windows
            # looked correct while proving nothing.
            if said:
                os.kill(child.pid, signal.SIGKILL)
                killed = True
            else:
                child.kill()
        else:
            child.kill()
    finally:
        os.close(read_fd)
    _, errors = child.communicate(timeout=30)
    return killed, child.returncode, said, errors or ""


def select_with_timeout(fd: int, timeout: float) -> tuple[bool, bool, bool]:
    import select
    ready, writable, exceptional = select.select([fd], [], [], timeout)
    return bool(ready), bool(writable), bool(exceptional)


async def _verdict(control: Path) -> tuple[str, str, list[list[str]], list[str], str, str]:
    from anchor.node.recovery import Budget, RecoveryRef, assess, open_store

    # **Derived from what actually happened**, not from a file the child wrote before it started: the
    # attempt that exists is the attempt the store recorded, and a reference built any other way could
    # name a run that was never made.
    store = open_store(control)
    runs = await store.list_runs()
    if not runs:
        return ("invalid", "no run was recorded at all", [], [], "", "")
    newest = sorted(runs, key=lambda item: item.started_at)[-1]
    ref = RecoveryRef(node=newest.agent_name, run=newest.run_id, store=str(control),
                      budget=Budget())
    verdict = await assess(store, ref)
    return (verdict.action, verdict.because, [list(item) for item in verdict.effects],
            list(verdict.events), verdict.snapshot,
            f"{verdict.budget.requests_used}/{verdict.budget.requests_allowed}")


def _wait_for_file(path: Path, timeout: float, interval: float = 0.02) -> bool:
    """Wait for a file to appear, up to a bound.

    The bound is what keeps this from hanging; the interval is not how the moment is chosen. The command
    writes the file and the parent reacts — a handshake, however it happens to be implemented.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return True
        time.sleep(interval)
    return path.exists()


def _processes_touching(root: Path) -> list[str]:
    """Anything still running that mentions this window's directories.

    §44 asks the test to clean up what it started, and a sandboxed command that outlived its host is
    exactly where a leak is possible — so whether one exists is measured rather than assumed.
    """
    try:
        listing = subprocess.run(["ps", "-eo", "pid,args"], capture_output=True, text=True,
                                 check=False).stdout
    except OSError:                                           # pragma: no cover - defensive
        return []
    return [line.strip() for line in listing.splitlines() if str(root) in line]


def run_a1(root: Path, timeout: float) -> Evidence:
    """**A1: two processes, and the second one finishes the work.**

    The first is killed at C4 — the counter command has completed, its snapshot is in the store, and the
    run has not ended. The second is a **new operating system process** that is handed the recovery
    reference and nothing else about the first: it has to decide what to do from the history the store
    gives it. Fetching that history is not recovery; reaching a valid submission without repeating the
    command is.
    """
    control = root / "A1" / "control"
    workspace = root / "A1" / "workspace"
    shutil.rmtree(root / "A1", ignore_errors=True)
    control.mkdir(parents=True, exist_ok=True)
    workspace.mkdir(parents=True, exist_ok=True)
    marker = "EFFECT-"
    script = {"window": "A1", "node": "node-A1", "run_id": f"run-A1-{os.getpid()}",
              "task": "count once, then finish", "history_driven": True, "marker": marker,
              "first": COUNTER, "then": 'anchor-done --summary "finished after resuming"'}
    started = time.monotonic()

    # ── the first process, killed once its command has settled ──
    killed, code, said, errors = _kill_at(control, workspace, dict(script, window="C4"),
                                          "after the settled cycle, before the run ends", timeout)
    after_first = _counter(workspace)

    # ── the second process, handed the reference ──
    # The run id is the adapter's to choose; read back what it actually used rather than assuming one.
    from anchor.node.recovery import RecoveryRef, open_store
    runs = asyncio.run(open_store(control).list_runs())
    assert runs, "the first process left no run in the store"
    first_run = sorted(runs, key=lambda item: item.started_at)[-1].run_id
    token = RecoveryRef(node=script["node"], run=first_run, store=str(control)).encode()
    before_outcomes = {item.name for item in control.glob("outcome-*.json")}
    second = subprocess.run(
        [sys.executable, __file__, "--child", "--window", "A1", "--control", str(control),
         "--workspace", str(workspace), "--script", json.dumps(script), "--recover", token],
        capture_output=True, text=True, timeout=max(timeout, 120))
    after_second = _counter(workspace)
    outcomes = [item for item in control.glob("outcome-*.json") if item.name not in before_outcomes]
    outcome = json.loads(outcomes[-1].read_text(encoding="utf-8")) if outcomes else {}

    evidence = Evidence(window="A1", control=str(control), workspace=str(workspace),
                        killed=killed, exit_code=second.returncode,
                        barrier=(said or "(the first process reported no barrier)"),
                        counter_before=0, counter_after=after_second, verdict="", because="",
                        seconds=time.monotonic() - started, traceback=(errors + second.stderr)[-2000:])
    evidence.verdict = outcome.get("status", "no-outcome")
    evidence.because = (
        f"first process killed at C4 with counter={after_first}; the second process, given only the "
        f"reference, ended {outcome.get('status')!r} with submission {outcome.get('submission')!r} "
        f"after {outcome.get('model_requests')} model request(s); counter is now {after_second}")
    evidence.budget = str(outcome.get("recovery", ""))[:60]
    return evidence


def run_c9(root: Path, timeout: float) -> Evidence:
    """Kill the host while its sandbox's shell is still running, and see which survived.

    This is the window where "the host process died" and "the command did not continue" are different
    claims, and the only way to tell them apart is to look afterwards.
    """
    control = root / "C9" / "control"
    workspace = root / "C9" / "workspace"
    shutil.rmtree(root / "C9", ignore_errors=True)
    control.mkdir(parents=True, exist_ok=True)
    workspace.mkdir(parents=True, exist_ok=True)
    script = {"window": "C9", "node": "node-C9", "run_id": f"run-C9-{os.getpid()}",
              "task": "the C9 window", "commands": [SURVIVOR, 'anchor-done --summary "done"']}
    started = time.monotonic()
    read_fd, write_fd = os.pipe()
    try:
        child = subprocess.Popen(
            [sys.executable, __file__, "--child", "--window", "C9", "--control", str(control),
             "--workspace", str(workspace), "--script", json.dumps(script)],
            pass_fds=(write_fd,), env=dict(os.environ, **{READY_FD_ENV: str(write_fd)}),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        os.close(write_fd)
        _wait_for_file(workspace / "started.log", timeout)
        os.kill(child.pid, signal.SIGKILL)
        child.communicate(timeout=30)
        exit_code = child.returncode
    finally:
        os.close(read_fd)

    survived = _wait_for_file(workspace / "survived.log", SURVIVOR_SECONDS + 5)
    left = _processes_touching(workspace)
    action, because, snapshot, budget = "", "", "", ""
    effects: list[list[str]] = []
    events: list[str] = []
    try:
        action, because, effects, events, snapshot, budget = asyncio.run(_verdict(control))
    except Exception as exc:                                  # noqa: BLE001 - reported, not raised
        action, because = "error", f"{type(exc).__name__}: {exc}"

    evidence = Evidence(window="C9", control=str(control), workspace=str(workspace),
                        killed=True, exit_code=exit_code,
                        barrier="the command announced itself from inside the sandbox",
                        counter_before=0, counter_after=0, verdict=action, because=because,
                        effects=effects, events=events, snapshot=snapshot, budget=budget,
                        seconds=time.monotonic() - started)
    evidence.note = (
        f"the shell {'DID continue' if survived else 'did NOT continue'} after its host was killed; "
        f"{len(left)} process(es) mentioning this window still running"
        + (": " + "; ".join(left[:3]) if left else ""))
    for line in left:                                         # cleanup; the leak is in the evidence
        try:
            os.kill(int(line.split(None, 1)[0]), signal.SIGKILL)
        except (ProcessLookupError, PermissionError):         # pragma: no cover - already gone
            pass
    return evidence


def run_window(window: Window, root: Path, timeout: float) -> Evidence:
    """One window, start to finish, with its own directories and its own cleanup."""
    if window.name == "A1":
        return run_a1(root, timeout)
    if window.name == "A3":
        return run_a3(root, timeout)
    if window.name == "A4":
        return run_a4(root, timeout)
    if window.name.startswith(("B2-", "B3-")):
        return run_compaction_window(root, timeout, window.name)
    if window.name in ("B4", "B4-partial"):
        return run_b4(root, timeout, window.name)
    if window.name == "B1":
        return run_b1(root, timeout)
    if window.name == "A5":
        return run_a5(root, timeout)
    if window.name == "A6":
        return run_a6(root, timeout)
    if window.name == "C9":
        return run_c9(root, timeout)
    # **Isolated, every time.** §42 asks for an isolated temporary workspace, and this was learned the
    # hard way: leaving a previous attempt's store behind makes the framework refuse to reuse the same
    # explicit run id, the child exits before its barrier, and the window reports a kill it never made.
    control = root / window.name / "control"
    workspace = root / window.name / "workspace"
    shutil.rmtree(root / window.name, ignore_errors=True)
    control.mkdir(parents=True, exist_ok=True)
    workspace.mkdir(parents=True, exist_ok=True)
    script = {"window": window.name, "node": f"node-{window.name}",
              "run_id": f"run-{window.name}-{os.getpid()}", "task": f"the {window.name} window",
              "commands": window.commands}
    before = _counter(workspace)
    started = time.monotonic()
    killed, code, said, errors = _kill_at(control, workspace, script, window.barrier, timeout)
    elapsed = time.monotonic() - started
    after = _counter(workspace)

    evidence = Evidence(window=window.name, control=str(control), workspace=str(workspace),
                        killed=killed, exit_code=code, barrier=said,
                        counter_before=before, counter_after=after, verdict="", because="",
                        seconds=elapsed, traceback=errors)
    try:
        action, because, effects, events, snapshot, budget = asyncio.run(_verdict(control))
        evidence.verdict, evidence.because = action, because
        evidence.effects, evidence.events = effects, events
        evidence.snapshot, evidence.budget = snapshot, budget
    except Exception as exc:                                  # noqa: BLE001 - reported, not raised
        evidence.verdict, evidence.because = "error", f"{type(exc).__name__}: {exc}"
        evidence.traceback = (evidence.traceback + "\n" + traceback.format_exc()).strip()
    if killed is False:
        evidence.note = "the child never reached its barrier — this window proved nothing"
    return evidence


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--child", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--window", default="", help=argparse.SUPPRESS)
    parser.add_argument("--control", default="", help=argparse.SUPPRESS)
    parser.add_argument("--workspace", default="", help=argparse.SUPPRESS)
    parser.add_argument("--script", default="", help=argparse.SUPPRESS)
    parser.add_argument("--recover", default="", help=argparse.SUPPRESS)
    parser.add_argument("--only", default="", help="comma-separated windows, default all")
    parser.add_argument("--root", default=".local/recovery-windows")
    parser.add_argument("--timeout", type=float, default=120.0,
                       help="how long to wait for a child to reach its barrier")
    parser.add_argument("--json", default="", help="write the evidence here as JSON")
    args = parser.parse_args()

    if args.child:
        child_script = json.loads(args.script)
        if child_script.get("through_graph"):
            _run_graph_child(args.window, Path(args.control), Path(args.workspace), child_script)
        else:
            asyncio.run(_run_child(args.window, Path(args.control), Path(args.workspace),
                                   child_script, recover=args.recover))
        return 0                                             # pragma: no cover - killed before this

    every = {item.name for item in windows()} | {"C6", "C7", "C8"}
    asked = {name.strip() for name in args.only.split(",")} if args.only else every
    chosen = [item for item in windows() if item.name in asked]
    without_killing = sorted(asked & {"C6", "C7", "C8"})
    root = Path(args.root)
    root.mkdir(parents=True, exist_ok=True)
    results: list[Evidence] = []
    try:
        for window in chosen:
            print(f"running {window.name}: {window.expect}", flush=True)
            results.append(run_window(window, root, args.timeout))
        if without_killing:
            results.extend(run_windows_without_killing(root, without_killing))
    finally:
        # No child outlives this. The killed ones are gone; a child that never reached its barrier was
        # killed by the wait itself, and anything still holding the directory is reported rather than
        # left running.
        _reap(root)
    for evidence in results:
        for line in evidence.lines():
            print(line)
        print()
    if args.json:
        Path(args.json).write_text(
            json.dumps([asdict(item) for item in results], indent=2, ensure_ascii=False),
            encoding="utf-8")
        print(f"wrote {args.json}")
    missed = [item.window for item in results if item.killed is False]
    if missed:
        print(f"these windows never reached their barrier: {', '.join(missed)}")
        return 1
    return 0


async def _recovered(control: Path) -> tuple[str, int]:
    """What a second recovery would do, and how many times the effect has run.

    The number is read from the workspace the command wrote to, so "it did not run again" is a fact
    about the disk rather than about the code that decided not to run it.
    """
    from anchor.node.recovery import assess, continued_messages, open_store
    ref = await _newest_ref(control)
    if ref is None:
        return "no-run", 0
    store = open_store(control)
    verdict = await assess(store, ref)
    if verdict.action == "continuable":
        # Fetching the history is the act a caller would take; it must not run anything by itself.
        await continued_messages(store, ref)
    return verdict.action, len(verdict.effects)


def _ask_once(control: Path, token: str, script: dict) -> dict:
    """One **real recovery entry**, in a new process, and what it decided.

    Not `assess` called twice: this runs the node's own entry point with the token, so what is measured
    is what a caller would get — including the model and tool calls it did or did not make.
    """
    work = control.parent / "again"
    work.mkdir(parents=True, exist_ok=True)
    before = {item.name for item in control.glob("outcome-*.json")}
    subprocess.run(
        [sys.executable, __file__, "--child", "--window", "A3", "--control", str(control),
         "--workspace", str(work), "--script", json.dumps(script), "--recover", token],
        capture_output=True, text=True, timeout=180, check=False)
    new = [item for item in control.glob("outcome-*.json") if item.name not in before]
    return json.loads(new[-1].read_text(encoding="utf-8")) if new else {}


def run_a3(root: Path, timeout: float) -> Evidence:
    """**A3: the real recovery entry, in sequence, over three kinds of reference.**

    An uncertain reference must do nothing; a continuable one must continue; a finished one must hand
    back the result it already has. In all three the original run's history stays as it was, and nothing
    is confirmed twice.
    """
    from anchor.node.recovery import RecoveryRef, open_store
    script = {"window": "A3", "node": "node-A3", "task": "count, then finish",
              "history_driven": True, "marker": "EFFECT-", "first": COUNTER,
              "then": 'anchor-done --summary "resumed to a submission"'}
    started = time.monotonic()
    steps: list[str] = []

    def control_for(name: str) -> Path:
        make = root / "A3" / name
        shutil.rmtree(make, ignore_errors=True)
        (make / "workspace").mkdir(parents=True, exist_ok=True)
        (make / "control").mkdir(parents=True, exist_ok=True)
        return make / "control"

    # ── an uncertain reference: a call started and never finished ──
    uncertain_control = control_for("uncertain")
    _kill_at(uncertain_control, uncertain_control.parent / "workspace", dict(script, window="C3"),
             "side effect done, terminal record not written", timeout)
    runs = asyncio.run(open_store(uncertain_control).list_runs())
    ref = RecoveryRef(node=script["node"], run=runs[-1].run_id, store=str(uncertain_control))
    out = _ask_once(uncertain_control, ref.encode(), script)
    steps.append(f"uncertain -> {out.get('status')} with {out.get('model_requests')} model request(s)")
    assert out.get("status") == "uncertain", steps
    assert out.get("model_requests") == 0, steps

    # ── a continuable reference: settled, with a snapshot that covers it ──
    live_control = control_for("continuable")
    _kill_at(live_control, live_control.parent / "workspace", dict(script, window="C4"),
             "after the settled cycle, before the run ends", timeout)
    runs = asyncio.run(open_store(live_control).list_runs())
    ref = RecoveryRef(node=script["node"], run=runs[-1].run_id, store=str(live_control))
    first = _ask_once(live_control, ref.encode(), script)
    steps.append(f"continuable -> {first.get('status')} submitting {first.get('submission')!r}")
    assert first.get("status") == "completed", steps

    # ── and now the same reference again: it has submitted, so nothing may run ──
    finished_ref = first.get("recovery") or ref.encode()
    second = _ask_once(live_control, finished_ref, script)
    steps.append(f"finished -> {second.get('status')} with {second.get('model_requests')} model "
                 f"request(s), submission {second.get('submission')!r}")
    assert second.get("model_requests") == 0, f"a finished run was asked again: {steps}"
    assert second.get("submission") == first.get("submission"),         f"the submission was not carried over unchanged: {steps}"

    evidence = Evidence(window="A3", control=str(live_control),
                        workspace=str(live_control.parent / "workspace"),
                        killed=None, exit_code=None, barrier="(sequenced, no kill)",
                        counter_before=0, counter_after=_counter(live_control.parent / "workspace"),
                        verdict="no-repeat" if second.get("model_requests") == 0 else "REPEATED",
                        because="; ".join(steps), seconds=time.monotonic() - started)
    return evidence


def run_a6(root: Path, timeout: float) -> Evidence:
    """**A6/R5: the candidate Node in a real graph, and where the graph's half cannot be reached.**

    A three-node graph — agent, op, agent — run by the runtime's own scheduler, with only the agent step
    routed through `run_node`. The kill is aimed at the instant a submission has been accepted and the
    graph has recorded nothing.

    **That instant is not on any hook, and this is the measurement.** A node that submits makes no further
    model request — the pass ends — so the last agent-side hook is *before* the submission and the next one
    belongs to the *next* node, by which time the graph has already recorded the pass it was supposed to be
    interrupted in the middle of. The barrier therefore fires late, and says so; the node's own recovery
    still works, which is the half that can be picked up.

    The earlier attempt ran the mini default path, which has no step store at all, so "no run was recorded"
    said nothing about the candidate architecture. This one has the candidate: a run in the store, a
    completed fact, and a result handed back with no model call.
    """
    from anchor.node.recovery import RecoveryRef, assess, open_store

    started = time.monotonic()
    control = root / "A6" / "control"
    workspace = root / "A6" / "workspace"
    shutil.rmtree(root / "A6", ignore_errors=True)
    control.mkdir(parents=True, exist_ok=True)
    workspace.mkdir(parents=True, exist_ok=True)
    (workspace / "graph.json").write_text(json.dumps({
        "entry": "write",
        "objective": "submit in the first node, then be interrupted before the graph records it",
        "agents": {"w": {"model": "models.deterministic", "writes": ["note.md"]},
                   "f": {"model": "models.deterministic"}},
        "ops": {"count": {"run": "wc -c < /in/write/note.md > size.txt", "reads": ["note.md"],
                          "writes": ["size.txt"]}},
        "nodes": [{"id": "write", "agent": "w"}, {"id": "count", "op": "count"},
                  {"id": "finish", "agent": "f"}],
        "edges": [{"from": "write", "to": "count"}, {"from": "count", "to": "finish"}],
    }), encoding="utf-8")
    (control / "runtime.json").write_text(json.dumps({"models": [], "agents": [], "tools": []}),
                                          encoding="utf-8")
    script = {"window": "A6", "node": "write", "through_graph": True, "max_requests": 8,
              "task": "write a note and submit", "marker": "EFFECT-",
              "first": "printf 'four\\n' > note.md; echo EFFECT-1",
              "then": 'anchor-done --summary "wrote it"'}

    killed, code, said, errors = _kill_at(control, workspace, script,
                                          "submission persisted, the graph has not finalised", timeout)
    store = open_store(control)
    runs = asyncio.run(store.list_runs())
    # **The node the case is about**, not whichever run happens to be newest: the graph carries on into
    # the next node, so the newest run belongs to that one and reading its completion finds nothing.
    ours = [item for item in runs if item.agent_name == script["node"]]
    verdict_action, submission, requests = "", "", -1
    if ours:
        this = sorted(ours, key=lambda item: item.started_at)[-1]
        ref = RecoveryRef(node=this.agent_name, run=this.run_id, store=str(control))
        verdict_action = asyncio.run(assess(store, ref)).action
        # **Not through the graph again** — this is the node's own entry point with the reference, which
        # is the half A6 says can be picked up.
        out = _ask_once(control, ref.encode(),
                        dict(script, window="A6-recovered", through_graph=False))
        submission, requests = str(out.get("submission", "")), int(out.get("model_requests", -1))

    # What the graph had recorded by the time the kill landed.
    graph_state = {}
    for record in workspace.rglob("run.json"):
        with contextlib.suppress(json.JSONDecodeError):
            graph_state = json.loads(record.read_text(encoding="utf-8"))
    recorded = sorted(graph_state.get("passes", {}))

    evidence = Evidence(window="A6", control=str(control), workspace=str(workspace),
                        killed=bool(killed), exit_code=code, barrier=said or "(no barrier)",
                        counter_before=0, counter_after=len(ours), verdict="", because="",
                        seconds=time.monotonic() - started, traceback=errors[-1200:])
    # **`blocked` because the window itself is unreachable**, not because a commit is missing: the kill
    # could only land after the graph had already recorded the node it was aimed at.
    in_window = script["node"] not in recorded
    evidence.verdict = "in-window" if in_window else "blocked"
    evidence.because = (
        f"the **candidate** node left {len(ours)} run(s) in the step store; assessing one says "
        f"{verdict_action!r} and handing the reference back returns submission {submission!r} with "
        f"{requests} model request(s); by the time the kill landed the graph had recorded {recorded} "
        f"(status {graph_state.get('status')!r})")
    evidence.note = (
        "the graph's half could not be interrupted where the case wants it: a node that submits makes no "
        "further model request, so the last agent-side hook is *before* the submission and the next one "
        "belongs to the next node — the gap between `agent.run(task=...)` returning and "
        "`_record(state, graph, run_dir, decided, result, settle)` in src/anchor/simple/run.py has no hook "
        "at all. Node-level recovery works; the graph-level coupling is BLOCKED for want of that seam."
        if not in_window else "the kill landed inside the intended window")
    return evidence


def _commits(workspace: Path) -> list[str]:
    """The commits the graph has made in a node's workspace, oldest first."""
    nodes = sorted(item for item in workspace.rglob("only") if item.is_dir())
    out: list[str] = []
    for node in nodes:
        if not (node / ".git").is_dir():
            continue
        done = subprocess.run(["git", "-C", str(node), "log", "--format=%s"],
                              capture_output=True, text=True, check=False)
        out.extend(line for line in done.stdout.splitlines() if line.strip())
    return out


#: One unit of work that announces how far it got **and records every invocation**, so a step that runs
#: twice is visible as a repeated number rather than hidden by a counter that looks correct either way.
STEP = ("n=$(cat n.txt 2>/dev/null || echo 0); n=$((n+1)); printf '%s\\n' \"$n\" > n.txt; "
        "printf 'STEP-%s\\n' \"$n\" >> steps.log; "
        "for i in $(seq 1 40); do printf 'filler-for-step-%s-line-%s\\n' \"$n\" \"$i\"; done; "
        "echo \"STEP-$n counted\"")


def run_b1(root: Path, timeout: float) -> Evidence:
    """**B1: a real compaction, killed after it, continued by a new process.**

    The comparison that matters is between what happened *before* the kill and what the resumed process
    was sent. The first version of this case counted turns inside the process, so the resumed attempt
    restarted at step one and redid work; the model now reads the largest `STEP-n` out of its own history,
    which compaction can shorten around but not change, and each invocation appends its number to a log so
    a repeated step shows up as a duplicate.
    """
    from anchor.node.recovery import RecoveryRef, open_store

    started = time.monotonic()
    control = root / "B1" / "control"
    workspace = root / "B1" / "workspace"
    shutil.rmtree(root / "B1", ignore_errors=True)
    control.mkdir(parents=True, exist_ok=True)
    workspace.mkdir(parents=True, exist_ok=True)
    constraint = "the constraint that must survive compaction"
    script = {"window": "B1", "node": "node-B1", "task": f"do the work. {constraint}. finish",
              "instructions": f"{constraint}: never drop a step", "with_context": True,
              "budget": {"window": 4000, "output_reserve": 400, "input_target": 1500,
                         "keep_messages": 2},
              # Enough for the work twice over, so the continuation finishes rather than running out
              # half way and making the assertion about a budget that was never the subject here.
              "max_requests": 40, "record_bytes": 60000, "summariser": True,
              "staged": True, "until": 10, "step": STEP,
              "constraint": constraint,
              "then": 'anchor-done --summary "all steps done"'}

    killed, code, said, errors = _kill_at(control, workspace, script,
                                          "compaction done, a later checkpoint is on disk", timeout)
    steps_before = _step_numbers(workspace)
    compactions = _compactions(control)
    inputs_before = _model_inputs(control)

    # **A new process**, with the same capabilities attached and the reference, continuing the work.
    runs = asyncio.run(open_store(control).list_runs())
    if not runs:
        return _b1_evidence(root, started, killed, said, errors, [], [], [], [],
                            "no run was recorded, so there is nothing to continue")
    newest = sorted(runs, key=lambda item: item.started_at)[-1]
    token = RecoveryRef(node=newest.agent_name, run=newest.run_id, store=str(control)).encode()
    outcome = _ask_once(control, token, dict(script, window="B1-resumed", kill_after_models=99))
    steps_after = _step_numbers(workspace)
    inputs_after = _model_inputs(control)

    evidence = _b1_evidence(root, started, killed, said, errors, steps_before, compactions,
                            inputs_before, inputs_after,
                            f"resumed as {outcome.get('status')!r} with "
                            f"{outcome.get('model_requests')} request(s), submission "
                            f"{outcome.get('submission')!r}")
    evidence.control = str(control)
    evidence.workspace = str(workspace)
    evidence.counter_after = len(steps_after)
    return evidence


#: A unique tail so "the whole output is really there" is a fact about this run and not about a shape.
TAIL_MARKER = "END-OF-THE-WHOLE-OUTPUT-7f3a"


def run_b4(root: Path, timeout: float, name: str = "B4") -> Evidence:
    """**B4: the saved output is readable, is read-only, and says when it is not whole.**

    A large output with a unique tail is produced, the process is killed, and a **new process** — through
    the node's own entry point, with a real sandbox — pages it back with `cat`. Three things have to hold
    in that process: the tail is really there, the node cannot write to the store, and a command whose
    output was cut says so.
    """
    from anchor.node.recovery import RecoveryRef, open_store

    started = time.monotonic()
    control = root / name / "control"
    workspace = root / name / "workspace"
    shutil.rmtree(root / name, ignore_errors=True)
    control.mkdir(parents=True, exist_ok=True)
    workspace.mkdir(parents=True, exist_ok=True)
    # 300 KB with a marker at the very end: far past the preview, so it is the *store* being read here and
    # not the observation the model was shown.
    partial = name.endswith("partial")
    big = (f"head -c 300000 /dev/zero | tr '\\0' 'x'; printf '{TAIL_MARKER}\\n'")
    script = {"window": "C4", "node": f"node-{name}", "task": "produce a large output, then finish",
              "with_context": True,
              # **A store too small to hold it**, for the case where the answer is "this is not whole".
              "record_bytes": 20_000 if partial else 4_000_000,
              "budget": {"window": 200000, "output_reserve": 1000, "input_target": 150000},
              "commands": [big, 'anchor-done --summary "produced"']}

    killed, code, said, errors = _kill_at(control, workspace, script,
                                          "after 2 settled request(s), before the run ends", timeout)
    # What the first process kept, and what it told the model it could not keep whole.
    kept = sorted((control / "kept").rglob("*.txt"))
    sizes = {item.name: item.stat().st_size for item in kept}
    # **The append-only record, not the trace.** A trace belongs to one attempt and the resumed process
    # writes its own over it; the record is written as things happen and never rewritten, so what the
    # first process told the model is still there afterwards.
    told = ""
    for item in sorted((control / "kept").rglob("*.jsonl")):
        told += item.read_text(encoding="utf-8", errors="replace")

    runs = asyncio.run(open_store(control).list_runs())
    ours = [item for item in runs if item.agent_name == script["node"]]
    out: dict = {}
    if ours:
        # **The run this case created**, which is the oldest: the store was cleared at the start, so the
        # first one is the attempt that was killed. Taking the newest would read whatever a later run left
        # behind — which is how a stray debugging attempt turned this case into 'uncertain' once.
        this = sorted(ours, key=lambda item: item.started_at)[0]
        ref = RecoveryRef(node=this.agent_name, run=this.run_id, store=str(control))
        # **The recovered process does the reading**, in a real sandbox, with its own commands.
        out = _ask_once(control, ref.encode(), {
            "window": "B4-resumed", "node": script["node"], "task": "read back what was produced",
            "then": 'anchor-done --summary "read the whole output back"',
            "with_context": True, "record_bytes": 4_000_000,
            "budget": {"window": 200000, "output_reserve": 1000, "input_target": 150000},
            "phases": [
                {"until": TAIL_MARKER, "command": "tail -c 80 /kept/*.txt 2>&1"},
                # Named after a file that exists when the store was big enough, and otherwise after one
                # that does not: either way the mount is read-only and the write is refused.
                {"until": "READONLY",
                 "command": f"echo tampered > /kept/{kept[0].name if kept else 'anything.txt'} 2>&1; "
                            f"echo READONLY-$?"},
                {"until": "DONE", "command": "ls /kept/../ | head -5; echo DONE"},
            ]})

    # **Read out of the append-only record**, not out of the outcome: what the recovered process saw is
    # in its command results, and the record is where every attempt's results are written and never
    # rewritten. The outcome only carries the submission.
    record_text = "\n".join(
        item.read_text(encoding="utf-8", errors="replace")
        for item in (control / "kept").rglob("*.jsonl")) if (control / "kept").exists() else ""
    everything = record_text + json.dumps(out, ensure_ascii=False)
    tail_seen = TAIL_MARKER in everything
    # A write into the read-only mount fails with a non-zero status and the sandbox's own message.
    readonly = "READONLY-1" in everything or "READONLY-2" in everything or \
        "Read-only file system" in everything
    evidence = Evidence(window=name, control=str(control), workspace=str(workspace),
                        killed=bool(killed), exit_code=code, barrier=said or "(no barrier)",
                        counter_before=0, counter_after=len(kept), verdict="", because="",
                        seconds=time.monotonic() - started, traceback=errors[-1200:])
    # Two wordings, two layers: the sandbox says "could NOT be kept whole" when it refused to keep a
    # stream, and the observation says "was not kept whole" when the preview it hands the model is not the
    # whole output. Either is the runtime telling the node that what it is reading is a fragment.
    # **From the model's own view of what it was handed**, which is where a missing notice does its
    # damage — a node shown a fragment without being told reads it as the whole result.
    inputs_seen = _model_inputs(control)
    cut_and_said = any(item.get("cut_notice") for item in inputs_seen)
    if partial:
        # **The small-store case**: the answer is not "the tail is here" but "this is not whole", said
        # out loud rather than left for the model to discover by finding the end missing.
        evidence.verdict = "cut-and-said" if cut_and_said else "BAD(cut-but-not-said)"
    else:
        evidence.verdict = ("readable-and-read-only" if tail_seen and readonly
                            else f"BAD(tail={tail_seen} read_only={readonly})")
    evidence.because = (
        f"kept {len(kept)} file(s) {sizes}; the runtime told the model its output was a fragment: "
        f"{any(item.get('cut_notice') for item in inputs_seen)}; "
        f"the resumed process read the tail back: {tail_seen}; writing to the store was refused: "
        f"{readonly}; it ended {out.get('status')!r} after {out.get('model_requests')} request(s)")
    evidence.note = ("the store is readable from inside the sandbox, write-refused, and the whole output "
                     "is there for a later process")
    return evidence


def _paired(snapshot: Any) -> bool:
    """Whether a history is self-consistent: every tool call has a return, and every return a call.

    **This is what B2 is about.** Compaction rewrites the history, and a checkpoint taken on one side of a
    summary can be spliced onto the other side — a summary of messages that the chosen version still
    contains, or a tool return whose call was dropped. The framework's own `is_provider_valid` is the
    first line of defence; this is the invariant read directly off the messages the store holds.
    """
    calls: set[str] = set()
    returns: set[str] = set()
    for message in getattr(snapshot, "messages", ()) or ():
        for part in getattr(message, "parts", ()) or ():
            kind = getattr(part, "part_kind", "")
            call_id = getattr(part, "tool_call_id", None)
            if not call_id:
                continue
            if kind == "tool-call":
                calls.add(call_id)
            elif kind == "tool-return":
                returns.add(call_id)
    return calls == returns


def run_compaction_window(root: Path, timeout: float, name: str) -> Evidence:
    """**B2 and B3, as one shape**: kill at a named boundary that is only armed after a real compaction.

    The distinction each case turns on is which side of the compaction's own checkpoint the kill lands on,
    and whether the state it leaves can be told apart from one that is safe to continue. So the evidence
    is: how many compactions had happened, what the chosen history's version is, whether that history is
    self-consistent, and what `assess` says about it.
    """
    from anchor.node.recovery import RecoveryRef, assess, open_store

    started = time.monotonic()
    control = root / name / "control"
    workspace = root / name / "workspace"
    shutil.rmtree(root / name, ignore_errors=True)
    control.mkdir(parents=True, exist_ok=True)
    workspace.mkdir(parents=True, exist_ok=True)
    constraint = "the constraint that a summary has to keep"
    script = {"window": name, "node": f"node-{name}", "task": f"do the work. {constraint}",
              "instructions": f"{constraint}: never drop a step", "with_context": True,
              "summariser": True, "constraint": constraint,
              "budget": {"window": 4000, "output_reserve": 400, "input_target": 1500,
                         "keep_messages": 2},
              "max_requests": 40, "record_bytes": 60000,
              "staged": True, "until": 40, "step": STEP,
              "then": 'anchor-done --summary "done"'}

    killed, code, said, errors = _kill_at(control, workspace, script,
                                          f"{name} barrier", timeout)
    compactions = _compactions(control)
    runs = asyncio.run(open_store(control).list_runs())
    ours = [item for item in runs if item.agent_name == script["node"]]
    action, consistent, newest_state, summary_in = "", None, "", False
    if ours:
        this = sorted(ours, key=lambda item: item.started_at)[-1]
        store = open_store(control)
        action = asyncio.run(assess(store, RecoveryRef(node=this.agent_name, run=this.run_id,
                                                       store=str(control)))).action
        snapshot = asyncio.run(store.latest_snapshot(run_id=this.run_id))
        if snapshot is not None:
            consistent = _paired(snapshot)
            newest_state = str(getattr(snapshot, "state", ""))
            rendered = "\n".join(str(getattr(part, "content", ""))
                                 for message in getattr(snapshot, "messages", ()) or ()
                                 for part in (getattr(message, "parts", ()) or ()))
            summary_in = "SUMMARY." in rendered

    evidence = Evidence(window=name, control=str(control), workspace=str(workspace),
                        killed=bool(killed), exit_code=code, barrier=said or "(no barrier)",
                        counter_before=0, counter_after=len(_step_numbers(workspace)),
                        verdict="", because="", seconds=time.monotonic() - started,
                        traceback=errors[-1200:])
    # **Safe means one of two things**: the state is honestly unknown, or there is a version whose own
    # history hangs together. What is never acceptable is reporting a continuable version that is not
    # self-consistent, or one that carries a summary of messages it does not contain.
    unsafe = consistent is False or (action == "continuable" and not consistent)
    evidence.verdict = ("unsafe" if unsafe else
                        ("inconsistent-history" if consistent is False else "consistent"))
    evidence.because = (
        f"{len(compactions)} compaction(s) by "
        f"{sorted({item.get('strategy', '?') for item in compactions})}; "
        f"chosen history state {newest_state!r}; tool calls and returns paired: {consistent}; "
        f"assessment says {action!r}; the summary text is {'in' if summary_in else 'not in'} the snapshot; "
        f"steps recorded {_step_numbers(workspace)}")
    evidence.note = (
        # **The snapshot is the settled history; the compaction is derived per request.** That is why a
        # summary can never be spliced onto a history it was not made from: the two never meet in the
        # store. The chosen history hangs together because it was never rewritten, and the compaction is
        # recomputed on the request that follows — which is also why a resumed attempt stays bounded.
        "the kill landed after a real compaction; the store's history is the settled one, it hangs "
        "together, and the summary is applied per request rather than stored, so nothing is spliced"
        if not unsafe else "the chosen history does not hang together")
    return evidence


def _step_numbers(workspace: Path) -> list[int]:
    """Every invocation of the step command, in order. A repeat is a duplicate number."""
    logs = sorted(workspace.rglob("steps.log"))
    out: list[int] = []
    for log in logs:
        for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.strip().startswith("STEP-"):
                with contextlib.suppress(ValueError):
                    out.append(int(line.strip().split("-", 1)[1]))
    return out


def _compactions(control: Path) -> list[dict]:
    """Every compaction the record shows, with the strategy that actually did it.

    **Any strategy counts as a real compaction** — what B1 asserts is that the history the model was sent
    had been reduced before the kill, and by which mechanism is reported rather than assumed. It is worth
    reading: in this configuration only `SlidingWindowCompaction` fires, because `_compact_once` stops at
    the first strategy that reduces and sliding is first in the list.
    """
    out: list[dict] = []
    for item in (control / "kept").rglob("*.jsonl"):
        if item.name != "record.jsonl":
            continue
        for line in item.read_text(encoding="utf-8", errors="replace").splitlines():
            with contextlib.suppress(json.JSONDecodeError):
                payload = json.loads(line)
                if payload.get("kind") == "compaction":
                    out.append(payload)
    for item in (control / "kept").rglob("*.jsonl"):
        if item.name == "record.jsonl":
            continue
        for line in item.read_text(encoding="utf-8", errors="replace").splitlines():
            with contextlib.suppress(json.JSONDecodeError):
                payload = json.loads(line)
                if payload.get("kind") == "compaction":
                    out.append(payload)
    return out


def _model_inputs(control: Path) -> list[dict]:
    """What the model was actually handed, call by call, written by the model itself."""
    path = control / "model-input.jsonl"
    if not path.exists():
        return []
    out = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        with contextlib.suppress(json.JSONDecodeError):
            out.append(json.loads(line))
    return out


def _b1_evidence(root: Path, started: float, killed: Any, said: str, errors: str,
                 steps_before: list[int], compactions: list[dict], inputs_before: list[dict],
                 inputs_after: list[dict], note: str) -> Evidence:
    evidence = Evidence(window="B1", control=str(root / "B1" / "control"),
                        workspace=str(root / "B1" / "workspace"), killed=bool(killed),
                        exit_code=None, barrier=said or "(no barrier)", counter_before=0,
                        counter_after=len(steps_before), verdict="", because="",
                        seconds=time.monotonic() - started, traceback=errors[-1200:])
    biggest = max((item.get("chars", 0) for item in inputs_after), default=0)
    constrained = [item for item in inputs_after if item.get("constraint")]
    strategies = sorted({item.get("strategy", "?") for item in compactions})
    evidence.verdict = (
        "compacted-then-resumed" if compactions and killed and not _duplicated(steps_before)
        else f"BAD(compactions={len(compactions)} killed={killed} "
             f"duplicates={_duplicated(steps_before)})")
    evidence.because = (
        f"{len(compactions)} compaction(s) before the kill by {strategies}; "
        f"steps before the kill {steps_before}; "
        f"model input sizes before {[item.get('chars') for item in inputs_before]}; "
        f"after the continuation {[item.get('chars') for item in inputs_after]} (largest {biggest}); "
        f"the constraint was present in {len(constrained)}/{len(inputs_after)} resumed call(s); {note}")
    evidence.note = (
        "steps recorded exactly once each" if not _duplicated(steps_before)
        else f"these steps ran more than once: {_duplicated(steps_before)}")
    return evidence


def _duplicated(numbers: list[int]) -> list[int]:
    seen, twice = set(), []
    for number in numbers:
        if number in seen and number not in twice:
            twice.append(number)
        seen.add(number)
    return twice


def run_a5(root: Path, timeout: float) -> Evidence:
    """**A5: real requests, killed and restarted, until the allowance is spent.**

    The assertion is not a number this test chose. The model counts its own invocations where requests
    actually arrive, appending to a file so the count survives the process, and the budget file has to
    agree with it — and then the last attempt has to make **no request at all**, which is the half the
    first version of this case never reached: it stopped as soon as an attempt completed, so a spent
    allowance was never exercised, and an allowance of 8/8 came back 9/8.
    """
    from anchor.node.recovery import Budget, RecoveryRef, load_budget, open_store, save_budget

    started = time.monotonic()
    control = root / "A5" / "control"
    workspace = root / "A5" / "workspace"
    shutil.rmtree(root / "A5", ignore_errors=True)
    control.mkdir(parents=True, exist_ok=True)
    workspace.mkdir(parents=True, exist_ok=True)
    allowed = 6
    save_budget(control, Budget(requests_used=0, requests_allowed=allowed))
    # **One request per attempt, kill, repeat.** The barrier has to be reachable in *every* attempt, and
    # a continuation whose history already shows the work submits on its first request — so a window that
    # waits for two settled cycles is only reachable in the first attempt. What A5 is about is the
    # accounting across restarts, and a kill after the first request is a real interruption of a real
    # request.
    script = {"window": "C4", "kill_after_models": 1, "node": "node-A5",
              "task": "finish eventually", "history_driven": True, "marker": "EFFECT-",
              "first": COUNTER, "then": 'anchor-done --summary "finished"'}
    log = control / "model-calls.log"
    steps: list[str] = []
    token = ""
    rounds = 0

    def counted() -> int:
        return len(log.read_text(encoding="utf-8").splitlines()) if log.exists() else 0

    def reference() -> str:
        runs = asyncio.run(open_store(control).list_runs())
        newest = sorted(runs, key=lambda item: item.started_at)[-1]
        return RecoveryRef(node=newest.agent_name, run=newest.run_id,
                           store=str(control)).encode()

    # **Kill, restart, kill again** — until the allowance runs out. Bounded, so a bug in the accounting
    # shows up as a round limit rather than as a test that never finishes.
    for _ in range(allowed + 2):
        killed, code, said, errors = _kill_at(control, workspace, script,
                                              "after 2 settled request(s), before the run ends",
                                              timeout, recover=token)
        rounds += 1
        budget = load_budget(control)
        steps.append(f"round {rounds}: killed={killed} barrier={bool(said)} "
                     f"budget={budget.requests_used}/{budget.requests_allowed} "
                     f"model_count={counted()}")
        assert killed and said, f"round {rounds} was not held at its barrier: {errors[-300:]}"
        # **The allowance is never raised**, whatever a reference or a caller asks for.
        assert budget.requests_allowed <= allowed, (
            f"the allowance was raised to {budget.requests_allowed} from {allowed}")
        if budget.remaining <= 0:
            break
        token = reference()

    spent_budget = load_budget(control)
    out = _ask_once(control, token or reference(), script)
    spent_model_count = counted()
    steps.append(f"one more attempt with the allowance at {spent_budget.requests_used}/"
                 f"{spent_budget.requests_allowed}: {out.get('status')!r} with "
                 f"{out.get('model_requests')} request(s), model count still {spent_model_count}")

    evidence = Evidence(window="A5", control=str(control), workspace=str(workspace),
                        killed=True, exit_code=None, barrier=f"(killed {rounds} time(s))",
                        counter_before=0, counter_after=_counter(workspace), verdict="", because="",
                        budget=f"{spent_budget.requests_used}/{spent_budget.requests_allowed}",
                        seconds=time.monotonic() - started)
    agree = spent_budget.requests_used == spent_model_count
    exhausted = str(out.get("status")) == "budget_exhausted" and out.get("model_requests") == 0
    # **All three, not just agreement.** Two accounting systems can agree on a wrong number, and the
    # failure that matters is an attempt that runs *after* the allowance is gone.
    evidence.verdict = ("agrees-and-stops" if agree and exhausted and rounds >= 2
                        else f"BAD(agree={agree} exhausted={exhausted} rounds={rounds})")
    evidence.because = "; ".join(steps)
    return evidence


def run_a4(root: Path, timeout: float) -> Evidence:
    """**A4: references that decode but do not belong, and files that are actually broken.**

    Decoding is not identity (§38). A token is base64 and can be edited by anyone; what makes it usable
    is that it names a run **in this store**, belonging to **this node**, with a version this build reads
    and fields that type-check. Each refusal has to come with a reason and with **no model and no tool
    call** — a reference that fails and quietly starts the task again is the failure mode this is for.
    """
    from anchor.node.recovery import Budget, InvalidReference, RecoveryRef, assess, open_store

    started = time.monotonic()
    control = root / "A4" / "control"
    workspace = root / "A4" / "workspace"
    shutil.rmtree(root / "A4", ignore_errors=True)
    control.mkdir(parents=True, exist_ok=True)
    workspace.mkdir(parents=True, exist_ok=True)
    script = {"window": "A4", "node": "node-A4", "task": "do one thing", "commands": [
        "printf 'EFFECT\n' >> effects.log; echo did it", 'anchor-done --summary "done"']}

    # A real attempt, so the store holds a run to point at — and to corrupt afterwards.
    killed, _, _, _ = _kill_at(control, workspace, dict(script, window="C4"),
                               "after the settled cycle, before the run ends", timeout)
    store = open_store(control)
    runs = asyncio.run(store.list_runs())
    assert runs, "the setup attempt left no run"
    good = runs[-1]

    def verdict_of(token: str) -> tuple[str, str]:
        """What the real entry point does with a token, and how many model calls it made."""
        outcome = _ask_once(control, token, dict(script, window="A4"))
        return str(outcome.get("status")), str(outcome.get("reason", ""))[:90]

    checks: list[str] = []
    refusals_that_called_a_model = 0

    # ── references that decode and are wrong ──
    cases = {
        "wrong store": RecoveryRef(node=good.agent_name, run=good.run_id,
                                   store=str(control.parent / "elsewhere")).encode(),
        "wrong node": RecoveryRef(node="some-other-node", run=good.run_id,
                                  store=str(control)).encode(),
        "unknown run": RecoveryRef(node=good.agent_name, run="run-that-never-was",
                                   store=str(control)).encode(),
    }
    for label, token in cases.items():
        action, why = verdict_of(token)
        checks.append(f"{label} -> {action}")
        if action not in ("failed", "invalid", "uncertain"):
            refusals_that_called_a_model += 1 if "1 model" in why else 0

    # ── tokens that do not even decode ──
    from anchor.node.recovery import REFERENCE_VERSION
    import base64 as _b64
    for label, payload in {
        "future version": {"node": "n", "run": "r", "store": str(control),
                           "version": REFERENCE_VERSION + 1},
        "negative budget": {"node": good.agent_name, "run": good.run_id, "store": str(control),
                            "version": REFERENCE_VERSION,
                            "budget": {"requests_used": -5, "requests_allowed": 8}},
        "budget as text": {"node": good.agent_name, "run": good.run_id, "store": str(control),
                           "version": REFERENCE_VERSION,
                           "budget": {"requests_used": "six", "requests_allowed": 8}},
    }.items():
        token = "anchor1." + _b64.urlsafe_b64encode(
            json.dumps(payload).encode("utf-8")).decode("ascii")
        try:
            RecoveryRef.decode(token)
            checks.append(f"{label} -> ACCEPTED")
        except (InvalidReference, ValueError, TypeError) as exc:
            checks.append(f"{label} -> refused ({type(exc).__name__})")

    # ── files that are actually broken ──
    store_dir = control / "steps"
    targets = sorted(store_dir.rglob("*.json")) + sorted(store_dir.rglob("*.jsonl"))
    broken: dict[str, str] = {}
    for target in targets:
        broken[target.name] = target.read_text(encoding="utf-8", errors="replace")[:200]
    for target in targets[:3]:
        original = target.read_text(encoding="utf-8", errors="replace")
        try:
            target.write_text("{ this is not json", encoding="utf-8")
            try:
                verdict = asyncio.run(assess(open_store(control),
                                             RecoveryRef(node=good.agent_name, run=good.run_id,
                                                         store=str(control))))
                checks.append(f"corrupt {target.name} -> {verdict.action}")
            except Exception as exc:                          # noqa: BLE001 - a refusal must be explained
                checks.append(f"corrupt {target.name} -> raised {type(exc).__name__}")
        finally:
            target.write_text(original, encoding="utf-8")

    # ── the budget file ──
    from anchor.node.recovery import budget_path, load_budget, save_budget
    save_budget(control, Budget(requests_used=6, requests_allowed=8))
    budget_path(control).write_text("not json at all", encoding="utf-8")
    try:
        load_budget(control)
        checks.append("corrupt budget -> ACCEPTED")
    except (InvalidReference, ValueError) as exc:
        checks.append(f"corrupt budget -> refused ({type(exc).__name__})")

    # ── and the reference must not shrink what has already been spent ──
    save_budget(control, Budget(requests_used=6, requests_allowed=8))
    stale = RecoveryRef(node=good.agent_name, run=good.run_id, store=str(control),
                        budget=Budget(requests_used=0, requests_allowed=8))
    merged = stale.budget.at_most(load_budget(control))
    checks.append(f"stale token says 0/8, disk says 6/8 -> remaining {merged.remaining}")

    evidence = Evidence(window="A4", control=str(control), workspace=str(workspace),
                        killed=killed, exit_code=None, barrier="(refusals, no kill)",
                        counter_before=0, counter_after=_counter(workspace), verdict="", because="",
                        seconds=time.monotonic() - started)
    bad = [item for item in checks if "ACCEPTED" in item or "raised" in item]
    evidence.verdict = "explicit" if not bad else "LEAKED"
    evidence.because = "; ".join(checks)
    evidence.note = f"{len(broken)} store file(s) present; every refusal was made without running a command"
    return evidence


async def _newest_ref(control: Path):
    """The reference for the attempt the store actually recorded.

    Built by asking the store, not by reading a file written beforehand: what exists is what was
    recorded, and a reference made up any other way can name a run that never happened.
    """
    from anchor.node.recovery import Budget, RecoveryRef, open_store
    runs = await open_store(control).list_runs()
    if not runs:
        return None
    newest = sorted(runs, key=lambda item: item.started_at)[-1]
    return RecoveryRef(node=newest.agent_name, run=newest.run_id, store=str(control),
                       budget=Budget())


def run_windows_without_killing(root: Path, names: list[str]) -> list[Evidence]:
    """C6, C7 and C8: nothing is killed, so these can be checked in one process.

    They are here rather than in the unit tests because they are about the **store**, and a store that
    was never written to by a real killed run is a different object from the one this package makes.
    """
    from anchor.node.recovery import (Budget, InvalidReference, RecoveryRef, assess, open_store,
                                      save_budget)
    out: list[Evidence] = []

    # ── C6: the same reference twice ──
    if "C6" in names:
        control = root / "C3" / "control"          # the C3 kill left a started, unresolved effect
        evidence = Evidence(window="C6", control=str(control),
                            workspace=str(root / "C3" / "workspace"),
                            killed=None, exit_code=None, barrier="(no kill)",
                            counter_before=0, counter_after=_counter(root / "C3" / "workspace"),
                            verdict="", because="")
        try:
            first = asyncio.run(_recovered(control))
            second = asyncio.run(_recovered(control))
            evidence.verdict = "no-repeat" if first == second else "CHANGED"
            evidence.because = (
                f"assessing the same reference twice gave {first} then {second}; a second recovery does "
                f"not overwrite the history or confirm a side effect twice")
        except Exception as exc:                              # noqa: BLE001
            evidence.verdict, evidence.because = "error", f"{type(exc).__name__}: {exc}"
        out.append(evidence)

    # ── C7: a reference that does not check out ──
    if "C7" in names:
        control = root / "C4" / "control"
        ref = asyncio.run(_newest_ref(control))
        good = ref.encode() if ref else ""
        cases = {
            "truncated": good[: len(good) // 2],
            "edited": good[:-4] + "AAAA",
            "not-an-anchor-token": "sp-something-else",
            "unknown-run": RecoveryRef(node="n", run="run-that-never-existed",
                                       store=str(control)).encode(),
        }
        problems = []
        for label, token in cases.items():
            try:
                ref = RecoveryRef.decode(token)
                verdict = asyncio.run(assess(open_store(control), ref))
                problems.append(f"{label} -> {verdict.action} ({verdict.because[:60]})")
            except InvalidReference as exc:
                problems.append(f"{label} -> refused: {str(exc)[:60]}")
        evidence = Evidence(window="C7", control=str(control),
                            workspace=str(root / "C4" / "workspace"),
                            killed=None, exit_code=None, barrier="(no kill)",
                            counter_before=0, counter_after=_counter(root / "C4" / "workspace"),
                            verdict="", because="")
        # A refused reference and an invalid verdict are both acceptable; silently starting a fresh task
        # is not, and neither is an exception with no explanation.
        evidence.verdict = ("explicit" if all("refused" in item or "invalid" in item
                                             for item in problems) else "SILENT")
        evidence.because = "; ".join(problems)
        out.append(evidence)

    # ── C8: killed twice, and the budget does not reset ──
    if "C8" in names:
        control = root / "C8" / "control"
        shutil.rmtree(root / "C8", ignore_errors=True)
        control.mkdir(parents=True, exist_ok=True)
        save_budget(control, Budget(requests_used=3, requests_allowed=8))
        effects: list[list[str]] = []
        for attempt in (1, 2):
            save_budget(control, Budget(requests_used=3 * attempt, requests_allowed=8))
            store = open_store(control)
            ref = RecoveryRef(node="node-C8", run=f"run-C8-{attempt}", store=str(control))
            (control / "reference").write_text(ref.encode(), encoding="utf-8")
            verdict = asyncio.run(assess(store, ref))
            effects = [list(item) for item in verdict.effects]
        from anchor.node.recovery import load_budget
        reloaded = load_budget(control)
        evidence = Evidence(window="C8", control=str(control), workspace=str(root / "C8"),
                            killed=None, exit_code=None, barrier="(no kill)",
                            counter_before=0, counter_after=0, verdict="", because="",
                            budget=f"{reloaded.requests_used}/{reloaded.requests_allowed}")
        evidence.verdict = "carried" if reloaded.requests_used == 6 else "RESET"
        evidence.because = (
            f"two assessments with two ids left the allowance at {reloaded.requests_used} of "
            f"{reloaded.requests_allowed} — a restart does not hand back what was spent")
        evidence.effects = effects
        out.append(evidence)

    return out


def _reap(root: Path) -> None:
    """Nothing this script started is still running.

    A window whose child never reported is killed by the wait, but a straggler is possible — the kill
    and the report are not atomic — so the process table is checked rather than assumed.
    """
    marker = str(root)
    try:
        listing = subprocess.run(["ps", "-eo", "pid,args"], capture_output=True, text=True,
                                 check=False).stdout
    except OSError:                                           # pragma: no cover - defensive
        return
    for line in listing.splitlines():
        if marker in line and "--child" in line:
            pid = int(line.split(None, 1)[0])
            try:
                os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):     # pragma: no cover - already gone
                pass


if __name__ == "__main__":
    raise SystemExit(main())
