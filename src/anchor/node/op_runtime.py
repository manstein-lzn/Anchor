"""Run one op node: a single command in the real sandbox, and its exit code is the verdict.

An op is not a second mechanism (ADR-061). It has the same workspace, the same read-only pointer to
what came before, the same commit per pass and the same record as an agent node; what changes is one
thing — a program decides instead of a model.

**Nothing in this module knows the agent runtime exists.** That is the invariant ADR-062 fixes and the
one a import makes checkable: `pydantic_ai` and `pydantic_ai_harness` must not appear in this file's
imports, because an op that pulled them in would make "ops do not depend on the harness" unverifiable
by construction rather than by test. `tests/test_ops.py` asserts it.

**Why this exists at all, when mini already runs ops.** Today the op path goes through
`simple/agent.py`'s `OpEnvironment`, which means an op node imports `minisweagent` to run one command.
After M3 that import is gone and this is what is left. It is a mirror of `OpEnvironment`'s rules rather
than a re-use of them, deliberately: those rules live behind the framework the migration is removing.

**The protocol it implements, which is the one `OpEnvironment` implements:**

    exit 0, one way out        the pass finished; stdout is what it says
    exit 0, more than one way  it must route: first line `ANCHOR_ROUTE: <target>`
    exit 127                   the command is not on the sandbox PATH, named as such
    exit non-zero              the pass failed, and the output is the reason
    a marker with non-zero     refused — a command that printed the marker and failed did not finish

stdout is not the success protocol; the exit code is. A command that prints the done marker and exits 1
has failed, and the migration does not adopt mini's reading, which routes on a marker it never checked
the exit code for.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from anchor.node import COMPLETED, FAILED, NodeOutcome, NodeRequest
from anchor.runtime.execenv import NodeSandbox

#: The marker `anchor-route` prints, and the prefix its target follows. The same string the agent
#: runtime and `route/__main__.py` use; one protocol, whichever runtime reads it.
ROUTE_SENTINEL = "ANCHOR_ROUTE:"
DONE_SENTINEL = "COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT"


@dataclass(frozen=True)
class OpResult:
    """What one command did, before it is turned into a `NodeOutcome`.

    Carried separately so the decision — which is the same shape for every kind of result — is
    testable without a sandbox, and so the record has something to hold that is not a status.
    """

    output: str
    returncode: int
    timed_out: bool = False
    route: str | None = None
    #: Why this is not a completion, when it is not.
    refused: str = ""
    #: Set when the command could not be run at all, which is different from one that ran and failed:
    #: a sandbox that would not start is not evidence about the command.
    unreachable: str = ""


def _first_line(text: str) -> str:
    return next((line.strip() for line in text.lstrip().splitlines() if line.strip()), "")


def _after_first(text: str) -> str:
    lines = text.lstrip().splitlines()
    for at, line in enumerate(lines):
        if line.strip():
            return "\n".join(lines[at + 1:]).strip()
    return ""


def read_op_result(ran: Any, routes: tuple[str, ...]) -> OpResult:
    """Turn one command's result into a verdict, by exit code first and output second.

    **The exit code is the verdict, and nothing in the output outranks it.** A timed-out command that
    printed the marker is refused rather than routed, which is where this deliberately differs from
    mini's `_check_finished` — that one routes on the marker without testing the exit code, and a
    `anchor-route` that died mid-write would move the graph on the strength of a command that failed.
    """
    output = getattr(ran, "output", "") or ""
    code = int(getattr(ran, "returncode", 0) or 0)
    first = _first_line(output)

    if getattr(ran, "timed_out", False):
        if first.startswith((DONE_SENTINEL, ROUTE_SENTINEL)):
            return OpResult(output=output, returncode=code, timed_out=True,
                            refused="the command timed out, so its output cannot finish this node")
        return OpResult(output=output, returncode=code, timed_out=True,
                        refused="the command did not finish inside its timeout")

    if code == 127:
        # Not a failed check: a command that is not there. Saying which one beats a bare 127, and it
        # is the failure an author hits most often.
        return OpResult(output=output, returncode=code, unreachable="")
    if code != 0:
        return OpResult(output=output, returncode=code,
                        refused=output.strip() or f"the op's command exited {code}")

    if first.startswith(ROUTE_SENTINEL):
        target = first.split(":", 1)[1].strip()
        if target not in routes:
            return OpResult(output=output, returncode=code,
                            refused=f"{target!r} is not a way out of this node. Choose one of: "
                                    f"{', '.join(routes)}")
        return OpResult(output=output, returncode=code, route=target)
    if first == DONE_SENTINEL and len(routes) == 1:
        return OpResult(output=output, returncode=code)
    if len(routes) > 1:
        # Same rule as an agent's: a node that chooses where the graph goes has to choose.
        return OpResult(output=output, returncode=code,
                        refused=f"this node has more than one way out and did not route. Finish by "
                                f"running `anchor-route --to <{'|'.join(routes)}> --reason \"…\"`")
    return OpResult(output=output, returncode=code)


def run_op_node(request: NodeRequest, *, command: str | None = None) -> NodeOutcome:
    """Run one op: prepare the mounts, run the command once, read the verdict off its exit code.

    Synchronous, and deliberately: it is one command, and nothing about it waits on a model. The
    agent runtime is async because the framework is; making the op runtime async to match would be
    symmetry for its own sake.
    """
    op_command = command if command is not None else request.task
    if not op_command:
        return NodeOutcome(status=FAILED, reason="an op node was given no command to run",
                           files=_files(request.workspace))
    sandbox = NodeSandbox(tree=request.workspace, node_id=request.roles or request.execution_id,
                          network=request.network, timeout_seconds=request.timeout_seconds,
                          routes=request.routes, inputs=request.inputs)
    try:
        sandbox.require_working()
        ran = sandbox.run(op_command)
    except Exception as exc:                      # noqa: BLE001 - a status, not a raise
        # The sandbox could not be established or the command could not be dispatched. That is not
        # evidence about the command, so it is a failure with the reason rather than a verdict read
        # off a return code that does not exist.
        return NodeOutcome(status=FAILED, reason=f"the op's command could not be run: "
                                                 f"{type(exc).__name__}: {exc}",
                           files=_files(request.workspace))
    result = read_op_result(ran, request.routes)
    if result.refused:
        return NodeOutcome(status=FAILED, reason=result.refused, files=_files(request.workspace))
    return NodeOutcome(status=COMPLETED, submission=result.output, route=result.route,
                       files=_files(request.workspace))


def _files(workspace: Path) -> tuple[str, ...]:
    """What the op left, relative and sorted. The workspace is the authority; this is a convenience."""
    if not workspace.is_dir():
        return ()
    return tuple(sorted(
        str(item.relative_to(workspace)) for item in workspace.rglob("*")
        if item.is_file() and ".git" not in item.relative_to(workspace).parts))
