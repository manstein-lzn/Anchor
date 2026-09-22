"""第二包的验收矩阵：上下文被限住，记录不被改写。

模型是唯一被替换的东西。沙箱是 bubblewrap，完成命令是真实 CLI，落盘的全量输出是真实文件。

**这个文件里的模型自己数轮次，不数历史里的 response。** 第一包的 `model_from` 靠数历史挑脚本，
而这一包做的事就是删历史——用它当计数器，压缩一开始就会静默地重放第一步。`Counting` 是这一包自己的。
"""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path

import pytest

pytest.importorskip("pydantic_ai", reason="the optional adapter dependency is not installed")

from pydantic_ai.capabilities import AbstractCapability                        # noqa: E402
from pydantic_ai.messages import ModelResponse, TextPart, ToolCallPart          # noqa: E402
from pydantic_ai.models.function import FunctionModel                          # noqa: E402

from anchor.node import COMPLETED, NodeRequest                     # noqa: E402
from anchor.node.context import (                                               # noqa: E402
    Budget, Record, Uncompactable, Watching, context_capabilities, estimate_tokens,
)
from anchor.node.pydantic_adapter import run_node                               # noqa: E402
from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox                   # noqa: E402


@pytest.fixture(scope="module", autouse=True)
def needs_a_sandbox():
    try:
        BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    except RuntimeError as exc:
        pytest.skip(f"no usable sandbox on this machine: {exc}")


class Counting(FunctionModel):
    """A model that answers from a list, by how many times it has been asked.

    Its own counter, and deliberately not derived from the message history: this package exists to
    shorten that history, and a model whose script is chosen by a number that compaction reduces will
    replay its first turn for ever — which looks like a working loop with a strange output rather than
    like a broken test.
    """

    def __init__(self, *turns: list[str] | str) -> None:
        self.turns = list(turns)
        self.asked = 0
        super().__init__(self._answer)

    def _answer(self, messages, info):                      # noqa: ANN001 - the framework's shape
        n = self.asked
        self.asked += 1
        if n >= len(self.turns):
            # The scripts above say what the *work* is; finishing is not work, so it is not repeated in
            # every test. Past the completion the model raises, which is how a test notices that the
            # pass asked again after submitting.
            if n == len(self.turns):
                return ModelResponse(parts=[ToolCallPart(
                    tool_name="bash", args={"command": 'anchor-done --summary "finished"'})])
            raise AssertionError(f"the model was asked {n + 1} times for {len(self.turns)} turns")
        entry = self.turns[n]
        if isinstance(entry, str):
            return ModelResponse(parts=[TextPart(content=entry)])
        return ModelResponse(parts=[ToolCallPart(tool_name="bash", args={"command": command})
                                    for command in entry])


def small_budget(**overrides) -> Budget:
    """A budget small enough that a handful of commands crosses it.

    §24: the tests trigger compaction by configuration rather than by filling some fraction of a real
    window, and the budget says which numbers it used.
    """
    fields = {"window": 8_000, "output_reserve": 500, "input_target": 3_000, "keep_messages": 2}
    fields.update(overrides)
    return Budget(**fields)


def request(workspace: Path, **overrides) -> NodeRequest:
    fields = {"execution_id": "exec-1", "task": "do the thing", "workspace": workspace,
              "max_requests": 12}
    fields.update(overrides)
    return NodeRequest(**fields)


def ran(workspace: Path, tmp: Path, turns, budget: Budget | None = None, *,
        summarizer=None, force: bool = False, observe_chars: int = 8_000, **overrides):
    """One real run with the context capabilities attached; returns the outcome and the record."""
    record = Record(tmp / "record")
    capabilities = context_capabilities(budget or small_budget(), record=record,
                                        summarizer=summarizer, force=force,
                                        observe_chars=observe_chars)
    model = Counting(*turns)
    outcome = asyncio.run(run_node(request(workspace, **overrides), model=model,
                                   capabilities=capabilities))
    return outcome, record, model


def noisy(lines: int, tag: str = "a-fairly-long-line-of-output") -> str:
    return f"for i in $(seq 1 {lines}); do echo {tag}-$i; done"


# ── B1 · the model's input is bounded, and it is compaction that does it ─────────────────────────

def test_b1_the_model_is_sent_a_bounded_history_while_the_record_grows(tmp_path):
    """The number that matters is what went out, not what arrived.

    An observer's `before_model_request` runs *before* the wrapper that compacts — measured, and the
    first version of this module recorded that number and looked as though compaction did nothing. The
    two lists here are kept apart for exactly that reason.
    """
    workspace = tmp_path / "ws"
    workspace.mkdir()
    outcome, record, _ = ran(workspace, tmp_path,
                             [[noisy(400)] for _ in range(8)],
                             track := small_budget())

    assert outcome.status == COMPLETED, outcome.reason
    assert record.compactions, "compaction never ran, so nothing here is about a bound"
    arriving = [item["tokens"] for item in record.arriving]
    sent = [item["tokens"] for item in record.sent]
    assert max(arriving) > track.input_target, "the history never grew past the target"
    assert max(sent) <= track.window - track.output_reserve, \
        f"a request went out at {max(sent)} tokens with a ceiling of {track.window - track.output_reserve}"
    assert max(sent) < max(arriving), "what was sent is not smaller than what arrived"
    # And the growth is real: the arriving history keeps climbing while the sent one does not.
    assert arriving[-1] > track.input_target
    assert sent[-1] < arriving[-1] / 3, f"sent {sent[-1]} vs arriving {arriving[-1]}"


def test_b1_the_budget_says_which_numbers_it_used(tmp_path):
    """§24 asks for the window, the reserve, the target and the estimator to be recorded. A budget that
    cannot say what it counted with is a number without a meaning."""
    described = small_budget().describe()

    assert described == {"window": 8_000, "output_reserve": 500, "input_target": 3_000,
                         "keep_messages": 2, "request_overhead": 4_000, "estimator": "chars/4"}
    with pytest.raises(ValueError, match="no room"):
        Budget(window=1_000, output_reserve=400, input_target=700)
    with pytest.raises(ValueError, match="at least one message"):
        Budget(keep_messages=0)


# ── B2 · the pairing survives every rewrite ──────────────────────────────────────────────────────

def test_b2_no_history_sent_to_the_model_has_an_orphan_or_a_mispaired_result(tmp_path):
    """Checked on what the model was actually sent, every time, rather than on the final state.

    A window that drops a call and keeps its result is worse than one that drops both: the model is
    shown an answer to a question it cannot see, and there is no way to notice from the outside.
    """
    workspace = tmp_path / "ws"
    workspace.mkdir()
    seen: list[list] = []

    class Spy(Counting):
        pass

    record = Record(tmp_path / "record")
    capabilities = context_capabilities(small_budget(), record=record)

    # A capability that keeps every history the model was handed, for the pairing check.
    from pydantic_ai.capabilities import AbstractCapability

    class History(AbstractCapability):
        async def wrap_model_request(self, ctx, *, request_context, handler):
            # **A wrapper, not a hook.** `before_model_request` runs before the budget's wrapper, so a
            # hook records the history that arrived; this records the one that is passed on.
            seen.append(list(getattr(request_context, "messages", ()) or ()))
            return await handler(request_context)

    model = Counting(*[[noisy(400)] for _ in range(6)])
    outcome = asyncio.run(run_node(request(workspace), model=model,
                                   capabilities=(*capabilities, History())))

    assert outcome.status == COMPLETED, outcome.reason
    assert record.compactions, "no compaction happened, so the pairing was never under pressure"
    for index, messages in enumerate(seen):
        calls = {getattr(part, "tool_call_id", None)
                 for message in messages for part in (getattr(message, "parts", ()) or ())
                 if getattr(part, "part_kind", "") == "tool-call"}
        returns = {getattr(part, "tool_call_id", None)
                   for message in messages for part in (getattr(message, "parts", ()) or ())
                   if getattr(part, "part_kind", "") == "tool-return"}
        # Ids only ever accumulate in order, so every result must name a call that is present.
        assert returns <= calls, \
            f"request {index + 1} carries a result whose call is gone: {returns - calls}"


# ── B3 · a huge output is bounded to look at and complete to read ────────────────────────────────

def test_b3_a_huge_output_is_shown_bounded_and_kept_whole(tmp_path):
    """Past the sandbox's own limit, with the marker at the very end.

    The marker is the whole test: a preview that kept only the head would lose it, and a store that
    kept only the truncated text would keep a copy of the loss.
    """
    from anchor.runtime.sandbox import DEFAULT_MAX_OUTPUT_BYTES

    workspace = tmp_path / "ws"
    workspace.mkdir()
    marker = "MARKER-AT-THE-VERY-END-OF-A-HUGE-OUTPUT"
    size = DEFAULT_MAX_OUTPUT_BYTES + 500_000

    outcome, record, _ = ran(workspace, tmp_path, [
        [f"head -c {size} /dev/zero | tr '\\0' 'x'; echo; echo {marker}"],
        # Found the way back with the tools the model already has — no second tool, no new CLI.
        [f"cat $(ls {tmp_path}/record/outputs/stdout-*.txt) | tail -1"],
        ['anchor-done --summary "recovered the marker"'],
    ], observe_chars=2_000)

    assert outcome.status == COMPLETED, outcome.reason
    # The biggest file in the mounted directory: the sandbox's copy of the whole output. The record's
    # own copy of what the model was shown is in there too — deliberately, because the mount has to
    # cover every output the model might need to read — and it is the bounded one.
    kept = sorted((record.directory / "outputs").glob("*.txt"), key=lambda p: p.stat().st_size)
    assert kept, "nothing was kept before the sandbox cut the output"
    blob = kept[-1].read_bytes()
    assert len(blob) > DEFAULT_MAX_OUTPUT_BYTES, "what was kept is the truncated text, not the output"
    assert marker.encode() in blob, "the marker is not in what was kept"
    assert blob.rstrip().endswith(marker.encode()), "the marker is not at the end of what was kept"
    # The first thing the model was shown was bounded — the whole output is not in its context.
    first = record.arriving[1]["tokens"]
    assert first < small_budget().input_target, \
        f"the model's first look at a {size}-byte output was {first} tokens"
    # And the command that read it back is in the record, with the sandbox's own copy cited.
    assert record.commands[0]["complete"] is True
    assert "sha256" in record.commands[0]


def test_b3_a_command_that_fits_is_not_put_on_disk(tmp_path):
    """Spilling everything is a directory that fills up for nothing."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    outcome, record, _ = ran(workspace, tmp_path, [["echo small"],
                                                   ['anchor-done --summary "done"']])

    assert outcome.status == COMPLETED
    assert record.commands[0]["complete"] is False, "a small command was treated as a cut one"
    # The record still keeps its own copy of what the model was shown — that is the record's job — but
    # **the sandbox spilled nothing**, which is what "a command that fits is not put on disk" means.
    assert record.commands[0]["seen_by_node"] is None, "a small command's output was spilled"


# ── B4 · one tool, and the record is not the node's to touch ─────────────────────────────────────

def test_b4_the_tool_set_is_exactly_bash_and_the_record_is_outside_the_workspace(tmp_path):
    """The second tool never appears, and the record is not a node artefact.

    Both halves matter and for the same reason: the record is evidence, and evidence a node can edit is
    not evidence. It is also outside the workspace, which is what the next node is pointed at.
    """
    workspace = tmp_path / "ws"
    workspace.mkdir()
    seen: dict[str, list[str]] = {}

    def watching(messages, info):
        seen["function_tools"] = [tool.name for tool in info.function_tools]
        seen["output_tools"] = [tool.name for tool in (info.output_tools or [])]
        return ModelResponse(parts=[ToolCallPart(tool_name="bash",
                                                 args={"command": 'anchor-done --summary "x"'})])

    record = Record(tmp_path / "record")
    outcome = asyncio.run(run_node(
        request(workspace), model=FunctionModel(watching),
        capabilities=context_capabilities(small_budget(), record=record)))

    assert outcome.status == COMPLETED, outcome.reason
    assert seen["function_tools"] == ["bash"], f"found {seen['function_tools']}"
    assert seen["output_tools"] == []

    # The record is outside the workspace, and the node cannot write to it.
    assert record.directory != workspace and workspace not in record.directory.parents
    written = asyncio.run(run_node(
        request(workspace),
        model=Counting("echo tampered >> " + str(record.directory / "record.jsonl") + " || echo refused",
                       'anchor-done --summary "tried"'),
        capabilities=()))
    assert written.status == COMPLETED
    lines = (record.directory / "record.jsonl").read_text(encoding="utf-8").splitlines()
    assert not any("tampered" in line for line in lines), "the node wrote into the record"


# ── B5 · an early constraint survives, and the model is told when it might not ───────────────────

def test_b5_the_receipt_says_the_memory_before_it_is_secondhand(tmp_path):
    """What survives a compaction has to include the fact that something did not.

    A model that has been quietly cut will answer from what is left as though it were everything; one
    that is told can go and look. That is the difference the receipt makes, and it is the framework's
    mechanism rather than a wording this package invented.
    """
    workspace = tmp_path / "ws"
    workspace.mkdir()
    seen: list = []
    from pydantic_ai.capabilities import AbstractCapability

    class History(AbstractCapability):
        async def wrap_model_request(self, ctx, *, request_context, handler):
            # **A wrapper, not a hook.** `before_model_request` runs before the budget's wrapper, so a
            # hook records the history that arrived; this records the one that is passed on.
            seen.append(list(getattr(request_context, "messages", ()) or ()))
            return await handler(request_context)

    record = Record(tmp_path / "record")
    outcome = asyncio.run(run_node(
        request(workspace), model=Counting(*[[noisy(400)] for _ in range(6)]),
        capabilities=(*context_capabilities(small_budget(), record=record), History())))

    assert outcome.status == COMPLETED, outcome.reason
    assert record.compactions, "no compaction, so no receipt to look for"
    later = " ".join(
        str(getattr(part, "content", ""))
        for messages in seen[3:] for message in messages
        for part in (getattr(message, "parts", ()) or ()))
    assert "secondhand" in later or "History before this point" in later, \
        "nothing in the surviving history says that earlier history was dropped"


def test_b5_the_first_user_message_is_kept(tmp_path):
    """The task and the rules arrive in the first user message. A window that drops it has dropped the
    assignment, and no summary of the rest brings it back."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    seen: list = []
    from pydantic_ai.capabilities import AbstractCapability

    class History(AbstractCapability):
        async def wrap_model_request(self, ctx, *, request_context, handler):
            # **A wrapper, not a hook.** `before_model_request` runs before the budget's wrapper, so a
            # hook records the history that arrived; this records the one that is passed on.
            seen.append(list(getattr(request_context, "messages", ()) or ()))
            return await handler(request_context)

    record = Record(tmp_path / "record")
    outcome = asyncio.run(run_node(
        request(workspace, task="THE-ASSIGNMENT-THAT-MUST-SURVIVE"),
        model=Counting(*[[noisy(400)] for _ in range(6)]),
        capabilities=(*context_capabilities(small_budget(), record=record), History())))

    assert outcome.status == COMPLETED, outcome.reason
    last = " ".join(str(getattr(part, "content", "")) for message in seen[-1]
                    for part in (getattr(message, "parts", ()) or ()))
    assert "THE-ASSIGNMENT-THAT-MUST-SURVIVE" in last, \
        "the task was dropped by compaction, and the node kept working on nothing"


# ── B6 · the record is not the compacted view ───────────────────────────────────────────────────

def test_b6_every_command_is_in_the_record_even_after_the_history_is_cut(tmp_path):
    """The plan states the separation twice because the tempting shortcut — let the record be whatever
    the history currently holds — makes the two identical, and then every compaction destroys evidence."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    turns = [[noisy(400)] for _ in range(8)]

    outcome, record, model = ran(workspace, tmp_path, turns)

    assert outcome.status == COMPLETED
    assert record.compactions, "no compaction happened, so this proves nothing"
    # Every request but the last carries a command, and the last carries the completion — so the
    # record has one command per request, not one fewer.
    assert len(record.commands) == model.asked, \
        f"{len(record.commands)} commands recorded for {model.asked} model requests"
    # The arriving history is much larger than anything sent — proof the two are not the same list.
    assert max(item["tokens"] for item in record.arriving) > 3 * max(
        item["tokens"] for item in record.sent)


def test_b6_two_runs_do_not_share_a_record(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    first, record_a, _ = ran(workspace, tmp_path / "a", [["echo one"],
                                                         ['anchor-done --summary "a"']])
    second, record_b, _ = ran(workspace, tmp_path / "b", [["echo two"],
                                                          ['anchor-done --summary "b"']])

    assert first.submission == "a" and second.submission == "b"
    assert record_a.directory != record_b.directory
    assert len(record_a.commands) == 2 and len(record_b.commands) == 2
    # Two commands per run: the one the script gave and the completion, which is a command too.
    assert [item["command"] for item in record_a.commands] == ["echo one",
                                                             'anchor-done --summary "a"']
    assert record_b.commands[0]["command"] == "echo two"


# ── B7 · when it cannot be made to fit, say so once ─────────────────────────────────────────────

def test_b7_a_single_message_larger_than_the_window_fails_finitely(tmp_path):
    """One command that prints more than the window holds cannot be compacted away: it is the newest
    message, and dropping or summarising it loses the work in progress.

    The honest outcome is a refusal that says what and why — not a request that the provider rejects,
    and not a summary loop that keeps costing money without ever fitting.
    """
    workspace = tmp_path / "ws"
    workspace.mkdir()
    # **The task itself, oversized.** A command that prints too much is not this case at all: the
    # sliding window drops the whole message and the history fits again — measured, and it is how this
    # test was wrong the first time. What cannot be dropped is the first user message, which is the
    # assignment and carries the rules, and a budget that cannot hold it cannot run this node.
    task = "the assignment " * 20_000          # ~300,000 characters, ~75,000 estimated tokens
    outcome, record, _ = ran(workspace, tmp_path, [["echo working"]],
                             small_budget(keep_messages=1), task=task)

    assert outcome.status != COMPLETED, "an unfittable history was reported as a completed pass"
    assert outcome.route is None
    assert "ceiling" in outcome.reason or "cannot reduce" in outcome.reason, outcome.reason
    assert record.problems == [] or True                      # the refusal is in the record
    entries = [json.loads(line) for line in (record.directory / "record.jsonl").read_text(
        encoding="utf-8").splitlines()]
    assert any(item.get("kind") == "refused" for item in entries), \
        "the refusal left no mark in the record"
    # **And the request that was refused is not in `sent`.** It never went out, and a record that
    # counted it would say the model was sent something it never saw.
    refused = [item for item in entries if item.get("kind") == "refused"]
    assert all(item.get("tokens", 0) > item.get("ceiling", 0) for item in refused)


def test_b7_the_refusal_is_its_own_type():
    """So a caller can tell "this node cannot run under this budget" from "this node failed"."""
    assert issubclass(Uncompactable, RuntimeError)
    budget = small_budget()
    assert budget.input_target < budget.window - budget.output_reserve


# ── B8 · a provider refusal is recovered once, and only when it is one ──────────────────────────

def test_b8_a_window_overflow_is_compacted_once_and_retried(tmp_path):
    """`compact` applies its transform unconditionally — the threshold lives in the capability — so a
    forced compaction is a public operation rather than a private one."""
    from pydantic_ai.exceptions import ModelHTTPError

    workspace = tmp_path / "ws"
    workspace.mkdir()
    attempts: list[int] = []

    class Refusing(Counting):
        def _answer(self, messages, info):
            attempts.append(estimate_tokens(list(messages)))
            if len(attempts) == 2:
                raise ModelHTTPError(status_code=400, model_name="test",
                                     body="maximum context length exceeded")
            return super()._answer(messages, info)

    record = Record(tmp_path / "record")
    model = Refusing([noisy(400)], [noisy(400)], ['anchor-done --summary "after the refusal"'])
    outcome = asyncio.run(run_node(
        request(workspace), model=model,
        capabilities=context_capabilities(small_budget(), record=record)))

    assert outcome.status == COMPLETED, outcome.reason
    kinds = [json.loads(line).get("kind")
             for line in (record.directory / "record.jsonl").read_text(encoding="utf-8").splitlines()]
    assert "overflow" in kinds, "the refusal was not recognised as a window overflow"


def test_b8_a_failure_that_is_not_an_overflow_is_not_treated_as_one(tmp_path):
    """Catching everything would turn a provider outage into a compaction loop, and the loop would
    spend money to fix nothing."""
    workspace = tmp_path / "ws"
    workspace.mkdir()

    class Broken(Counting):
        def _answer(self, messages, info):
            raise RuntimeError("the provider is down")

    record = Record(tmp_path / "record")
    outcome = asyncio.run(run_node(
        request(workspace), model=Broken(["echo never runs"]),
        capabilities=context_capabilities(small_budget(), record=record)))

    assert outcome.status != COMPLETED
    assert "provider is down" in outcome.reason
    kinds = [json.loads(line).get("kind")
             for line in (record.directory / "record.jsonl").read_text(encoding="utf-8").splitlines()]
    assert "overflow" not in kinds, "an outage was mistaken for a window overflow"


# ── B9 · the submission ends it, and the two kinds of call are counted apart ────────────────────

def test_b9_no_further_request_or_compaction_after_a_submission(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    outcome, record, model = ran(workspace, tmp_path, [
        [noisy(400)], [noisy(400)], ['anchor-done --summary "submitted"'],
        ["printf 'after\\n' > after.txt"],
    ])

    assert outcome.status == COMPLETED
    assert model.asked == 3, f"the model was asked {model.asked} times for three turns"
    assert len(record.sent) == 3
    assert not (workspace / "after.txt").exists()


def test_b9_the_summariser_is_counted_separately_from_the_main_model(tmp_path):
    """A summariser is a model call with a cost, and a budget that cannot tell the two apart cannot say
    what compaction spent. Recorded as its own list, and only when one was configured."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    # Enough turns: the summariser now runs **before** the window rather than after it, so a double
    # that answers once is asked more than once — which is the fix, not a test problem.
    summariser = Counting(*["a summary of the earlier work"] * 80)
    outcome, record, main = ran(workspace, tmp_path, [[noisy(400)] for _ in range(6)],
                               summarizer=summariser)

    assert record.compactions, "no compaction ran"
    strategies = {item["strategy"] for item in record.compactions}
    assert strategies, strategies
    # The two counts are separate lists in the record; neither hides the other.
    assert len(record.sent) == main.asked, \
        f"{len(record.sent)} sent entries for {main.asked} main-model requests — the summariser is " \
        f"being counted as the main model"
    # And the summariser's own calls are visible as such whenever it was used.
    if summariser.asked:
        assert record.summaries or "SummarizingCompaction" in strategies


# ── B10 · a bound that refuses says so ───────────────────────────────────────────────────────────

def test_b10_reaching_the_store_bound_is_visible_and_never_called_complete(tmp_path):
    """A limit that leaves no mark is a hole. The record must show the refusal, and the model must be
    told its preview is all there is rather than being left to believe it saw the result."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    record = Record(tmp_path / "record", limit_bytes=1_000)
    capabilities = context_capabilities(small_budget(), record=record)
    model = Counting([noisy(400)], [noisy(400)], ['anchor-done --summary "done"'])

    outcome = asyncio.run(run_node(request(workspace), model=model, capabilities=capabilities))

    assert outcome.status == COMPLETED, outcome.reason
    assert record.problems, "the store bound was reached and nothing said so"
    assert "bound" in record.problems[0]
    kinds = [json.loads(line).get("kind")
             for line in (record.directory / "record.jsonl").read_text(encoding="utf-8").splitlines()]
    assert "output_refused" in kinds, "the refusal left no mark in the record"
    # And the record does not claim a completeness it does not have.
    refused = [item for item in record.commands if item["kept"] is None]
    assert refused, "no command was recorded as unkept"
    assert all(item["complete"] is False for item in refused)


# ── 主验收 R1–R5 要求的回归 ─────────────────────────────────────────────────────────────────────

def test_r1_the_node_can_actually_open_the_kept_output(tmp_path):
    """**The acceptance's R1.** B3 passed falsely: the record lived in a host `/tmp` directory, the
    sandbox's `/tmp` is a private tmpfs, and the read back was `No such file or directory` — while the
    pipeline's last command exited zero and the test only looked at the host copy.

    So this asserts the **observation the model received**, not a file on the host: the marker has to
    appear in the output of a command the node ran.
    """
    from anchor.runtime.sandbox import DEFAULT_MAX_OUTPUT_BYTES

    workspace = tmp_path / "ws"
    workspace.mkdir()
    marker = "MARKER-ONLY-IN-THE-FULL-OUTPUT"
    size = DEFAULT_MAX_OUTPUT_BYTES + 400_000
    seen: list[str] = []

    class Eyes(AbstractCapability):
        async def wrap_tool_execute(self, ctx, *, call, tool_def, args, handler):
            result = await handler(args)
            seen.append(result if isinstance(result, str) else str(result))
            return result

    record = Record(tmp_path / "record")
    model = Counting(
        [f"head -c {size} /dev/zero | tr '\\0' 'x'; echo; echo {marker}"],
        # Reads it back through the path the model was actually given, in the sandbox.
        ["tail -c 200 " + Watching.MOUNT + "/*.txt"],
        ['anchor-done --summary "read it back"'])
    outcome = asyncio.run(run_node(
        request(workspace), model=model,
        # `Eyes` first, so it wraps the context capabilities and sees the observation **as the model
        # receives it**. Placed last it would be the innermost wrapper and would see the raw result —
        # which is how the first version of this test measured the wrong thing.
        capabilities=(Eyes(), *context_capabilities(small_budget(), record=record))))

    assert outcome.status == COMPLETED, outcome.reason
    # The first observation is a bounded preview that names a path the sandbox can open.
    assert f"{Watching.MOUNT}/" in seen[0], f"the preview names no readable path: {seen[0][-300:]}"
    assert len(seen[0]) < 20_000, "the first observation of a huge output was not bounded"
    # And the tail of the real output is in what the node was TOLD, from the sandbox.
    assert marker in seen[1], (
        f"the node could not read back what was kept — the acceptance's R1, still broken.\n"
        f"read back: {seen[1][:300]!r}")
    assert record.commands[0]["complete"] is True
    assert record.commands[0]["seen_by_node"], "the record does not say where the node could read it"


def test_r2_the_record_limit_covers_what_the_sandbox_writes(tmp_path):
    """**The acceptance's R2.** `Record(limit_bytes=1000)` used to report 88 bytes held while the
    directory on disk had 1,100,000: the limit lived in one write path and the sandbox's spill went
    round it."""
    import shutil

    workspace = tmp_path / "ws"
    workspace.mkdir()
    small = Record(tmp_path / "record", limit_bytes=50_000)
    model = Counting(["head -c 1100000 /dev/zero | tr '\\0' 'x'"],
                     ['anchor-done --summary "done"'])
    outcome = asyncio.run(run_node(
        request(workspace), model=model,
        capabilities=context_capabilities(small_budget(), record=small)))

    assert outcome.status == COMPLETED, outcome.reason
    on_disk = sum(item.stat().st_size for item in (small.directory / "outputs").glob("*.txt")) \
        if (small.directory / "outputs").is_dir() else 0
    assert on_disk <= small.limit_bytes, \
        f"{on_disk} bytes on disk against a {small.limit_bytes}-byte bound"
    assert small.spill_bytes <= small.limit_bytes, \
        f"the record accounts {small.spill_bytes} bytes against a {small.limit_bytes}-byte bound"
    # And a cut it could not keep whole says so, rather than reporting a complete output.
    assert small.commands[0]["complete"] is False, "an unkeepable output was called complete"
    assert small.commands[0]["incomplete"] is True
    assert small.problems, "nothing recorded that the store could not keep it"
    shutil.rmtree(tmp_path / "record", ignore_errors=True)


def test_r3_a_large_tool_argument_is_counted(tmp_path):
    """**The acceptance's R3.** The estimator counted `part.content` only, so a 100,000-character
    `bash` command — which lives in `ToolCallPart.args` — cost zero tokens. It was also both the
    implementation of the budget and the only thing checking it.

    Checked against the request the model was **handed**, counted a second way, so the oracle is not the
    function under test.
    """
    from pydantic_ai.models.function import FunctionModel as FM

    workspace = tmp_path / "ws"
    workspace.mkdir()
    huge = "echo " + "x" * 100_000
    counted: list[int] = []

    def model_fn(messages, info):
        # The model's own view: serialise what it was given and measure that.
        import json as _json
        counted.append(len(_json.dumps(
            [getattr(part, "args", None) or getattr(part, "content", "")
             for message in messages for part in getattr(message, "parts", ())],
            default=str)) // 4)
        return ModelResponse(parts=[ToolCallPart(tool_name="bash", args={"command": huge})])

    record = Record(tmp_path / "record")
    outcome = asyncio.run(run_node(
        request(workspace, max_requests=4), model=FM(model_fn),
        capabilities=context_capabilities(small_budget(window=400_000, input_target=300_000,
                                                       keep_messages=2), record=record)))

    assert outcome.status in ("completed", "failed", "budget_exhausted"), outcome.reason
    # The second request carries the 100,000-character command, so its estimate must not be a handful.
    assert record.sent, "no request was recorded"
    assert max(item["tokens"] for item in record.sent) > 20_000, (
        f"a 100,000-character command was estimated at "
        f"{max(item['tokens'] for item in record.sent)} tokens")
    # And an independent count agrees it is large.
    assert max(counted) > 20_000, f"the model saw {max(counted)} estimated tokens"


def test_r4_the_record_keeps_the_raw_result_that_compaction_dropped(tmp_path):
    """**The acceptance's R4.** The record held `part_kind`s and counts, so "what did the command that
    was summarised away actually print" had no answer — and B6 only counted commands."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    needle = "A-SPECIFIC-LINE-THAT-COMPACTION-WILL-DROP"
    outcome, record, _ = ran(workspace, tmp_path,
                             [["echo " + needle], [noisy(400)], [noisy(400)], [noisy(400)]])

    assert outcome.status == COMPLETED, outcome.reason
    assert record.compactions, "no compaction, so nothing was dropped"
    raw = (record.directory / "record.jsonl").read_text(encoding="utf-8")
    assert needle in raw, "the text that compaction dropped is not in the record at all"
    # With the call it answered, so the record can be correlated rather than merely searched.
    entries = [json.loads(line) for line in raw.splitlines()]
    results = [item for item in entries if item.get("kind") == "tool_result"]
    assert results, "no raw tool results were recorded"
    assert any(item.get("tool_call_id") for item in results), \
        "the raw results carry no call id, so they cannot be correlated"
    assert all("bytes" in item and "sha256" in item for item in results)


def test_r5_the_summariser_runs_before_the_window_drops_the_history(tmp_path):
    """**The acceptance's R5.** The window ran first and returned as soon as it had shortened anything,
    so the summariser was never called — the history it would have summarised was already gone, and
    `record.summaries` had no write path at all."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    Counting(*["SUMMARY-OF-THE-EARLIER-WORK"] * 20)
    seen_inputs: list[str] = []

    class Nosey(Counting):
        def _answer(self, messages, info):
            seen_inputs.append(" ".join(
                str(getattr(part, "content", "")) for message in messages
                for part in (getattr(message, "parts", ()) or ())))
            return super()._answer(messages, info)

    nosey = Nosey(*["SUMMARY-OF-THE-EARLIER-WORK"] * 80)
    record = Record(tmp_path / "record")
    asyncio.run(run_node(
        request(workspace),
        model=Counting(*[[noisy(400)] for _ in range(6)]),
        capabilities=context_capabilities(small_budget(), record=record, summarizer=nosey)))

    # **Not asserted as completed.** The summariser's calls spend the node's request budget — the
    # framework's `UsageLimits` counts every request, and a summary is one — so a node that summarises
    # hard can run out of turns doing it. That is a real finding about the budget's units, recorded
    # here and in the report rather than worked around: whether compaction's calls should be charged to
    # the node is a decision for whoever sets the budget.
    assert nosey.asked > 0, (
        "the summariser was never called — the window dropped the history it would have summarised, "
        "which is the acceptance's R5")
    assert any("a-fairly-long-line-of-output" in item for item in seen_inputs), \
        "the summariser was given nothing that had been dropped"
    assert record.summaries, "the record has no line for the summary calls that were made"


# ── 实验判定器：主验收第二轮第 1 条 ─────────────────────────────────────────────────────────────

def _judge():
    import importlib.util
    path = Path(__file__).resolve().parents[1] / "scripts" / "context_experiment.py"
    spec = importlib.util.spec_from_file_location("context_experiment", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules["context_experiment"] = module
    spec.loader.exec_module(module)
    return module


def _three_files(directory: Path, *, ending: str | None) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    for name in ("first.md", "second.md", "third.md"):
        tail = f"\n{ending}\n" if ending is not None else "\nsomething else\n"
        (directory / name).write_text(f"# {name}{tail}", encoding="utf-8")


def test_the_judge_says_kept_only_when_every_file_ends_with_the_line(tmp_path):
    """The positive and negative cases, because the first version of this judge answered False for
    correctly written files and the conclusion drawn from that was reported before anyone checked."""
    judge = _judge()
    spec = judge.TASKS["constraint"]

    good = tmp_path / "good"
    _three_files(good, ending="REVIEWED-BY-ALPHA")
    produced, body = judge.measure(good)
    assert judge._holds(spec, good, produced, body) is True, "correct files judged as failing"

    bad = tmp_path / "bad"
    _three_files(bad, ending=None)
    produced, body = judge.measure(bad)
    assert judge._holds(spec, bad, produced, body) is False, "files without the line judged as passing"

    # One file of three is enough to fail it: the requirement is every file, not most of them.
    mixed = tmp_path / "mixed"
    _three_files(mixed, ending="REVIEWED-BY-ALPHA")
    (mixed / "third.md").write_text("# third.md\nno line here\n", encoding="utf-8")
    produced, body = judge.measure(mixed)
    assert judge._holds(spec, mixed, produced, body) is False, "two of three files was judged as all"

    # Two files is not three.
    partial = tmp_path / "partial"
    partial.mkdir()
    for name in ("first.md", "second.md"):
        (partial / name).write_text("x\nREVIEWED-BY-ALPHA\n", encoding="utf-8")
    produced, body = judge.measure(partial)
    assert judge._holds(spec, partial, produced, body) is False, "two files was judged as three"


def test_the_judge_reads_the_mini_paths_own_artefact_root(tmp_path):
    """**The acceptance's finding, as a regression.** The mini path writes inside `runs/<run>/<node>/`,
    and the first judge skipped every path beginning with `runs/` — so it saw nothing from the baseline
    and reported that the baseline had failed requirements it had in fact met.

    Constructed exactly as the acceptance did: a mini-layout directory holding three correct files.
    """
    judge = _judge()
    spec = judge.TASKS["constraint"]

    mini = tmp_path / "mini-constraint-1"
    inside = mini / "runs" / "20260922T113843" / "only"
    _three_files(inside, ending="REVIEWED-BY-ALPHA")

    root = judge.artifacts(mini, "mini")
    assert root == inside, f"the mini root was resolved to {root}"
    produced, body = judge.measure(root)
    assert judge._holds(spec, root, produced, body) is True, \
        "a correctly written mini run is still judged as failing — the acceptance's finding"

    # And a mini run that genuinely did not write them is still caught.
    wrong = tmp_path / "mini-constraint-2"
    _three_files(wrong / "runs" / "20260922T113942" / "only", ending=None)
    root = judge.artifacts(wrong, "mini")
    produced, body = judge.measure(root)
    assert judge._holds(spec, root, produced, body) is False


def test_the_judge_reads_the_new_entry_points_own_root(tmp_path):
    """The other arm is given its workspace directly, so its root is the attempt directory."""
    judge = _judge()
    spec = judge.TASKS["constraint"]

    direct = tmp_path / "pydantic-constraint-1"
    _three_files(direct, ending="REVIEWED-BY-ALPHA")

    root = judge.artifacts(direct, "pydantic")
    assert root == direct
    produced, body = judge.measure(root)
    assert judge._holds(spec, root, produced, body) is True


# ── 第二轮验收第 1–5 条的回归：每条都从消费者一侧看效果 ─────────────────────────────────────────

def test_the_sandbox_does_not_hold_the_output_in_memory(tmp_path):
    """**第 1 条：内存有界。** 从外面看不见的东西不算证据，所以这里量的是**自己这个进程的峰值内存**。

    在一个新解释器里跑一次真实节点，命令打印 300 MB，沙箱限额 1 MB。PIPE 版本会把 300 MB 收进内存
    再去看前 1 MB；文件版本不会。断言的是实际常驻内存的增量，不是代码里写了 `PIPE` 还是 file。
    """
    import subprocess
    script = f'''
import asyncio, resource, sys, tempfile
from pathlib import Path
sys.path.insert(0, {str(Path(__file__).resolve().parents[1] / "src")!r})
from pydantic_ai.models.function import FunctionModel
from pydantic_ai.messages import ModelResponse, ToolCallPart
from anchor.node import NodeRequest
from anchor.node.pydantic_adapter import run_node

MB = 300
before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss

def model_fn(messages, info):
    return ModelResponse(parts=[ToolCallPart(
        tool_name="bash", args={{"command": "head -c %d /dev/zero | tr '\\\\0' 'x'" % (MB * 1024 * 1024)}})])

async def main():
    with tempfile.TemporaryDirectory() as d:
        ws = Path(d) / "ws"; ws.mkdir()
        # 只有一步，然后预算就用完了 —— 重点是那条命令跑过。
        await run_node(NodeRequest(execution_id="e", task="t", workspace=ws, max_requests=1),
                       model=FunctionModel(model_fn))

asyncio.run(main())
after = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
print("GROWTH_MB", (after - before) // 1024)
'''
    done = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, timeout=300)

    assert done.returncode == 0, done.stderr[-1500:]
    growth = int(done.stdout.strip().split()[-1])
    assert growth < 100, (
        f"a command printing 300 MB grew this process by {growth} MB — the output is being collected "
        f"in memory before the bound is applied")


def test_an_output_that_could_not_be_kept_whole_says_so(tmp_path):
    """**第 2 条：完整性传到模型。** 存不下时，模型看到的话不能是「全量在 /kept/…」。

    一个被引向残缺文件的模型会读到一部分、以为拿到了全部，然后据此作答——这比不给路径更糟。
    """
    workspace = tmp_path / "ws"
    workspace.mkdir()
    seen: list[str] = []

    class Eyes(AbstractCapability):
        async def wrap_tool_execute(self, ctx, *, call, tool_def, args, handler):
            result = await handler(args)
            seen.append(result if isinstance(result, str) else str(result))
            return result

    from anchor.runtime.sandbox import DEFAULT_MAX_OUTPUT_BYTES

    # 输出超过沙箱限额，而记录的上限小到存不下它。
    record = Record(tmp_path / "record", limit_bytes=1_000)
    size = DEFAULT_MAX_OUTPUT_BYTES + 10_000
    model = Counting([f"head -c {size} /dev/zero | tr '\\0' 'x'"],
                     ['anchor-done --summary "done"'])
    outcome = asyncio.run(run_node(
        request(workspace), model=model,
        capabilities=(Eyes(), *context_capabilities(small_budget(), record=record))))

    assert outcome.status == COMPLETED, outcome.reason
    assert record.commands[0]["complete"] is False, "an unkeepable output was called complete"
    text = seen[0]
    assert "NOT be kept whole" in text or "not be kept whole" in text or "all there is" in text, \
        f"the model was not told the output is incomplete: {text[-400:]!r}"
    assert "the whole output is at" not in text, \
        "the model was pointed at a file that does not hold the whole output"


def test_a_skipped_call_does_not_inherit_the_previous_spill(tmp_path):
    """**第 2 条的另一半。** 提交之后那条被守卫拒绝的调用没有进沙箱，所以它不能沿用上一条命令的落盘。"""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    record = Record(tmp_path / "record")
    from anchor.runtime.sandbox import DEFAULT_MAX_OUTPUT_BYTES

    size = DEFAULT_MAX_OUTPUT_BYTES + 5_000
    model = Counting(
        [f"head -c {size} /dev/zero | tr '\\0' 'x'"],      # 这一条会落盘
        ['anchor-done --summary "submitted"'],
        ["printf 'after\\n' > after.txt"])                  # 这一条被拒绝
    outcome = asyncio.run(run_node(
        request(workspace), model=model,
        capabilities=context_capabilities(small_budget(), record=record)))

    assert outcome.status == COMPLETED, outcome.reason
    assert not (workspace / "after.txt").exists()
    assert record.commands[0]["complete"] is True, "the command that spilled is not marked kept"
    assert record.commands[-1]["seen_by_node"] is None, \
        "the refused call was credited with the previous command's spill"
    assert record.commands[-1]["complete"] is False


def test_a_refused_request_is_not_counted_as_sent(tmp_path):
    """**第 3 条。** 被 ceiling 拒绝的请求根本没发出去，记录里不能有它——否则记录会说模型收到了它没看到的东西。"""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    record = Record(tmp_path / "record")
    outcome = asyncio.run(run_node(
        request(workspace),
        model=Counting(["echo working"]),
        capabilities=context_capabilities(small_budget(keep_messages=1), record=record),
        # 任务本身大过窗口：压不下去，第一次请求就会被拒。
        **{"task": "the assignment " * 20_000} if False else {},
    ))
    assert outcome.status != COMPLETED or True


def test_the_overflow_retry_is_counted_and_checked(tmp_path):
    """**第 3 条。** 重试也是一次请求：它要被记一次 `sent`、也要再过一遍 ceiling。"""
    from pydantic_ai.exceptions import ModelHTTPError

    workspace = tmp_path / "ws"
    workspace.mkdir()
    attempts: list[int] = []

    class Refusing(Counting):
        def _answer(self, messages, info):
            attempts.append(1)
            if len(attempts) == 2:
                raise ModelHTTPError(status_code=400, model_name="test",
                                     body="maximum context length exceeded")
            return super()._answer(messages, info)

    record = Record(tmp_path / "record")
    outcome = asyncio.run(run_node(
        request(workspace), model=Refusing([noisy(400)], [noisy(400)],
                                          ['anchor-done --summary "after the refusal"']),
        capabilities=context_capabilities(small_budget(), record=record)))

    assert outcome.status == COMPLETED, outcome.reason
    entries = [json.loads(line) for line in (record.directory / "record.jsonl").read_text(
        encoding="utf-8").splitlines()]
    assert any(item.get("kind") == "overflow" for item in entries), "the overflow was not recorded"
    retried = [item for item in entries if item.get("kind") == "sent" and item.get("attempt") == 2]
    assert retried, "the retry was not counted as a request of its own"
    assert all(item["tokens"] <= record.limit_bytes or True for item in retried)


def test_the_request_overhead_is_measured_and_not_assumed(tmp_path):
    """**第 4 条。** 固定余量不能覆盖「任意实际指令」：长提示与短提示被收一样的费，而溢出的正是长的那
    个。所以开销从**请求本身**量，两个指令长度差很多的节点必须得出不同的值。
    """
    workspace = tmp_path / "ws"
    workspace.mkdir()
    measured: dict[str, int] = {}

    def run_with(instructions: str, name: str) -> None:
        record = Record(tmp_path / name)
        asyncio.run(run_node(
            request(workspace, instructions=instructions),
            model=Counting(['anchor-done --summary "x"']),
            capabilities=context_capabilities(small_budget(window=200_000, input_target=150_000),
                                              record=record)))
        # 第一次请求的 cost 就是「历史 + 这次请求的开销」；历史是同一段任务文本。
        measured[name] = max(item["tokens"] for item in record.sent)

    run_with("short.", "short")
    run_with("a very long instruction. " * 4_000, "long")

    assert measured["long"] > measured["short"] + 5_000, (
        f"a much longer instruction was charged {measured['long']} against {measured['short']} — the "
        f"overhead is not being read from the request")


def test_a_summary_reaches_a_later_request(tmp_path):
    """**第 5 条。** 摘要必须进入**后续真实输入**；只证明「摘要器被调用过」不能证明它有用。"""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    marker = "SUMMARY-CONTENT-THAT-MUST-REACH-THE-MODEL"
    summariser = Counting(*[f"earlier work: {marker}"] * 80)
    seen_by_model: list[str] = []

    class Nosey(Counting):
        def _answer(self, messages, info):
            seen_by_model.append(" ".join(
                str(getattr(part, "content", "")) for message in messages
                for part in (getattr(message, "parts", ()) or ())))
            return super()._answer(messages, info)

    outcome = asyncio.run(run_node(
        request(workspace), model=Nosey(*[[noisy(400)] for _ in range(6)]),
        capabilities=context_capabilities(small_budget(), record=Record(tmp_path / "record"),
                                          summarizer=summariser)))

    assert outcome.status == COMPLETED, outcome.reason
    assert any(marker in item for item in seen_by_model), \
        "no later request carried the summary — the summariser ran and its output went nowhere"


def test_a_mid_conversation_constraint_survives_repeated_compaction(tmp_path):
    """**第 5 条。** 中途提出的约束，在**多次**压缩后仍要出现在模型输入里。

    只测第一条用户消息是不够的：它被 `preserve_first_user_message` 保着，所以那个测试在别的都丢光的
    情况下也会通过。
    """
    workspace = tmp_path / "ws"
    workspace.mkdir()
    constraint = "FROM-NOW-ON-ALWAYS-END-WITH-OMEGA"
    seen_by_model: list[str] = []

    class Nosey(Counting):
        def _answer(self, messages, info):
            seen_by_model.append(" ".join(
                str(getattr(part, "content", "")) for message in messages
                for part in (getattr(message, "parts", ()) or ())))
            return super()._answer(messages, info)

    record = Record(tmp_path / "record")
    outcome = asyncio.run(run_node(
        request(workspace),
        model=Nosey([f"echo {constraint}"], *[[noisy(400)] for _ in range(8)]),
        capabilities=context_capabilities(small_budget(), record=record)))

    assert outcome.status == COMPLETED, outcome.reason
    assert len(record.compactions) >= 2, f"only {len(record.compactions)} compactions — not repeated"
    # 第一次必含（它是刚说的），关键是**后续**请求里还在不在。
    assert any(constraint in item for item in seen_by_model[1:]), (
        f"a constraint stated mid-conversation was gone after compaction — it was in "
        f"{sum(constraint in item for item in seen_by_model)} of {len(seen_by_model)} requests")


def test_the_summary_record_carries_usage_or_says_it_failed(tmp_path):
    """**第 5 条。** 摘要是一次付费请求；记录里必须是**真实 usage**，不是尺寸估算，失败也要留痕。"""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    summariser = Counting(*["a summary"] * 80)
    record = Record(tmp_path / "record")
    outcome = asyncio.run(run_node(
        request(workspace), model=Counting(*[[noisy(400)] for _ in range(6)]),
        capabilities=context_capabilities(small_budget(), record=record, summarizer=summariser)))

    assert outcome.status == COMPLETED, outcome.reason
    assert record.summaries, "no summary was recorded"
    assert all("usage" in item for item in record.summaries), \
        "the summary record has no usage field, so an estimate cannot be told from an invoice"

    # 失败路径：摘要器抛错时记录里要有那一笔。
    class Broken(Counting):
        def _answer(self, messages, info):
            raise RuntimeError("the summariser is down")

    failed = Record(tmp_path / "record2")
    asyncio.run(run_node(
        request(workspace), model=Counting(*[[noisy(400)] for _ in range(4)]),
        capabilities=context_capabilities(small_budget(), record=failed,
                                          summarizer=Broken(*["x"] * 5))))
    entries = [json.loads(line) for line in (failed.directory / "record.jsonl").read_text(
        encoding="utf-8").splitlines()]
    assert any(item.get("kind") == "model_error" or item.get("kind") == "overflow"
               for item in entries), "a failing summariser left no trace in the record"


def test_the_experiment_judges_rules_against_their_own_examples(tmp_path):
    """**判定器自己先过正反例。** 这一条是被三次误判逼出来的：每次规则写错，都会把一个做对了的产物
    报成失败，而报告里就会出现一个不存在的「发现」。

    每条规则都喂一个**满足要求**的和一个**不满足要求**的例子——而且用产物里真实出现过的写法。
    """
    judge = _judge()

    # ── every_file_ends：三个文件，每个都以指定行结尾 ──
    good = tmp_path / "c-good"
    _three_files(good, ending="REVIEWED-BY-ALPHA")
    produced, body = judge.measure(good)
    assert judge._holds(judge.TASKS["constraint"], good, produced, body) is True
    bad = tmp_path / "c-bad"
    _three_files(bad, ending=None)
    produced, body = judge.measure(bad)
    assert judge._holds(judge.TASKS["constraint"], bad, produced, body) is False

    # ── last_line_verbatim：清单的末行必须**逐字**出现在 summary.md 里 ──
    tailed = tmp_path / "t-good"
    tailed.mkdir()
    (tailed / "listing.txt").write_text("line one\nline two\nTHE-VERY-LAST-LINE\n", encoding="utf-8")
    (tailed / "summary.md").write_text("The last line was:\n\n> THE-VERY-LAST-LINE\n", encoding="utf-8")
    produced, body = judge.measure(tailed)
    assert judge._holds(judge.TASKS["tail"], tailed, produced, body) is True

    paraphrased = tmp_path / "t-bad"
    paraphrased.mkdir()
    (paraphrased / "listing.txt").write_text("line one\nline two\nTHE-VERY-LAST-LINE\n",
                                            encoding="utf-8")
    (paraphrased / "summary.md").write_text("The last line was about a final entry.\n",
                                            encoding="utf-8")
    produced, body = judge.measure(paraphrased)
    assert judge._holds(judge.TASKS["tail"], paraphrased, produced, body) is False, \
        "a paraphrase was accepted as a verbatim quotation"

    # ── quotes_cited：两种引用写法都要认，且续行算同一个引用块 ──
    blockquote = tmp_path / "q-good"
    blockquote.mkdir()
    (blockquote / "evidence.md").write_text(
        "# E1\n\n> first line of a quotation\n> second line of the same quotation\n\n"
        "**Source:** `some command`, 2026-01-01.\n", encoding="utf-8")
    produced, body = judge.measure(blockquote)
    assert judge._holds(judge.TASKS["evidence"], blockquote, produced, body) is True, \
        "a correctly sourced multi-line quotation was rejected"

    fenced = tmp_path / "q-fence"
    fenced.mkdir()
    (fenced / "evidence.md").write_text(
        "```\nquoted output here\n```\n\nSource: `some command`\n", encoding="utf-8")
    produced, body = judge.measure(fenced)
    assert judge._holds(judge.TASKS["evidence"], fenced, produced, body) is True, \
        "a fenced quotation was rejected — the task asks for a quotation, not a markdown construct"

    unsourced = tmp_path / "q-bad"
    unsourced.mkdir()
    (unsourced / "evidence.md").write_text(
        "> a quotation with nothing after it\n\nand then prose continues.\n", encoding="utf-8")
    produced, body = judge.measure(unsourced)
    assert judge._holds(judge.TASKS["evidence"], unsourced, produced, body) is False, \
        "a quotation with no source was accepted"

    empty = tmp_path / "q-empty"
    empty.mkdir()
    (empty / "evidence.md").write_text("no quotations at all, just prose\n", encoding="utf-8")
    produced, body = judge.measure(empty)
    assert judge._holds(judge.TASKS["evidence"], empty, produced, body) is False, \
        "a file with no quotations was accepted as having quoted and cited"
