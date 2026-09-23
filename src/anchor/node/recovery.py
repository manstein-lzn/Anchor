"""What survives a crash, and what may honestly be done about it.

A node that is killed in the middle of a command may or may not have run that command. Nothing in this
package can find out: the side effect happened in a shell, and a shell that has been killed leaves no
record of how far it got. What *can* be established is what the framework recorded before the kill —
whether a tool call had started, whether it had returned — and from that, which of four things is true:

    replayable    the model asked for something and no tool call ever began, so running it once is
                  running it once;
    uncertain     a tool call started and never reached a terminal state, so it may or may not have
                  happened. **Not replayable.** The plan treats returning this as a correct result, and
                  it is: "I do not know" is the true answer and guessing it is how a side effect happens
                  twice;
    continuable   every call reached a terminal state and a settled snapshot exists, so the work can
                  carry on from there without repeating anything;
    finished      **the run already submitted.** Continuing would ask the model again and stand a chance
                  of running the submission a second time, so there is nothing to resume — the result is
                  in the history and the caller should read it rather than restart the work;
    invalid       the reference does not check out, so there is nothing to recover from and starting
                  fresh would silently discard whatever did happen.

`finished` is not the framework's `run_completed`: on this node's success path that event never appears,
because the pass leaves `agent.iter` at the boundary after a submission and the framework records the run
as cancelled — measured. So "it already submitted" is read from the history, where the completion
command's own output is.

**Not exactly-once.** This decides what is *knowable*; it does not make a command idempotent, and it does
not reconcile anything. A caller that wants a repeated command to be harmless has to make it harmless.

**And the framework is not being asked for more than it offers.** `StepPersistence`'s own documentation
says it is not a graph-state checkpoint: it does not restore capability state, retry counters, or
graph-node state. So the request budget is persisted here instead, because losing it across a restart
would let a node spend its whole allowance again every time it was killed.
"""

from __future__ import annotations

import base64
import binascii
import hashlib
import json
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any, Literal

if TYPE_CHECKING:                    # never at runtime: the default path must load without the framework
    from pydantic_ai_harness.step_persistence import FileStepStore

#: The reference format's version, in the token itself. A token from an older Anchor is refused by name
#: rather than misread — the fields are not stable across versions and guessing at them is how a
#: recovery reference points at the wrong run.
REFERENCE_VERSION = 1
PREFIX = "anchor1."

#: What a caller may do, and nothing finer. The distinction between the last two is the whole point of
#: the package, so it is the type and not a comment.
Action = Literal["replayable", "uncertain", "continuable", "finished", "invalid"]

#: How many lines of the record a verdict carries back for a person to read. The evidence itself is in
#: the store; this is what makes a report say why rather than only what.
EVIDENCE_LINES = 12


@dataclass(frozen=True)
class Budget:
    """The part of a node's allowance that has to survive a restart.

    Not in the framework's store, because the framework does not keep it: its documentation is explicit
    that retry counters and capability state are out of scope. A node killed three times with the budget
    reset each time would spend its whole allowance three times, so it is kept beside the run.
    """

    requests_used: int = 0
    requests_allowed: int | None = None

    def __post_init__(self) -> None:
        # A negative count is not a smaller allowance, it is a corrupted file — and treating it as an
        # allowance would hand back a budget nobody granted.
        if self.requests_used < 0:
            raise InvalidReference(f"a budget cannot have spent {self.requests_used} requests")
        if self.requests_allowed is not None and self.requests_allowed < 0:
            raise InvalidReference(f"a budget cannot allow {self.requests_allowed} requests")

    @property
    def remaining(self) -> int | None:
        return (max(self.requests_allowed - self.requests_used, 0)
                if self.requests_allowed is not None else None)

    def at_most(self, other: Budget) -> Budget:
        """The **larger** of two accounts of what has been spent, and the smaller allowance.

        §38: a reference must not be able to hand back an allowance that a control directory already
        says was used. A caller replaying an old token would otherwise reset the budget it had spent,
        which is the one thing a persisted budget exists to prevent.
        """
        return Budget(requests_used=max(self.requests_used, other.requests_used),
                      requests_allowed=min((v for v in
                          (self.requests_allowed, other.requests_allowed) if v is not None),
                          default=None))

    def after(self, more: int) -> Budget:
        return Budget(requests_used=self.requests_used + more,
                      requests_allowed=self.requests_allowed)


@dataclass(frozen=True)
class RecoveryRef:
    """An opaque token naming one node execution, its framework run, and the store it lives in.

    Opaque on purpose: §19 asks for a reference the Graph can carry without knowing what is in it, and
    the fields are Anchor's business — the graph must not learn to read framework messages or pick a
    checkpoint. `node` is the **logical** node execution and `run` the framework's per-attempt id, which
    is what keeps a retry from being filed as the same thing as the attempt before it.
    """

    node: str
    run: str
    store: str
    #: The workspace this attempt was working in. Carried so a reference can be checked against the
    #: **request that presents it**, not only against itself: a token that decodes and names a real run is
    #: still the wrong token if it was made for another node, another workspace, or another store.
    workspace: str = ""
    budget: Budget = field(default_factory=Budget)
    version: int = REFERENCE_VERSION

    def encode(self) -> str:
        body = json.dumps(asdict(self), sort_keys=True, separators=(",", ":"))
        return PREFIX + base64.urlsafe_b64encode(body.encode("utf-8")).decode("ascii")

    @staticmethod
    def decode(token: str) -> RecoveryRef:
        """Read a token back, or refuse it.

        **Refused by name, not repaired.** A truncated or edited token that was coerced into something
        usable would point at a run that is not the one the caller meant — and a recovery that resumes
        the wrong run is worse than a recovery that says it cannot.
        """
        if not token.startswith(PREFIX):
            raise InvalidReference(f"not an Anchor recovery reference: {token[:24]!r}")
        try:
            raw = base64.urlsafe_b64decode(token[len(PREFIX):].encode("ascii"))
            payload = json.loads(raw)
        except (binascii.Error, UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise InvalidReference(f"the recovery reference is corrupt: {exc}") from exc
        if not isinstance(payload, dict):
            raise InvalidReference("the recovery reference is not an object")
        if payload.get("version") != REFERENCE_VERSION:
            raise InvalidReference(
                f"reference version {payload.get('version')!r}, this build reads "
                f"{REFERENCE_VERSION} — refusing rather than guessing at the fields")
        try:
            budget = Budget(**payload.get("budget") or {})
            return RecoveryRef(node=str(payload["node"]), run=str(payload["run"]),
                               store=str(payload["store"]), budget=budget)
        except (KeyError, TypeError) as exc:
            raise InvalidReference(f"the recovery reference is missing a field: {exc}") from exc

    def digest(self) -> str:
        """A short, stable name for this run, for evidence lines and for the store's own directory."""
        return hashlib.sha256(f"{self.node}\x00{self.run}".encode("utf-8")).hexdigest()[:16]


class InvalidReference(RuntimeError):
    """The token does not name a run this build can look at. A failure with an explanation."""


@dataclass(frozen=True)
class Verdict:
    """What may be done, and the evidence that says so.

    The evidence is carried rather than logged because a verdict without it is an assertion: the
    difference between "uncertain" and "replayable" is a handful of ledger entries, and a reader has to
    be able to see which ones.
    """

    action: Action
    because: str
    effects: tuple[tuple[str, str, str], ...] = ()
    """(tool_call_id, tool_name, status) for every effect this run recorded, in order."""
    events: tuple[str, ...] = ()
    snapshot: str = ""
    budget: Budget = field(default_factory=Budget)

    @property
    def safe_to_replay(self) -> bool:
        return self.action == "replayable"

    def lines(self) -> list[str]:
        out = [f"{self.action}: {self.because}"]
        if self.effects:
            out.append("  effects:")
            out.extend(f"    {call_id} {name} = {status}" for call_id, name, status in self.effects)
        if self.events:
            out.append("  events: " + ", ".join(self.events[:EVIDENCE_LINES]))
        if self.snapshot:
            out.append(f"  snapshot: {self.snapshot}")
        out.append(f"  budget: {self.budget.requests_used}/{self.budget.requests_allowed} used")
        return out


def verify(ref: RecoveryRef, request_node: str, request_workspace: Path | None,
           configured_store: Path | None) -> str:
    """Whether a reference belongs to the request presenting it. Returns the reason it does not, or `""`.

    **Self-consistency is not identity.** R1's checking of a token was internal — does it decode, does it
    name a run — and a token made for a different node, a different workspace, or a different control
    directory passes all of that. §65 asks for the binding to the request, and this is it.

    The store check is the one that matters most operationally: a caller that configured one control
    directory and presented a reference to another would otherwise be reading and writing two different
    records of the same work.
    """
    if ref.node != request_node:
        return (f"the reference is for node {ref.node!r}, not for the {request_node!r} that presented it")
    if ref.workspace and request_workspace is not None:
        if str(Path(ref.workspace).resolve()) != str(Path(request_workspace).resolve()):
            return (f"the reference is for workspace {ref.workspace!r}, not for the one this request "
                    f"is running in")
    if configured_store is not None:
        if str(Path(ref.store).resolve()) != str(Path(configured_store).resolve()):
            return (f"the reference names the store {ref.store!r}, not the control directory this request "
                    f"configured")
    return ""


def open_store(control: Path) -> FileStepStore:
    """The store, in a directory the node cannot write.

    §17: the database and its records live in a control directory, not in the workspace. A node that can
    edit the ledger of its own side effects can make the ledger say the side effect never happened.
    """
    from pydantic_ai_harness.step_persistence import FileStepStore

    control = Path(control)
    control.mkdir(parents=True, exist_ok=True)
    return FileStepStore(directory=control / "steps")


def budget_path(control: Path) -> Path:
    return Path(control) / "budget.json"


def save_budget(control: Path, budget: Budget) -> None:
    """Write the allowance down, atomically.

    Replaced by rename rather than truncated in place: a kill between the truncate and the write leaves
    a budget file that says nothing, and the next run would start with a full allowance — which is the
    failure this exists to prevent.
    """
    path = budget_path(control)
    # Made here rather than assumed: a caller that has not written anything yet has no control
    # directory, and a budget that cannot be recorded is worse than one that is recorded late.
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".writing")
    temporary.write_text(json.dumps(asdict(budget), sort_keys=True), encoding="utf-8")
    temporary.replace(path)


def load_budget(control: Path) -> Budget:
    """The allowance as it was, or a full one when there is nothing to read.

    A missing file means this execution has not been killed yet; a corrupt one is **not** silently
    replaced with a full allowance, because that is the same as forgetting what was spent.
    """
    path = budget_path(control)
    if not path.exists():
        return Budget()
    try:
        return Budget(**json.loads(path.read_text(encoding="utf-8")))
    except (json.JSONDecodeError, TypeError) as exc:
        raise InvalidReference(f"the saved budget is unreadable: {exc}") from exc


async def assess(store: FileStepStore, ref: RecoveryRef) -> Verdict:
    """Decide which of the four things is true, from what the framework recorded.

    The order of the checks is the safety property, so it is worth reading as one: **anything that
    might have happened is settled before anything that could repeat it**. A run with an unresolved
    effect is `uncertain` even if it also has a usable snapshot, because continuing from that snapshot
    is continuing past a call whose effect is unknown — the framework's own documentation says an
    interrupted snapshot may re-execute a pending tool call, and this is where that is not allowed to be
    a surprise.
    """
    try:
        run = await store.get_run(run_id=ref.run)
    except (LookupError, FileNotFoundError, ValueError, TypeError) as exc:
        # A store whose files are unreadable is a refusal with a reason, not a traceback out of the
        # entry point — and never a quiet decision to start the task again.
        return Verdict("invalid", f"the store cannot be read for {ref.run!r}: {exc}",
                       budget=ref.budget)
    if run is None:
        return Verdict("invalid", f"no run {ref.run!r} in this store — nothing to recover from",
                       budget=ref.budget)
    # **The reference has to belong to the run it names.** A token that decodes and points at a real run
    # is not thereby a token for *this* node: the same store holds every attempt of every node, and a
    # mismatched reference would resume somebody else's work. `§38` asks for the binding, and this is it.
    if getattr(run, "agent_name", ref.node) != ref.node:
        return Verdict(
            "invalid",
            f"run {ref.run!r} belongs to {run.agent_name!r}, not to {ref.node!r} — a reference that "
            f"does not name its own run is refused rather than used.",
            budget=ref.budget)

    # **A store whose files are broken is a refusal with a reason.** Corrupt events or a corrupt
    # snapshot raise inside the backend — `ValidationError`, in the pinned version — and letting that out
    # of here would take it out of the entry point too, where the contract promises a status. Found by
    # breaking the files on purpose: two of the three leaked.
    try:
        events = await store.list_events(run_id=ref.run)
        kinds = tuple(_kind(event) for event in events)
        effects = await store.list_unresolved_tool_effects(run_id=ref.run)
        all_effects = await _every_effect(store, ref.run)
        snapshot = await store.latest_snapshot(run_id=ref.run)
    except Exception as exc:                                  # noqa: BLE001 - explained, not raised
        return Verdict(
            "invalid",
            f"the records for {ref.run!r} cannot be read ({type(exc).__name__}: {exc}) — refusing "
            f"rather than starting the task again over a broken checkpoint",
            budget=ref.budget)
    where = f"snapshot step {getattr(snapshot, 'step_index', '?')} ({getattr(snapshot, 'state', '?')})"

    unresolved = tuple((item.tool_call_id, item.tool_name, item.status) for item in effects)
    every = tuple((call_id, name, status) for call_id, name, status in all_effects)

    # ── 1. Something may have happened and the framework cannot say whether it did. ──
    if unresolved:
        return Verdict(
            "uncertain",
            f"{len(unresolved)} tool call(s) started and never reached a terminal state: the effect "
            f"may or may not have happened. Not replaying — the framework's own note says a started "
            f"call with no terminal update is unknown_after_crash.",
            effects=every, events=kinds,
            snapshot=where if snapshot else "", budget=ref.budget)

    failed = [item for item in every if item[2] == "failed"]
    if failed:
        # A failure is not permission: the command may have done half its work before it failed.
        return Verdict(
            "uncertain",
            f"{len(failed)} tool call(s) failed, and a failure says the command returned non-zero, not "
            f"that it changed nothing. Not replaying on that evidence alone.",
            effects=every, events=kinds,
            snapshot=where if snapshot else "", budget=ref.budget)

    # ── 2. Nothing entered a tool. Running the work once is running it once. ──
    started = [kind for kind in kinds if kind == "tool_call_started"]
    if not started:
        if "model_request_completed" in kinds:
            return Verdict(
                "replayable",
                "the model asked for something and no tool call ever began, so nothing has run yet; "
                "the request may be made once.",
                effects=every, events=kinds, budget=ref.budget)
        return Verdict(
            "uncertain",
            "the run stopped before any model request completed, so what it was about to do is not "
            "recorded anywhere. Nothing to replay from.",
            effects=every, events=kinds, budget=ref.budget)

    # ── 3. It already submitted. Nothing to resume, and asking again is the thing to avoid. ──
    #
    # **The recorded fact, not the history's text.** The history holds the observation the model saw, and
    # the marker appears in it whether or not the protocol accepted it: a command that printed the marker
    # and exited 1 was reported as finished here, with the refusal as its submission.
    try:
        fact = read_completion_fact(Path(ref.store), ref.node)
    except InvalidReference as exc:
        return Verdict("invalid", str(exc), effects=every, events=kinds, snapshot=where,
                       budget=ref.budget)
    if fact is not None:
        return Verdict(
            "finished",
            f"the completion protocol accepted a {fact.kind} for this node in run {fact.run!r}, so the "
            f"work finished: resuming would ask the model again and could run the submission a second "
            f"time. Read the result rather than restarting.",
            effects=every, events=kinds, snapshot=where, budget=ref.budget)

    # ── 4. Everything settled. Continue only if the snapshot actually covers it. ──
    #
    # **"A complete snapshot exists" is not enough.** The framework writes a tool's terminal record in
    # `after_tool_execute` and the snapshot in `after_node_run` — two hooks, two writes, not one step
    # (`step_persistence/_capability.py`, the pinned 0.32.0). So a kill between them leaves a settled
    # effect that the newest complete snapshot does not contain, and continuing from that snapshot
    # **re-runs the command whose effect already happened**. Found by the G2 plan refusing to accept the
    # earlier "that window does not exist" reading, and it does exist; it is reached by registering the
    # barrier before `StepPersistence`, whose hooks then run first.
    if snapshot is not None and getattr(snapshot, "state", None) == "complete":
        covered = _covers(snapshot, every)
        if covered:
            return Verdict(
                "continuable",
                "every tool call reached a terminal state and the newest complete snapshot contains the "
                "result of each one; continue from it without repeating anything.",
                effects=every, events=kinds, snapshot=where, budget=ref.budget)
        missing = [call_id for call_id in _settled_ids(every) if call_id not in _snapshot_ids(snapshot)]
        return Verdict(
            "uncertain",
            f"{len(missing)} settled tool call(s) are not covered by the newest complete snapshot "
            f"(step {getattr(snapshot, 'step_index', '?')}): the snapshot predates an effect that has "
            f"already happened, so continuing from it would repeat that command. Not replaying.",
            effects=every, events=kinds, snapshot=where, budget=ref.budget)

    return Verdict(
        "uncertain",
        "every tool call settled, but no complete snapshot was captured — there is no point the work "
        "can be picked up from without deciding afresh what has already been done. Not replaying.",
        effects=every, events=kinds,
        snapshot=where if snapshot else "", budget=ref.budget)


async def continued_messages(store: FileStepStore, ref: RecoveryRef) -> list[Any]:
    """The history to carry on from, or a refusal.

    Only a `complete` snapshot: an `interrupted` one is sendable but may re-execute a pending call, and
    this package does not re-execute what it cannot account for. The caller is expected to have read the
    verdict first — this does not decide, it fetches.
    """
    from pydantic_ai_harness.step_persistence import continue_run
    try:
        return await continue_run(store, run_id=ref.run)
    except LookupError as exc:
        raise InvalidReference(
            f"{ref.run!r} has no complete snapshot to continue from: {exc}") from exc


@dataclass(frozen=True)
class CompletionFact:
    """**The completion the normal protocol accepted**, written down as a fact rather than as text.

    Exists because recovery cannot re-derive this. A snapshot's history holds a *rendered observation* —
    `<returncode>1</returncode><output>COMPLETE_TASK…</output>` — and a scanner looking for the marker in
    it cannot tell an accepted completion from a command that printed the marker and then failed. It did
    not: a command that printed the marker and exited 1, which `read_completion` refuses and the model is
    told about, was reported by recovery as `finished` with the refusal text as its submission.

    So the fact is recorded **where the protocol accepts it** (`agent_runtime`'s bash tool) and read
    back whole. One implementation of the protocol, and recovery reads its output rather than guessing at
    it from a string.
    """

    node: str
    run: str
    kind: str
    submission: str
    route: str | None
    command: str
    at: str


def completion_path(control: Path) -> Path:
    """Where an accepted completion is written: beside the step store, outside the node's workspace."""
    return Path(control) / "completion.json"


def record_completion(control: Path, fact: CompletionFact) -> None:
    """Write the accepted completion down, atomically, before anything else can happen.

    Called from inside the tool, at the instant `read_completion` says yes — which is what makes it
    durable before a kill can land. A node cannot write here: it is the control directory (§17).
    """
    path = completion_path(control)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".writing")
    temporary.write_text(json.dumps(asdict(fact), sort_keys=True), encoding="utf-8")
    temporary.replace(path)


def read_completion_fact(control: Path, node: str) -> CompletionFact | None:
    """The accepted completion for a logical node, if the protocol recorded one.

    Returns `None` when there is no fact, and raises `InvalidReference` when the file is unreadable: a
    completion that cannot be read is not the same as one that never happened.
    """
    path = completion_path(control)
    if not path.exists():
        return None
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise InvalidReference(f"the recorded completion cannot be read: {exc}") from exc
    try:
        fact = CompletionFact(**payload)
    except TypeError as exc:
        raise InvalidReference(f"the recorded completion is not a completion: {exc}") from exc
    # **Bound to the node it names.** One control directory holds one node's record, but the check is
    # what stops a fact being read under the wrong identity — the same reason references carry a node.
    return fact if fact.node == node else None


def charge_request(control: Path, allowed: int | None = None) -> Budget:
    """**One more request, written down before it is made.** The only writer of `requests_used`.

    B5's failure was two writers: the adapter recorded `max(my count, what is on disk)` while the
    summariser incremented `what is on disk + 1`. Depending on how the two interleaved the file came out
    under one total or over the other — 13 against a real 22, and 24 against a real 19, measured both
    ways. Neither is a count of anything.

    So the rule is one sentence: **every request from any kind charges one, exactly once, before it is
    sent, by incrementing what is on disk.** A process that dies mid-request has already paid, which is
    what the old `max` was trying to achieve and could not do consistently. The allowance is merged the
    other way — smaller wins — so a replayed reference or a caller asking for more cannot buy turns.

    Charging is not optional: a failure here propagates, because the record is the only thing between a
    spent allowance and the next request.
    """
    here = Path(control)
    on_disk = load_budget(here)
    merged = min((value for value in (allowed, on_disk.requests_allowed)
                  if value is not None), default=None)
    charged = Budget(requests_used=on_disk.requests_used + 1, requests_allowed=merged)
    save_budget(here, charged)
    return charged


def already_finished(control: Path, node: str) -> tuple[str, str | None]:
    """What an attempt that already submitted produced: `(submission, route)`.

    From the **recorded fact**, never from the history's text. A caller handed a `finished` verdict has a
    result already and needs no model call to collect it, which is the point: the submission is the one
    action that must not happen twice — and handing back a refused command's output as if it were a
    submission is the other half of the same mistake.
    """
    fact = read_completion_fact(Path(control), node)
    if fact is None:
        return "", None
    return fact.submission, fact.route


def _settled_ids(effects: tuple[tuple[str, str, str], ...]) -> list[str]:
    """The calls that finished — the ones a snapshot has to contain for it to be a place to continue."""
    return [call_id for call_id, _, status in effects if status == "completed"]


def _snapshot_ids(snapshot: Any) -> set[str]:
    """Every tool call the snapshot's history has a result for.

    Read out of the messages rather than inferred from the snapshot's step index: a step number says
    when it was taken, and what has to be shown is *which* calls it already accounts for.
    """
    found: set[str] = set()
    for message in getattr(snapshot, "messages", ()) or ():
        for part in getattr(message, "parts", ()) or ():
            if getattr(part, "part_kind", "") == "tool-return":
                call_id = getattr(part, "tool_call_id", None)
                if call_id:
                    found.add(call_id)
    return found


def _covers(snapshot: Any, effects: tuple[tuple[str, str, str], ...]) -> bool:
    """Whether the snapshot accounts for every call that has finished."""
    settled = _settled_ids(effects)
    return not settled or set(settled) <= _snapshot_ids(snapshot)


def _kind(event: Any) -> str:
    """`StepEvent.kind` is already a string — the type is a `Literal`, not an enum with a value."""
    return str(getattr(event, "kind", ""))


async def _every_effect(store: FileStepStore, run_id: str) -> list[tuple[str, str, str]]:
    """Every effect the run recorded, resolved or not.

    Read from the events' own fields, which is where the framework puts them: `StepEvent` carries
    `tool_call_id`, `tool_name` and `kind` directly. `list_unresolved_tool_effects` answers one question;
    a verdict needs the whole picture, because "three calls, two completed and one started" reads very
    differently from "one call, started".
    """
    seen: dict[str, tuple[str, str, str]] = {}
    for event in await store.list_events(run_id=run_id):
        call_id = getattr(event, "tool_call_id", None)
        if not call_id:
            continue
        status = _status_from(_kind(event))
        if status not in ("started", "completed", "failed"):
            continue
        name = str(getattr(event, "tool_name", "?"))
        previous = seen.get(call_id)
        # Terminal wins over started, whatever order the events arrive in — a run that started and then
        # completed one call has one resolved effect, not two.
        if previous is None or previous[2] == "started":
            seen[call_id] = (call_id, name, status)
    return list(seen.values())


def _status_from(kind: str) -> str:
    if kind.endswith("_completed"):
        return "completed"
    if kind.endswith("_failed"):
        return "failed"
    if kind.endswith("_started"):
        return "started"
    return kind
