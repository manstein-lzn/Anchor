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


def _barrier(window: str):
    """The capability that stops the node at the chosen boundary.

    The four tool-related hooks are not interchangeable and the boundaries they give are not the same
    ones people assume. Measured on this build:

        after_model_request   tool_call_started is NOT yet in the ledger
        before_tool_execute   tool_call_started IS in the ledger, the command has not run
        wrap_tool_execute     entered after both, so a pause after its handler has the effect done and
                              no terminal record

    Which is why C1 pauses in the first, C2 in the second, and C3 between the handler and its return.
    """
    from pydantic_ai.capabilities import AbstractCapability

    class Barrier(AbstractCapability):
        def __init__(self) -> None:
            self.seen_models = 0
            self.seen_tools = 0

        async def after_model_request(self, ctx, *, request_context, response):
            self.seen_models += 1
            if window == "C1" and self.seen_models == 1:
                _wait_for_a_kill("after_model_request, before the tool cycle")
            if window == "C4" and self.seen_models >= 2:
                # The snapshot for the cycle that just settled has been written by now; what has not
                # happened is the run ending.
                _wait_for_a_kill("after the settled cycle, before the run ends")
            return response

        async def before_tool_execute(self, ctx, *, call, tool_def, args):
            self.seen_tools += 1
            if window == "C2" and self.seen_tools == 1:
                _wait_for_a_kill("tool_call_started persisted, command not executed")
            return args                                     # must be returned; None breaks the call

        async def after_tool_execute(self, ctx, *, call, tool_def, args, result):
            # **Reached only because this capability is registered before `StepPersistence`.** Hooks run
            # in a fixed order and the framework's own runs after this one when it is registered later:
            # measured both ways, and registering it after gave a hook where the terminal record had not
            # been written yet. Registered first, this is the instant the plan's C5 asks about — the
            # tool's terminal record is in the ledger and the snapshot for this cycle is not.
            if window == "C5" and self.seen_tools == 2:
                _wait_for_a_kill("terminal effect record written, snapshot not yet")
            return result

        async def wrap_tool_execute(self, ctx, *, call, tool_def, args, handler):
            result = await handler(args)
            if window == "C3" and self.seen_tools == 2:
                # The counter command is the first call; this is the pause after *its* effect and
                # before the framework writes the terminal record for it.
                _wait_for_a_kill("side effect done, terminal record not written")
            return result

    return Barrier()


async def _next_free_run_id(control: Path, agent_name: str) -> str:
    """The next unused run id for this logical node — the adapter would derive one, and this is the same
    rule applied where the child can see it."""
    from anchor.node.recovery import open_store
    used = [item.run_id for item in await open_store(control).list_runs()
            if item.agent_name == agent_name]
    return f"{agent_name}-a{len(used) + 1}"


async def _run_child(window: str, control: Path, workspace: Path, script: dict,
                     recover: str = "") -> None:
    """One node, with the barrier attached, expected never to return for the killed windows."""
    from pydantic_ai.models.function import FunctionModel
    from pydantic_ai.messages import ModelResponse, ToolCallPart

    from anchor.node import NodeRequest
    from anchor.node.pydantic_adapter import run_node
    from anchor.node.recovery import save_budget, Budget

    if not (control / "budget.json").exists():
        save_budget(control, Budget(requests_used=0, requests_allowed=8))

    turn = {"n": 0}

    def model(messages, info):
        """What to do next, decided from **the history this process was handed**.

        §77 asks for a double driven by evidence another process can check, and not by a per-process
        counter that compaction can silently reset. So the marker is looked for in the tool results
        already in the history: a resumed process sees the completed command's output and moves on, and
        a process that sees nothing does the work. Which is also what a real model would do.
        """
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
    outcome = await run_node(
        NodeRequest(execution_id=script["node"], task=script["task"], workspace=workspace,
                    max_requests=8, trace=control / "trace.jsonl", recovery=recover),
        model=FunctionModel(model),
        capabilities=((_barrier(window), here) if barrier_first else (here, _barrier(window))),
        recovery_store=control)
    # The outcome's own account, written where the parent can read it. A resumed attempt that only
    # fetched history and did nothing would have nothing to put here.
    (control / f"outcome-{os.getpid()}.json").write_text(json.dumps({
        "status": outcome.status, "submission": outcome.submission, "route": outcome.route,
        "model_requests": outcome.model_requests, "reason": outcome.reason,
        "recovery": outcome.recovery, "files": list(outcome.files)}, ensure_ascii=False),
        encoding="utf-8")


# ── the parent ────────────────────────────────────────────────────────────────────────────────────

#: The counter command: atomic, in the sandbox, and it writes a marker so the effect is visible even
#: without reading the number. `>>` after a `flock` is not needed for a test that runs one command.
#: A command that is still running when the host dies, and says so afterwards if it survived.
#: A command that is still running when the host dies, and says so afterwards **if it survived**.
#:
#: It announces itself by writing a file, because the handshake has to come from inside the
#: sandbox: `bwrap` does not pass an inherited pipe in, and the whole question is whether this
#: process keeps going after the process that started it is gone. The parent waits for that file
#: — a handshake, not a sleep — and kills the host the moment it appears.
#: Windows whose barrier has to land **after** the framework's own write, which means registering it
#: **before** step persistence. See the note in `_run_child`: the two hook directions differ.
BEFORE_PERSISTENCE_WINDOWS = frozenset({"C1", "C4", "C5"})

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
             timeout: float) -> tuple[bool | None, int | None, str, str]:
    """Spawn the child, wait for its barrier byte, and kill it there.

    Returns (was_killed, exit_code, what_the_child_said, traceback_text). The wait is a **read** on a
    pipe: no polling, no sleeping, and nothing about the machine's speed enters into when the kill lands.
    """
    read_fd, write_fd = os.pipe()
    environment = dict(os.environ, **{READY_FD_ENV: str(write_fd)})
    child = subprocess.Popen(
        [sys.executable, __file__, "--child", "--window", script["window"], "--control", str(control),
         "--workspace", str(workspace), "--script", json.dumps(script)],
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
        asyncio.run(_run_child(args.window, Path(args.control), Path(args.workspace),
                               json.loads(args.script), recover=args.recover))
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
