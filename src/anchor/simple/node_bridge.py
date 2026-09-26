"""The scheduler's node, backed by the node runtimes instead of by mini.

`_agent_for` used to return mini's `TracingAgent` for every node: an agent built around a model, and an
op built around a scripted model of one command. After ADR-062 it returns one of these, which is the
same thing the scheduler asked for — `run(task)` / `resume()`, and a `route` to read afterwards —
coming from `run_agent_node` and `run_op_node` instead. The scheduler is otherwise untouched: it still
writes the cursor, still freezes the commit, still decides the edges, and still knows nothing about
which loop ran the work.

**The dict this returns is the interface that existed, not a new one.** `_result_of` reads
`exit_status == "Submitted"`, `submission` and the `route` beside it. Those are the seam the scheduler
was written against in package 01, so they are what this produces and the reason the switch is one file
rather than a rewrite of the loop. Nothing here reads a trace: what a node continues from is its own
step store, which is the one thing that can answer whether repeating the work is safe.

**A node's control directory is derived here and not passed in.** Where a node keeps its record, its
budget and its completion is the node's business (ADR-062 invariant 1). The scheduler names the run
directory; this turns that name into the control directory beside it, and the node never learns the
run's layout.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any, Callable

from anchor.node import NodeRequest, node_key
from anchor.node.op_runtime import run_op_node
from anchor.node.recovery import RecoveryRef, open_store


class Node:
    """One node's pass, however it is run. What the scheduler holds between `_agent_for` and `_record`."""

    def __init__(self, *, node_id: str, directory: Path, routes: tuple[str, ...],
                 inputs: tuple[Any, ...], trace: Path | None, model: Any, instructions: str,
                 network: bool, timeout_seconds: float, max_requests: int | None,
                 command: str | None = None, control: Path | None = None,
                 capabilities: tuple[Any, ...] = (),
                 resources: tuple[tuple[str, str], ...] = (),
                 cancelled: Callable[[], bool] | None = None) -> None:
        self.node_id = node_id
        self.directory = Path(directory)
        self.routes = tuple(routes)
        self.inputs = tuple(inputs)
        self.trace = trace
        self.command = command
        self.control = Path(control) if control is not None else None
        self._model = model
        self._instructions = instructions
        self._network = network
        self._timeout = timeout_seconds
        self._max_requests = max_requests
        self._capabilities = tuple(capabilities)
        self._resources = resources
        self._cancelled = cancelled
        # Read by `_result_of` through `agent.env.route`. Kept as an object rather than a bare
        # attribute so the shape the scheduler reads does not change with the loop behind it.
        self.env = _Route()
        self.route: str | None = None

    # ── what the scheduler calls ─────────────────────────────────────────────────────────────────

    def run(self, task: str, *, resume_mark: bool = False) -> dict:
        """One pass. `resume_mark` says a previous attempt of this same pass is gone.

        The node does not read a trace either way: **its own record is the authority on what may be
        continued**, and a node that reads a conversation back into the model without asking the store
        whether the work is safe to repeat is the mistake the recovery module exists to refuse. What the
        mark changes is the question — a fresh attempt is asked about itself, and one whose earlier
        attempt left no trace is asked whether *something happened that this process cannot see*.
        """
        return self._dispatch(task=task, recovery=False, resume_mark=resume_mark)

    def resume(self) -> dict:
        """Continue a previous attempt whose record is still on disk.

        Nothing is passed in, because there is nothing a caller knows about an interrupted attempt that
        the node does not: what it continues from is the step store, and a trace is only what tells the
        scheduler this node was interrupted in the middle rather than never started.
        """
        return self._dispatch(task=None, recovery=True)

    # ── the dispatch ─────────────────────────────────────────────────────────────────────────────

    def _dispatch(self, *, task: str | None, recovery: bool, resume_mark: bool = False) -> dict:
        if self.command is not None:
            # **An op does not resume, and the scheduler is told that rather than left to guess.** Its
            # command is a program that may have run; there is no model to re-ask and no conversation to
            # continue, so a repeated invocation is exactly the side effect that must not happen twice.
            if recovery or resume_mark:
                self.route = None
                self.env.route = None
                return {"submission": "", "exit_status": "Uncertain",
                        "reason": "an op is a single command and has no attempt to continue: a previous "
                                  "attempt of this pass may already have run it, so it is not re-run "
                                  "automatically"}
            outcome = run_op_node(
                NodeRequest(execution_id=self.node_id, task=self.command, workspace=self.directory,
                            inputs=_binds(self.inputs), routes=self.routes, network=self._network,
                            timeout_seconds=self._timeout, roles=self.node_id,
                            cancelled=self._cancelled),
                command=self.command)
            # **The runtime's own answer, and nothing re-derived from it.** It already read the exit
            # code against the command line it dispatched — the one fact a caller could not supply and
            # be trusted about — so the two things this record needs beyond the status come off the
            # outcome: its submission, and whether the command was on the PATH at all.
            self.route = outcome.route
            self.env.route = outcome.route
            # **What a failed op says is the reason, not its output.** `OpEnvironment` ended one with
            # `submission = the refusal`, and the refusal is the diagnosis: an op that exited zero with
            # two ways out and chose neither printed something true and said nothing about the failure,
            # so recording its stdout would lose the only sentence that explains the run. Its own output
            # is in `OpResult` where it was read.
            return {"submission": (outcome.submission if outcome.status == "completed"
                                   else outcome.reason or outcome.submission),
                    "exit_status": ("Submitted" if outcome.status == "completed"
                                    else _spelled(outcome.status, outcome.command_missing)),
                    "reason": outcome.reason}
        else:
            # **Imported here, not at the top, and the difference is whether `run.py` can be loaded
            # without the harness.** The adapter is the one module in the runtime that names
            # `pydantic_ai`, and `run.py` imports this file on the default path; a module-scope import
            # would make an op-only graph need the framework installed. A12/A13 hold the invariant by
            # running a graph in an interpreter where it cannot be imported at all.
            from anchor.node.adapter import run_agent_node

            outcome = asyncio.run(run_agent_node(
                NodeRequest(execution_id=self.node_id, task=task or "", workspace=self.directory,
                            instructions=self._instructions, inputs=(*_binds(self.inputs), *self._resources),
                            routes=self.routes, network=self._network,
                            timeout_seconds=self._timeout, max_requests=self._max_requests,
                            trace=self.trace, roles=self.node_id, cancelled=self._cancelled,
                            # **A write that landed before the record is what `recovery` is for.** The
                            # adapter refuses a reference whose node, workspace, store or budget does not
                            # match, and a bare name is not one — so the node says `uncertain` instead,
                            # which is the honest answer and the one the graph may not schedule.
                            recovery=_resumed_token(self.control, self.node_id) if resume_mark else ""),
                model=self._model, capabilities=self._capabilities, recovery_store=self.control))
        self.route = outcome.route
        self.env.route = outcome.route
        if outcome.status == "completed":
            return {"submission": outcome.submission, "exit_status": "Submitted",
                    "reason": outcome.reason}
        # **What a failed agent said, and in the record's own words.** An agent that fails has nothing
        # to submit, so the reason is what the record holds — which is what a mini pass's `submission`
        # was on that path. The adapter keeps status, reason and submission apart; the run's record has
        # one field for a result, and this is where the two vocabularies meet.
        return {"submission": outcome.submission or outcome.reason,
                "exit_status": _spelled(outcome.status, outcome.command_missing),
                "reason": outcome.reason}


class _Route:
    """The one field the scheduler reads off a node's environment."""

    route: str | None = None


def _resumed_token(control: Path | None, node_id: str) -> str:
    """A reference to this node's last attempt, when there is one to point at.

    The adapter will not accept a token it cannot check — the node, the workspace, the store and the
    allowance all have to match — and it refuses one that does not rather than running the work again.
    That refusal is the answer: a previous attempt of this pass is gone and whether its side effects
    happened is not knowable from here.
    """
    if control is None:
        return ""
    try:
        runs = [item for item in asyncio.run(open_store(Path(control)).list_runs())
                if item.agent_name == node_key(node_id)]
    except Exception:                                    # noqa: BLE001 - no store, so nothing to point at
        return ""
    if not runs:
        return ""
    newest = sorted(runs, key=lambda item: item.started_at)[-1]
    return RecoveryRef(node=node_key(node_id), run=newest.run_id, store=str(control)).encode()



def _spelled(status: str, command_missing: bool) -> str:
    """How a node that did not submit names what happened, in the words a run's record already uses.

    `CommandNotFound` is kept as its own name because it is not a failed check: nothing was checked.
    The node runtime says whether that is what happened rather than this reading it off an exit code —
    what a code means is the runner's business, and a caller inferring it is a caller that breaks when
    a different runner numbers things differently.
    """
    if status == "failed" and command_missing:
        return "CommandNotFound"
    return {"failed": "Failed", "uncertain": "Uncertain",
            "budget_exhausted": "LimitsExceeded"}.get(status, status)


def _binds(inputs: tuple[Any, ...]) -> tuple[tuple[str, str], ...]:
    """A step's given inputs, as the `(host path, mount point)` pairs a request carries."""
    return tuple(bind for item in inputs for bind in item.binds())
