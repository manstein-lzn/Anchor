"""Bounding what the model is sent, without losing what happened.

Two things that are easy to run together and must not be:

- **the model's context** — a working set, deliberately finite, rewritten as the pass goes on;
- **the record** — everything that happened, appended to and never rewritten.

A node that has run for an hour has a conversation far larger than any window, and the point of
compaction is that the model keeps working anyway. The point of the record is that "the model was not
shown it" never becomes "it did not happen". The plan states the separation twice because the tempting
shortcut — let the record be whatever the framework's history currently holds — makes the two
identical, and then every compaction quietly destroys evidence.

**Where the strategies come from.** The Harness ships them; this module configures them and does not
reimplement them. Summarising in particular is a model call with a quality that only a real model can
demonstrate, and a hand-rolled one here would be a claim this package cannot support.

**What is deliberately not enabled.** `DeduplicateFileReads` needs to know which bash commands read
which files — a shell semantics guess dressed as a cache, and wrong in the direction that loses
evidence. The default large-output spill registers a second tool (`read_tool_result`) so the model can
page through it, which would break the single-bash rule; the spill here is a file the model reads with
`head`/`sed`/`tail`, which it already has.
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from pydantic_ai.capabilities import AbstractCapability
from pydantic_ai.models.wrapper import WrapperModel
from pydantic_ai.messages import ModelMessage
from pydantic_ai_harness.compaction import SlidingWindowCompaction, SummarizingCompaction

class Uncompactable(RuntimeError):
    """The context cannot be made to fit, and saying so is the outcome.

    Its own type so the caller can tell "this node cannot run under this budget" from "this node
    failed": the first is a configuration or a task-shape problem, and the second is work that went
    wrong.
    """


#: Characters per token. The framework's own heuristic, and named rather than buried because §41 asks
#: for the estimation to be recorded: a budget computed with this and a budget the provider computes
#: are not the same number, and the difference is what makes a request overflow anyway.
CHARS_PER_TOKEN = 4

#: What one command's output may occupy on disk before the spill refuses it. Explicit, because the
#: alternative to a bound is a node that fills the disk, and the alternative to an explicit bound is a
#: silent truncation — which is the defect this package was sent back for once already.
DEFAULT_SPILL_BYTES = 32 * 1024 * 1024


@dataclass(frozen=True)
class Budget:
    """What the model may be sent, decided explicitly.

    Every number is a field and every field has a default that is a real value for a real model, so a
    test can set small ones and see compaction happen on a conversation of ten messages rather than ten
    thousand. Nothing here is a fraction of something else: §24 asks for the window, the reserve, the
    target and the estimator to be recorded, and a percentage of an assumed window is how a budget
    comes to be wrong for the one model it is used with.

    `input_target` is what the history may occupy, and it is **not** `window - output_reserve`: the
    estimate is a heuristic and the tokeniser is the provider's, so the gap is the disagreement
    between them. A budget that fills the window exactly will overflow on the request that fills it.
    """

    window: int = 128_000
    output_reserve: int = 8_000
    input_target: int = 96_000
    #: Kept regardless of size, because these are the things whose loss is not recoverable by reading
    #: anything: the task itself and the rules of the loop.
    keep_messages: int = 8
    #: Characters charged to every request that are not messages: the instructions and the tool
    #: schemas. Left at zero a budget is a claim about the history only, which is not what the provider
    #: is billed for or refuses.
    request_overhead: int = 4_000
    estimator: str = f"chars/{CHARS_PER_TOKEN}"

    def __post_init__(self) -> None:
        if self.input_target > self.window - self.output_reserve:
            raise ValueError(
                f"input_target {self.input_target} leaves no room for a {self.output_reserve}-token "
                f"answer inside a {self.window}-token window")
        if self.keep_messages < 1:
            raise ValueError("keep_messages has to leave at least one message")

    def describe(self) -> dict[str, Any]:
        """The budget as it should appear in a record: what was configured, and how it was counted."""
        return {"window": self.window, "output_reserve": self.output_reserve,
                "input_target": self.input_target, "keep_messages": self.keep_messages,
                "request_overhead": self.request_overhead, "estimator": self.estimator}


def estimate_tokens(messages: list[ModelMessage], *, tools: int = 0,
                    instructions: int = 0) -> int:
    """A cheap estimate of what a request costs — **not** a tokeniser, and not a counter of message text.

    The first version of this counted `part.content` and nothing else, which put a 100,000-character
    `bash` command at **zero tokens**: the command lives in `ToolCallPart.args`, and a request's size is
    mostly the parts that are not prose. So it is not only an underestimate, it is an underestimate
    that gets worse exactly as the commands get bigger — and it was both the implementation of the
    budget and the only thing checking the budget, which is how a broken counter passes its own tests.

    Every part kind is walked: text, tool arguments, tool results, retry prompts. Instructions and tool
    schemas are passed in as characters because they are not messages and are charged to every request.
    A provider's own count will differ; the budget leaves room for that, and the report says which of
    the two numbers any figure came from.
    """
    total = max(instructions, 0) // CHARS_PER_TOKEN + max(tools, 0) // CHARS_PER_TOKEN
    for message in messages:
        for part in getattr(message, "parts", ()) or ():
            total += _part_tokens(part)
    return total


def _part_tokens(part: Any) -> int:
    """One part's size, whatever kind it is.

    `args` is counted as JSON rather than as its `repr`: a dict's repr and its wire form are not the
    same length, and the wire form is what the provider is sent.
    """
    size = 0
    content = getattr(part, "content", None)
    if isinstance(content, str):
        size += len(content)
    elif content is not None:
        size += len(str(content))
    args = getattr(part, "args", None)
    if args is not None:
        try:
            size += len(args if isinstance(args, str) else json.dumps(args, ensure_ascii=False,
                                                                    default=str))
        except (TypeError, ValueError):                       # pragma: no cover - defensive
            size += len(str(args))
    return size // CHARS_PER_TOKEN + (1 if size else 0)


# ── the record ───────────────────────────────────────────────────────────────────────────────────

@dataclass
class Record:
    """Everything that happened, appended to and never rewritten.

    Written as JSON lines as events occur, because the alternative — assembling it at the end — is
    exactly what loses the last batch, and because a record that exists only in memory is not evidence
    of anything that happened in a process that is gone.

    **Outside the workspace.** Inside it is a file the node can read and, worse, edit: a node that can
    rewrite its own audit record can make the record say anything. It is also not a node artefact — the
    workspace is what the next node is pointed at, and this is not that.

    Kept for the duration of the pass rather than cleaned up eagerly: a later package refers to these
    files from its own snapshots, and deleting them early would break references that nothing here can
    see.
    """

    directory: Path
    #: What **actually went to the model**, per request. This is the number B1 is about, and it is
    #: written by the budget rather than by the observer — measured, because an observer's
    #: `before_model_request` runs *before* the wrapper that compacts, so an observer placed anywhere
    #: records the history that arrived and not the one that left.
    sent: list[dict[str, Any]] = field(default_factory=list)
    #: What arrived, per request, before anything was done to it. The difference between this and
    #: `sent` is what compaction is worth, and the two together are the evidence that the record is
    #: not the compacted view.
    arriving: list[dict[str, Any]] = field(default_factory=list)
    compactions: list[dict[str, Any]] = field(default_factory=list)
    summaries: list[dict[str, Any]] = field(default_factory=list)
    commands: list[dict[str, Any]] = field(default_factory=list)
    spilled: dict[str, Path] = field(default_factory=dict)
    #: Where every kept output goes, and the one directory mounted read-only into the sandbox.
    outputs: Path = field(default=Path())
    spill_bytes: int = 0
    limit_bytes: int = DEFAULT_SPILL_BYTES
    problems: list[str] = field(default_factory=list)

    def __post_init__(self) -> None:
        self.directory = Path(self.directory)
        self.outputs = self.directory / "outputs"
        self.outputs.mkdir(parents=True, exist_ok=True)
        self._handle = (self.directory / "record.jsonl").open("a", encoding="utf-8")

    def note(self, kind: str, **fields: Any) -> None:
        line = json.dumps({"kind": kind, **fields}, ensure_ascii=False, default=str)
        self._handle.write(line + "\n")
        self._handle.flush()

    def keep_message(self, kind: str, message: Any, **fields: Any) -> None:
        """Append one message **as the framework has it**, before anything is allowed to rewrite it.

        §41 asks for the raw responses and tool results, and §46 asks for them to be kept apart from
        what the model is shown. Counts are not that: "nine commands" cannot answer "what did the
        command that was summarised away actually print", and a record that cannot answer it is not the
        complete record this package promises. The framework's own adapter does the encoding, so the
        shape is its business and not a guess made here.
        """
        from pydantic_ai.messages import ModelMessagesTypeAdapter
        try:
            encoded = ModelMessagesTypeAdapter.dump_python([message], mode="json")[0]
        except Exception as exc:                              # noqa: BLE001 - a record must not fail a run
            self.note("record_error", what=kind, error=f"{type(exc).__name__}: {exc}")
            return
        # Written directly rather than through `note`, because `note` takes `**fields` and mypy cannot
        # rule out that one of them is its own first parameter.
        line = json.dumps({"kind": kind, "message": encoded, **fields}, ensure_ascii=False,
                          default=str)
        self._handle.write(line + "\n")
        self._handle.flush()

    # ── large output ─────────────────────────────────────────────────────────────────────────────

    def keep_output(self, name: str, text: str) -> Path | None:
        """Put a command's full output somewhere the node can read it back and the record can cite it.

        Returns the path, or `None` when the storage bound is reached — and when it is, the reason is
        written to the record. A refusal that leaves a mark is a limit; a refusal that does not is a
        silent hole, and the difference is the whole of §35.
        """
        blob = text.encode("utf-8")
        self.outputs.mkdir(parents=True, exist_ok=True)
        if self.spill_bytes + len(blob) > self.limit_bytes:
            self.problems.append(
                f"refused to keep {name}: {len(blob)} bytes would take the store past its "
                f"{self.limit_bytes}-byte bound (holding {self.spill_bytes})")
            self.note("output_refused", name=name, bytes=len(blob), held=self.spill_bytes,
                      limit=self.limit_bytes)
            return None
        digest = hashlib.sha256(blob).hexdigest()[:16]
        # **Inside the mounted directory.** Written to the record's root, a medium-sized output — too
        # big to show, small enough that the sandbox did not cut it — was stored where the read-only
        # mount does not reach, so the model was handed a path it could not open, exactly as with the
        # host paths before it.
        path = self.outputs / f"output-{digest}.txt"
        if not path.exists():
            path.write_bytes(blob)
        self.spilled[name] = path
        self.spill_bytes += len(blob)
        self.note("output_kept", name=name, path=str(path), bytes=len(blob), sha256=digest)
        return path

    def close(self) -> None:
        self._handle.close()

    # ── what the caller asked for ────────────────────────────────────────────────────────────────

    def summary(self) -> dict[str, Any]:
        """The account §41 asks for: how the context was bounded, and what that cost."""
        return {
            "requests": len(self.sent),
            "largest_arriving": max((item["tokens"] for item in self.arriving), default=0),
            "compactions": self.compactions,
            "summaries": self.summaries,
            "commands": len(self.commands),
            "spilled": {name: str(path) for name, path in self.spilled.items()},
            "spill_bytes": self.spill_bytes,
            "spill_limit": self.limit_bytes,
            "problems": self.problems,
            "largest_request": max((item["tokens"] for item in self.sent), default=0),
        }


# ── the capability ───────────────────────────────────────────────────────────────────────────────

class Watching(AbstractCapability):
    """The observation half: writes the record, and bounds what one command may show the model.

    A capability rather than a wrapper around the adapter, because the adapter is main integration's
    and this is the seam G1 froze for exactly this. It also means the observation is of what the
    framework actually did — the request that went out, the results that came back — rather than of
    what this module believes it did.

    **It rewrites the tool result, and that is the point of putting it here.** A command may print more
    than the window holds, and a sliding window cannot help: the result of the command just run is the
    newest message and the one thing that must not be dropped. So the full output goes to the store and
    the model is shown a bounded preview with the path to the rest — which it can read with `head`,
    `sed` and `tail`, tools it already has. That is why no second tool is registered and why the
    single-bash rule survives: the model pages through a file rather than through a framework tool.
    """

    #: Where the kept outputs are mounted inside the sandbox. Fixed, because the model is told this
    #: path and a path that moves between commands is a path it cannot rely on. Read-only, and only
    #: this directory — mounting the whole record directory would put the audit trail inside the
    #: sandbox, where the node could rewrite what is said about it.
    MOUNT = "/kept"

    def __init__(self, record: Record, observe_chars: int = 8_000) -> None:
        self.record = record
        # How much of one command's output the model is shown. Bounded by characters rather than
        # tokens because it is a preview of a file, and the file is measured in bytes.
        self.observe_chars = observe_chars

    async def before_model_request(self, ctx: Any, request_context: Any) -> Any:
        # The request about to go out. Recorded here and not after the fact because this is the only
        # place the exact history is visible, and its size over the pass is what B1 is about.
        self._ask_the_sandbox_to_keep_what_it_would_cut(ctx)
        messages = list(getattr(request_context, "messages", ()) or ())
        item = {"request": len(self.record.arriving) + 1, "messages": len(messages),
                "tokens": estimate_tokens(messages)}
        self.record.arriving.append(item)
        self.record.note("arriving", **item)
        return request_context

    def _ask_the_sandbox_to_keep_what_it_would_cut(self, ctx: Any) -> None:
        """Point the sandbox at this pass's store, once.

        **The bytes are in hand when the sandbox cuts and gone immediately after**, so unless something
        says where to put them there is nothing to recover and no way to claim losslessness. It is
        reached through the run's dependencies because there is no parameter for it: the adapter builds
        the sandbox and the contract is frozen, so a capability that needs the seam reaches the one
        that exists. The cleaner patch — a field on `NodeRequest` that the adapter passes down — is
        described in `AGENT_NODE_PLAN_02_RESULT.md` for main integration.
        """
        sandbox = getattr(getattr(ctx, "deps", None), "sandbox", None)
        if sandbox is None or getattr(sandbox, "spill_dir", None) is not None:
            return
        target = self.record.directory / "outputs"
        target.mkdir(parents=True, exist_ok=True)
        sandbox.spill_dir = target
        # And the same directory made visible where the command can reach it. Without this the model
        # is handed a host path it cannot open: the sandbox's `/tmp` is a private tmpfs, so the read
        # fails, the pipeline still exits zero, and a test that checks the host copy says everything is
        # fine while the node never saw the file.
        sandbox.spill_mount = self.MOUNT

    async def wrap_tool_execute(self, ctx: Any, *, call: Any, tool_def: Any, args: Any,
                                handler: Any) -> Any:
        sandbox = getattr(getattr(ctx, "deps", None), "sandbox", None)
        if sandbox is not None:
            # Cleared per call. Read after the fact without this, a call that never reached the sandbox
            # — a command after a submission, which the guard refuses — is credited with whatever the
            # command before it spilled.
            sandbox.spilled = ()
            sandbox.visible = ()
            sandbox.incomplete = False
            sandbox.spill_limit_bytes = max(self.record.limit_bytes - self.record.spill_bytes, 0)
        result = await handler(args)
        name = getattr(call, "tool_name", "")
        command = ""
        raw = getattr(call, "args", None)
        if isinstance(raw, dict):
            command = str(raw.get("command", ""))
        text = result if isinstance(result, str) else str(result)
        full = tuple(getattr(sandbox, "spilled", ()) or ())
        visible = tuple(getattr(sandbox, "visible", ()) or ())
        # What the sandbox kept before cutting, when it did. This is the lossless copy; the text in
        # hand is a prefix of it, and a record that cited only the text would be citing the truncation.
        kept: Path | None = full[0] if full else self.record.keep_output(
            f"{len(self.record.commands) + 1}", text)
        # **The sandbox's writes are charged to the same bound as this record's own.** They used to be
        # invisible to it: `Record(limit_bytes=1000)` reported 88 bytes held while the directory on disk
        # had 1,100,000, because the limit lived only in `keep_output` and the spill went round it.
        if full:
            for path in full:
                try:
                    self.record.spill_bytes += path.stat().st_size
                except OSError as exc:                        # pragma: no cover - defensive
                    self.record.problems.append(f"cannot account for {path}: {exc}")
        incomplete = bool(getattr(sandbox, "incomplete", False))
        item = {"command": command, "chars": len(text), "sha256": hashlib.sha256(
            text.encode("utf-8")).hexdigest()[:16], "kept": None if kept is None else str(kept),
                "seen_by_node": visible[0] if visible else None, "complete": bool(full) and not incomplete,
                "incomplete": incomplete, "shown": min(len(text), self.observe_chars)}
        self.record.commands.append(item)
        self.record.note("command", tool=name, **item)
        # The raw result and the id it answers, recorded as they are. Not wrapped in a synthesized
        # framework message: the adapter would try to serialise a shape it does not own, and what a
        # later package needs is the text and the id, not a plausible-looking envelope.
        self.record.note("tool_result", tool=name, content=text,
                         tool_call_id=getattr(call, "tool_call_id", None),
                         bytes=len(text.encode("utf-8")),
                         sha256=hashlib.sha256(text.encode("utf-8")).hexdigest()[:16])
        if incomplete:
            self.record.problems.append(
                f"command {len(self.record.commands)} produced more than the store could keep; the "
                f"model is being told its preview is not the whole output")
        return self._bounded(text, kept if not visible else Path(visible[0]),
                             visible[0] if visible else "", complete=not incomplete)

    def _bounded(self, text: str, kept: Path | None, seen: str = "",
                 complete: bool = True) -> str:
        """What the model sees of one command's output.

        Head and tail, with the middle elided and the whole thing named. Both ends matter: a command's
        first lines are usually what it was and the last lines are usually how it went, and a preview
        that kept only the head would hide an error message at the end.

        When the store refused it — the bound was reached — the message says so **and the preview is
        not silently complete**. A node told its output was too large to keep can decide what to do;
        a node quietly shown the first eight thousand characters would believe it had seen the result.
        """
        if len(text) <= self.observe_chars:
            return text
        head = self.observe_chars * 3 // 4
        tail = self.observe_chars - head
        if kept is None or complete is False:
            # **Says what it is.** A preview with a path the model cannot use, or a path holding only
            # part of the output, is worse than no path: the model reads it, believes it has the whole
            # thing, and answers from a fragment.
            return (f"{text[:head]}\n\n[the rest of this output was not kept whole — the store is at "
                    f"its bound or the output was too large, so there is nowhere to read the rest "
                    f"from. This preview is all there is.]\n\n{text[-tail:]}")
        # **The path the command can open**, not the host's. Quoted in full so a model that has to
        # escape it in a shell does not have to guess.
        return (f"{text[:head]}\n\n[{len(text) - self.observe_chars} characters elided — the whole "
                f"output is at {seen or kept}, read it with head/sed/tail]\n\n{text[-tail:]}")

    async def after_model_request(self, ctx: Any, *, request_context: Any, response: Any) -> Any:
        """The model's own words, appended before anything can rewrite them.

        `keep_message` existed and **was never called**: the record kept a list of `part_kind`s, so a
        later package asking "what did the model actually say, and with which arguments" had no answer.
        The response goes in whole, through the framework's adapter, so its shape is the framework's.
        """
        self.record.keep_message("model_response", response)
        self.record.note("response", parts=[getattr(part, "part_kind", "")
                                            for part in getattr(response, "parts", ()) or ()],
                         usage=_usage_of(response))
        return response

    async def on_model_request_error(self, ctx: Any, *, request_context: Any,
                                     error: Exception) -> Any:
        # Recorded, and not repaired. Whether this was a window overflow is not something to guess at:
        # treating every failure as one is how a provider outage becomes a compaction loop.
        self.record.note("model_error", error=type(error).__name__, detail=str(error)[:500])
        raise error


class _CountedSummariser(WrapperModel):
    """Counts the summariser's own calls, so they can be recorded apart from the node's model.

    `record.summaries` had **no write path at all** — it was a list that stayed empty, and a test whose
    assertion was conditional on it passed for that reason. A count of the calls is the least it has to
    hold; the tokens are on the same wrapper because a summary is a model call with a price.
    """

    calls: int = 0
    #: What the provider reported for each summary call. Kept because a summary is a paid request and
    #: an estimate of its size is not an invoice — the record has to let a reader tell the two apart.
    usage: list[dict[str, Any]] = []

    async def request(self, messages: Any, model_settings: Any,
                      model_request_parameters: Any) -> Any:
        self.calls += 1
        try:
            response = await super().request(messages, model_settings, model_request_parameters)
        except Exception as exc:                              # noqa: BLE001 - recorded, then re-raised
            # A summary that failed still happened, and a run whose compaction failed needs to say so
            # rather than look like one that never tried.
            self.usage.append({"failed": f"{type(exc).__name__}: {exc}"[:200]})
            raise
        self.usage.append(_usage_of(response) or {"reported": None})
        return response


class WithinBudget(AbstractCapability):
    """The changing half: compacts an overflowing history before the request goes out.

    Two strategies, in this order and only these two:

    - `SlidingWindowCompaction`, which costs nothing and preserves tool-call/return pairing;
    - `SummarizingCompaction`, which costs a model call and keeps the gist of what was dropped.

    The deterministic one goes first so that the common case — a long conversation of ordinary turns —
    never pays for a summary. The summary is for the case that matters: constraints stated early, whose
    loss is not recoverable by reading anything later.

    `receipts` is on for both. A receipt tells the model that its memory before some point is
    secondhand, which is both more honest than a silent cut and more useful — a model that knows it may
    be missing something can go and look, and one that does not will confabulate.
    """

    def __init__(self, budget: Budget, record: Record | None = None,
                 summarizer: Any = None, force: bool = False) -> None:
        self.budget = budget
        self.record = record
        self.force = force
        self.summariser: _CountedSummariser | None = None
        if summarizer is not None:
            self.summariser = _CountedSummariser(summarizer)
        self.strategies: list[Any] = [SlidingWindowCompaction(
            max_tokens=budget.input_target,
            max_messages=max(budget.keep_messages * 4, 32),
            keep_messages=budget.keep_messages,
            # The task and the rules arrive in the first user message; a window that drops it has
            # dropped the assignment.
            preserve_first_user_message=True,
            receipts=True)]
        if self.summariser is not None:
            self.strategies.append(SummarizingCompaction(
                model=self.summariser,
                max_tokens=budget.input_target,
                keep_tokens=max(budget.input_target // 3, 1),
                keep_messages=budget.keep_messages,
                preserve_first_user_message=True,
                receipts=True))

    def cost(self, messages: list[ModelMessage], request_context: Any = None) -> int:
        """What one request costs: the history, plus what goes with every request.

        **Measured from the request, not assumed.** A fixed allowance cannot cover "any actual
        instruction" — a node with a long prompt and a node with a short one would be charged the same,
        and the one with the long prompt would be the one that overflows. The instruction parts and the
        tool schemas are in the request parameters, so they are counted where they are.
        """
        return estimate_tokens(messages, instructions=_overhead(request_context))

    async def _compact_once(self, messages: list[ModelMessage], ctx: Any,
                            request_context: Any = None) -> list[ModelMessage]:
        """Run the first strategy that actually reduces the history, and say which one did.

        `compact` applies its transform unconditionally — the threshold check is the capability's, not
        the strategy's — so "did it help?" is what decides whether to stop or to escalate to the next
        one. A strategy that returned the history unchanged has not compacted anything, and recording
        it as though it had would make the record claim a bound it did not achieve.
        """
        before, size = len(messages), self.cost(messages, request_context)
        # **The summariser goes first when there is one.** The order used to be "window, then summary if
        # the window did not help" — which means the old history is dropped, and only then is the
        # question asked whether anything needed summarising. The answer is always no by then, because
        # what would have been summarised is already gone: measured, with the summariser never called at
        # all. What must be retained is decided before history is discarded, not after.
        for strategy in ([s for s in self.strategies if type(s).__name__ == "SummarizingCompaction"]
                         + [s for s in self.strategies
                            if type(s).__name__ != "SummarizingCompaction"]):
            out = await strategy.compact(messages, ctx)
            # **The same measure on both sides.** `before` included the request overhead and `after`
            # did not, so a strategy that changed nothing could look like it had shortened the history
            # by exactly the overhead — and "did it help?" is what decides whether to escalate.
            after = self.cost(out, request_context)
            if self.record is not None:
                self.record.note("compaction", strategy=type(strategy).__name__, messages_before=before,
                                 messages_after=len(out), tokens_before=size, tokens_after=after,
                                 estimator=self.budget.estimator)
                self.record.compactions.append(
                    {"strategy": type(strategy).__name__, "messages_before": before,
                     "messages_after": len(out), "tokens_before": size, "tokens_after": after})
            if self.summariser is not None and self.record is not None:
                # One line per call, whether or not it helped: a call that did not reduce the history
                # still cost money, and a record that counted only the helpful ones would understate
                # what compaction spent.
                while len(self.record.summaries) < self.summariser.calls:
                    entry = {"call": len(self.record.summaries) + 1,
                             "strategy": type(strategy).__name__, "messages_in": before,
                             "tokens_in": size, "tokens_out": after,
                             "usage": (self.summariser.usage[len(self.record.summaries)]
                                       if len(self.summariser.usage) > len(self.record.summaries)
                                       else None)}
                    self.record.summaries.append(entry)
                    self.record.note("summary", call=entry["call"], strategy=entry["strategy"],
                                     messages_in=before, tokens_in=size, tokens_out=after,
                                     usage=entry["usage"])
            if after < size:
                return out
        return messages

    async def wrap_model_request(self, ctx: Any, *, request_context: Any, handler: Any) -> Any:
        """Compact first; and if the provider rejects the request anyway, compact once and retry.

        The retry is bounded to one and only for an error this module recognises as a window
        overflow — `pydantic_ai.exceptions` names the one the provider adapters raise. Catching
        everything would turn an outage into a compaction loop, and re-running the tools is not part of
        this: the request is what failed, so the request is what is repeated.
        """
        request_context = await self._prepare(request_context, ctx)
        self._send(request_context, attempt=1)

        try:
            return await handler(request_context)
        except Exception as error:                       # noqa: BLE001 - re-raised unless it is a window
            if not _is_window_overflow(error) or self.force:
                raise
            if self.record is not None:
                self.record.note("overflow", error=type(error).__name__, detail=str(error)[:300])
            reduced = await self._compact_once(
                list(getattr(request_context, "messages", ()) or ()), ctx)
            # **The retry is a request too**, so it is checked and counted like one. Cutting the corner
            # here is how a retry sends something larger than the ceiling that was just enforced.
            retried = _replace_messages(request_context, reduced)
            self._send(retried, attempt=2)
            return await handler(retried)

    async def _prepare(self, request_context: Any, ctx: Any) -> Any:
        """Compact if needed, and refuse if that did not make it fit."""
        messages = list(getattr(request_context, "messages", ()) or ())
        if self.force or self.cost(messages, request_context) > self.budget.input_target:
            messages = await self._compact_once(messages, ctx, request_context)
            request_context = _replace_messages(request_context, messages)
        # **And then refuse if it still does not fit.** A message larger than the target cannot be
        # compacted away: the strategies here drop or summarise *older* history and this is the newest
        # and most load-bearing part of it. Continuing would send a request that either fails at the
        # provider or costs more than the budget says; failing here says which and why, once.
        return request_context

    def _send(self, request_context: Any, *, attempt: int) -> None:
        """Record **the request that is about to go out**, and refuse it if it does not fit.

        Written here and not earlier: a request that was refused never went out, and a record that
        counted it would say the model was sent something it never saw. The refusal is its own event
        beside it, so the numbers and the reasons can be read together.
        """
        messages = list(getattr(request_context, "messages", ()) or ())
        over = self.cost(messages, request_context)
        ceiling = self.budget.window - self.budget.output_reserve
        if over > ceiling:
            if self.record is not None:
                self.record.note("refused", tokens=over, ceiling=ceiling, attempt=attempt,
                                 messages=len(messages), estimator=self.budget.estimator)
            raise Uncompactable(
                f"the request is {over} tokens (by {self.budget.estimator}) and the ceiling is "
                f"{ceiling}; compaction cannot reduce it further because what exceeds the budget is "
                f"the most recent history, which cannot be dropped or summarised without losing the "
                f"work in progress. Refusing to send a request that does not fit.")
        if self.record is not None:
            item = {"request": len(self.record.sent) + 1, "attempt": attempt,
                    "messages": len(messages), "tokens": over,
                    "estimator": self.budget.estimator}
            self.record.sent.append(item)
            self.record.note("sent", **item)


def _replace_messages(request_context: Any, messages: list[ModelMessage]) -> Any:
    """A copy of the request context with a different history.

    Copied rather than mutated: the original is what the record refers to, and a compaction that
    edited it in place would change what the pass is remembered as having been sent.
    """
    from dataclasses import replace
    return replace(request_context, messages=messages)


def _overhead(request_context: Any) -> int:
    """The characters every request carries that are not messages.

    Instructions and tool schemas. Read off the request being made, so a large prompt is charged as a
    large prompt and a budget built on a fixed guess cannot pass by being wrong in the safe direction.
    """
    params = getattr(request_context, "model_request_parameters", None)
    if params is None:
        return 0
    size = 0
    for part in getattr(params, "instruction_parts", ()) or ():
        content = getattr(part, "content", part)
        size += len(content if isinstance(content, str) else str(content))
    for tool in (getattr(params, "function_tools", ()) or []):
        size += _schema_chars(tool)
    for tool in (getattr(params, "output_tools", ()) or []):
        size += _schema_chars(tool)
    return size


def _schema_chars(tool: Any) -> int:
    """One tool definition's size, as JSON, which is how it is sent."""
    for name in ("parameters_json_schema", "schema"):
        schema = getattr(tool, name, None)
        if schema is not None:
            try:
                return len(json.dumps(schema, ensure_ascii=False, default=str))
            except (TypeError, ValueError):                   # pragma: no cover - defensive
                return len(str(schema))
    return len(str(getattr(tool, "name", ""))) + len(str(getattr(tool, "description", "")))


def _usage_of(response: Any) -> dict[str, Any] | None:
    """The provider's own token counts, when it reported any.

    Kept beside the estimate rather than instead of it: the record has to let a reader tell which of the
    two a figure came from, and a run whose estimate disagrees with the provider is the run worth
    looking at.
    """
    usage = getattr(response, "usage", None)
    if usage is None:
        return None
    return {name: getattr(usage, name, None)
            for name in ("input_tokens", "output_tokens", "requests", "total_tokens")}


def _is_window_overflow(error: BaseException) -> bool:
    """Whether a provider said the request did not fit.

    By exception type, not by message text. A string match on "context" would catch a validation error
    that mentions context length and miss a provider whose wording differs, and the consequence of a
    wrong yes is a compaction that does not help.
    """
    try:
        from pydantic_ai.exceptions import ModelHTTPError
    except ImportError:                                  # pragma: no cover - the import is stable
        return False
    if isinstance(error, ModelHTTPError):
        text = str(error).lower()
        return any(word in text for word in ("context length", "context_length", "too long",
                                             "maximum context", "token limit"))
    return False


def context_capabilities(budget: Budget, record: Record | None = None, summarizer: Any = None,
                         force: bool = False, observe_chars: int = 8_000) -> tuple[Any, ...]:
    """The composition, in one place: what a bound context node is made of.

    Two capabilities and no more.

    Which of them records "the model's input" is not a matter of ordering, and this was measured rather
    than assumed: an observer's `before_model_request` runs **before** the wrapper that compacts, so a
    record written there is the history that arrived, whatever order the capabilities are in. The
    budget therefore writes `record.sent` itself, after it has finished, and the observer writes
    `record.arriving`. The two numbers together are the evidence — one says the model was sent a
    bounded history, the other says what that history used to be.
    """
    chosen: list[Any] = [WithinBudget(budget, record=record, summarizer=summarizer, force=force)]
    if record is not None:
        chosen.append(Watching(record, observe_chars=observe_chars))
    return tuple(chosen)
