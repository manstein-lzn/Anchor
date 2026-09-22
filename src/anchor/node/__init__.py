"""Running one node, with Anchor's own types on both sides of the seam.

A Node is asked to do one pass of work in one workspace and to say how it went. What it runs is not
this module's business: `pydantic_adapter` is one runner, `simple/agent.py` is another, and the Graph
scheduler is written against neither. Nothing here names a framework, and nothing here imports one.

**The contract is the semantics, not the classes.** These dataclasses may change shape. What must not
change is in `NodeRequest`'s and `NodeOutcome`'s field docs: an execution identifier that does not
pretend to be a resume token, a workspace the caller has already prepared, inputs already resolved to
host paths and read-only mount points, routes the node may choose among with the Graph keeping the
final say, and a result that carries no route unless it completed.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

#: Done. The pass produced what it was asked for and named a way out if it had to.
COMPLETED = "completed"
#: Out of turns or out of clock, with nothing submitted. Not a failure of the work — the conversation
#: is good and continuing it is the right response — so it is its own status and never a route.
BUDGET_EXHAUSTED = "budget_exhausted"
#: The work went wrong, or the node said it was finished without finishing. Terminal.
FAILED = "failed"

STATUSES = (COMPLETED, BUDGET_EXHAUSTED, FAILED)


@dataclass(frozen=True)
class NodeRequest:
    """One execution of one node. Prepared by the caller; not interpreted by the runner.

    Everything the runner may touch is named here. It does not walk the graph, does not resolve an
    edge, and does not read a workspace it was not handed — which is what keeps a node's runner
    replaceable without touching scheduling.
    """

    execution_id: str
    """Which execution this is. **Not a resume token**: nothing yet promises a second process can pick
    an execution up from it, and a field that looks like one would be read as one."""

    task: str
    """The objective, as the node is told it."""

    instructions: str = ""
    """What this node adds to the role. The rules every node obeys are the runner's to supply, not the
    caller's — they are a contract, and a contract each node may reword is not one."""

    workspace: Path = Path()
    """Where it works. Kept between passes; whatever is in it when it finishes is what the next node
    is pointed at."""

    inputs: tuple[tuple[str, str], ...] = ()
    """What it was given, as (where it lives on the host, where it is visible inside). Read-only.
    Resolved already — the runner does not know what an edge is."""

    routes: tuple[str, ...] = ()
    """The node ids it may choose among. Empty or one means it does not choose, and its own edge
    follows. Validated by the runner and re-validated by the Graph, which keeps the final say: a
    route the node asks for is a request to be scheduled, not a scheduling decision."""

    network: bool = False
    timeout_seconds: float = 600.0
    """One command. Not the pass — a pass is bounded by `max_requests` and the caller's own clock."""

    max_requests: int = 60
    """How many times the model may be asked before the pass is out of budget. Counted **for this
    execution only**; nothing here claims a budget carries across a resume."""

    trace: Path | None = None
    """Where the record of this execution goes. **Outside the workspace**: inside it is a file the
    node can read, and one did, and reasoned about its own conversation instead of its task."""

    roles: str = ""
    """A label for the trace, not a behaviour."""


@dataclass(frozen=True)
class NodeOutcome:
    """What one execution produced, in Anchor's words."""

    status: str
    submission: str = ""
    """What the node said it did, taken from the completion command rather than from a model's last
    sentence. Empty unless it completed."""

    route: str | None = None
    """Which way out it chose. **Only a completed execution has one**: a node that ran out of budget
    or failed must not be schedulable, or the graph moves on the strength of work that did not
    happen."""

    model_requests: int = 0
    """How many times the model was asked, this execution. Pairs with `max_requests`."""

    trace_ref: str = ""
    """Where the record is. The only thing about the record the caller is given: the record itself may
    hold whatever the runner needed, and exposing its shape would freeze the runner."""

    reason: str = ""
    """Why it is not `completed`. Empty when it is."""

    files: tuple[str, ...] = field(default=())
    """What it left in its workspace, relative, sorted. A convenience for a caller that would
    otherwise walk the tree; the workspace is the authority."""

    def __post_init__(self) -> None:
        if self.status not in STATUSES:
            raise ValueError(f"{self.status!r} is not one of {STATUSES}")
        if self.status != COMPLETED and self.route is not None:
            raise ValueError(
                f"a {self.status} execution named a route ({self.route!r}). A node that did not "
                f"finish must not be schedulable — that is how a graph comes to move on the strength "
                f"of work that did not happen.")
        if self.status != COMPLETED and not self.reason:
            raise ValueError(f"a {self.status} execution has to say why")
