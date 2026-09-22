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
    invalid       the reference does not check out, so there is nothing to recover from and starting
                  fresh would silently discard whatever did happen.

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
from typing import Any, Literal

from pydantic_ai_harness.step_persistence import FileStepStore

#: The reference format's version, in the token itself. A token from an older Anchor is refused by name
#: rather than misread — the fields are not stable across versions and guessing at them is how a
#: recovery reference points at the wrong run.
REFERENCE_VERSION = 1
PREFIX = "anchor1."

#: What a caller may do, and nothing finer. The distinction between the last two is the whole point of
#: the package, so it is the type and not a comment.
Action = Literal["replayable", "uncertain", "continuable", "invalid"]

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
    requests_allowed: int = 0

    @property
    def remaining(self) -> int:
        return max(self.requests_allowed - self.requests_used, 0)

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


def open_store(control: Path) -> FileStepStore:
    """The store, in a directory the node cannot write.

    §17: the database and its records live in a control directory, not in the workspace. A node that can
    edit the ledger of its own side effects can make the ledger say the side effect never happened.
    """
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
    except LookupError:
        return Verdict("invalid", f"no run {ref.run!r} in this store — nothing to recover from",
                       budget=ref.budget)
    except FileNotFoundError as exc:
        return Verdict("invalid", f"the store cannot be read: {exc}", budget=ref.budget)
    if run is None:
        return Verdict("invalid", f"no run {ref.run!r} in this store — nothing to recover from",
                       budget=ref.budget)

    events = await store.list_events(run_id=ref.run)
    kinds = tuple(_kind(event) for event in events)
    effects = await store.list_unresolved_tool_effects(run_id=ref.run)
    all_effects = await _every_effect(store, ref.run)
    snapshot = await store.latest_snapshot(run_id=ref.run)
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

    # ── 3. Everything settled. Continue if there is a point to continue from. ──
    if snapshot is not None and getattr(snapshot, "state", None) == "complete":
        return Verdict(
            "continuable",
            "every tool call reached a terminal state and a complete snapshot exists; continue from it "
            "without repeating anything.",
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
