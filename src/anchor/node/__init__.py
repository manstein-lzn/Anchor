"""Running one node, with Anchor's own types on both sides of the seam.

A Node is asked to do one pass of work in one workspace and to say how it went. What it runs is not
this module's business. Since M3 of ADR-062's migration there is one runner behind this and it is two
modules rather than one: `agent_runtime.py` is the harness half, `adapter.py` is the runtime half, and
`adapter.py`'s `run_agent_node` is the entry point. `op_runtime.py`'s `run_op_node` is the other kind of
node and reaches none of it. The Graph scheduler is written against none of them. Nothing here names a
framework, and nothing here imports one.

**The contract is the semantics, not the classes.** These dataclasses may change shape. What must not
change is in `NodeRequest`'s and `NodeOutcome`'s field docs: an execution identifier that does not
pretend to be a resume token, a workspace the caller has already prepared, inputs already resolved to
host paths and read-only mount points, routes the node may choose among with the Graph keeping the
final say, and a result that carries no route unless it completed.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

#: Done. The pass produced what it was asked for and named a way out if it had to.
COMPLETED = "completed"
#: Out of turns or out of clock, with nothing submitted. Not a failure of the work — the conversation
#: is good and continuing it is the right response — so it is its own status and never a route.
BUDGET_EXHAUSTED = "budget_exhausted"
#: The work went wrong, or the node said it was finished without finishing. Terminal.
FAILED = "failed"
#: A previous attempt was interrupted, and **what it managed to do cannot be established**. The side
#: effect may or may not have happened, so this attempt did nothing at all. Its own status because the
#: two things a caller might do — retry, or give up — are both wrong: retrying may repeat a side effect,
#: and giving up discards work that may be finished. §35 of the G2 plan calls this a correct result.
UNCERTAIN = "uncertain"

STATUSES = (COMPLETED, BUDGET_EXHAUSTED, FAILED, UNCERTAIN)


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

    max_requests: int | None = None
    """Optional cumulative request budget, preserved across recovery. None means unbounded;
    zero permits no requests."""

    recovery: str = ""
    """An opaque token for a previous attempt of this same node, or empty to start fresh.

    **The Graph carries this and cannot read it** (§21): what is inside — the framework's run id, the
    store's location, the allowance already spent — is the node's business. A caller that resumes passes
    back what the last outcome handed it; a caller that does not gets a fresh attempt.
    """

    trace: Path | None = None
    """Where the record of this execution goes. **Outside the workspace**: inside it is a file the
    node can read, and one did, and reasoned about its own conversation instead of its task."""

    roles: str = ""
    """A label for the trace, not a behaviour."""

    cancelled: Callable[[], bool] | None = None
    """Whether the caller has asked to stop this pass now."""

    @property
    def node_key(self) -> str:
        """The node's name in the framework's own vocabulary, and **not always `execution_id`**.

        A node inside a module is legitimately called `work/draft` — that is its name in the graph, in
        the run directory and in a person's sentence about it — and the framework's step store refuses a
        `/` in an identifier, because it interpolates one into a path. So a name that has to be legal
        *and* stable is derived once, here, and used for every identity the framework sees: the store's
        `agent_name`, the run ids built from it, and the conversation. Deriving it in one place is the
        point — two spellings of the same node in one store is a node that cannot find its own attempt.

        Kept beside `execution_id` rather than replacing it because they answer different questions:
        `execution_id` is the Graph's name for this node and is what a caller reads in a record;
        `node_key` is only ever a name for the store.
        """
        return node_key(self.execution_id)


def node_key(execution_id: str) -> str:
    """The one rule, for the callers that have a name and not a request.

    The Graph and the store both need this before a request exists — the scheduler reads a node's
    completion fact before it builds anything — so the rule is a function and the property is a
    convenience over it. Two copies of `replace("/", "__")` would be two chances to disagree about what
    a node is called.
    """
    return execution_id.replace("/", "__")


@dataclass(frozen=True)
class NodeOutcome:
    """What one execution produced, in Anchor's words."""

    status: str
    submission: str = ""
    """What the node said it did, taken from the validated structured completion rather than from a
    model's unvalidated last sentence. Empty unless it completed."""

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

    command_missing: bool = False
    """Whether the command this node was to run was not on the sandbox PATH at all, which is not the
    same failure as one that ran and did not pass. Only an op sets it, and it is **the runner saying
    so rather than the caller working it out**: the meaning of an exit code belongs to whoever runs the
    command — a later op runtime on a different scaffold may report the same thing differently, and a
    caller that parsed output for it would be the caller that breaks when it does."""

    returncode: int = 0
    """The exit code the node's command ended with, for the runtimes that run one. Zero for an agent,
    which has no single command to point at. A **fact about the attempt**, not a status: the same
    status covers several codes, and a caller that wants the code has to be given it."""

    invocation: str = ""
    """The command as it was actually dispatched, or empty when the shape of the call was not a
    command's. Kept because a failure is read against what was run: the runtime answered "there is no
    such command" about *this* text, and a caller that had to remember what it asked for would be
    holding half of the evidence for a verdict it is given in full. A label here, not a resume token —
    nothing may be reconstructed from it."""

    recovery: str = ""
    """The token for this attempt, for a caller that wants to resume it. Empty when nothing was recorded.

    Handed out with every outcome, including a failed one: what a caller needs in order to ask "can this
    be picked up" is a name for the attempt, and making it guess one would put the store's shape in the
    Graph."""

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
