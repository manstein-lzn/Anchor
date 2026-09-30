"""Exercise the native streaming seam without a provider or platform send."""

from __future__ import annotations

import asyncio
import json
import threading
from dataclasses import replace

import pytest
from pydantic_ai import BinaryContent
from pydantic_ai.messages import ModelResponse, ToolCallPart, UserPromptPart
from pydantic_ai.models.function import DeltaThinkingPart, DeltaToolCall, FunctionModel
from pydantic_ai.toolsets import FunctionToolset

from anchor.node import BUDGET_EXHAUSTED, COMPLETED, FAILED, NodeRequest
from anchor.node.adapter import run_node
from anchor.node.recovery import completion_path, load_budget, open_store
from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox


@pytest.fixture(autouse=True)
def needs_a_sandbox():
    try:
        BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    except RuntimeError as exc:
        pytest.skip(f"no usable sandbox on this machine: {exc}")


def request(tmp_path, **kwargs):
    workspace = tmp_path / "workspace"
    workspace.mkdir(exist_ok=True)
    return NodeRequest(execution_id="answer", task="Explain the attached image", workspace=workspace,
                       **kwargs)


def test_incremental_summary_precedes_completion_and_native_history_is_reusable(tmp_path):
    from pydantic_ai_harness.step_persistence import continue_run

    control = tmp_path / "control"
    updates = []
    image_bytes = b"\x89PNG\r\n\x1a\nmodel-input"
    received = asyncio.Event()

    def emit(text):
        assert not completion_path(control).exists(), "previews must not persist a completion"
        updates.append(text)
        received.set()

    async def streaming(messages, info):
        user = next(part for msg in messages for part in msg.parts if isinstance(part, UserPromptPart))
        assert user.content[0] == "Explain the attached image"
        image = user.content[1]
        assert isinstance(image, BinaryContent)
        assert image.data == image_bytes and image.media_type == "image/png"
        yield {0: DeltaToolCall(name="final_result", json_args='{"summary":"图像')}
        await asyncio.wait_for(received.wait(), timeout=2)
        assert updates == ["图像"]
        assert load_budget(control).requests_used == 1
        yield {0: DeltaToolCall(json_args='显示\\n一只猫", "route":null}')}
        assert not completion_path(control).exists()

    req = request(tmp_path, on_output=emit, prompt_images=((image_bytes, "image/png"),),
                  conversation_id="member-a", trace=tmp_path / "trace.jsonl")
    result = asyncio.run(run_node(req, model=FunctionModel(stream_function=streaming), recovery_store=control))
    assert result.status == COMPLETED, result.reason
    assert updates == ["图像", "图像显示\n一只猫"]
    assert result.submission == updates[-1] and result.model_requests == 1
    assert json.loads(completion_path(control).read_text())["submission"] == result.submission

    async def read_history():
        store = open_store(control)
        runs = await store.list_runs(conversation_id="member-a")
        assert len(runs) == 1
        return await continue_run(store, run_id=runs[0].run_id, include_interrupted=True)

    history = asyncio.run(read_history())
    assert any(isinstance(part, ToolCallPart) and part.tool_name == "final_result"
               for msg in history for part in msg.parts)
    saved_images = [content for msg in history for part in msg.parts if isinstance(part, UserPromptPart)
                    and isinstance(part.content, list) for content in part.content if isinstance(content, BinaryContent)]
    assert saved_images[0].data == image_bytes

    def unexpected(messages, info):
        pytest.fail("a recovered completion must not ask the provider again")

    recovered = asyncio.run(run_node(replace(req, recovery=result.recovery, on_output=None),
                                    model=FunctionModel(unexpected), recovery_store=control))
    assert recovered.status == COMPLETED and recovered.model_requests == 0
    assert recovered.submission == result.submission


def test_only_summary_is_exposed_and_extra_toolsets_join_mcp(tmp_path, monkeypatch):
    updates, called = [], []
    mcp = FunctionToolset()
    extra = FunctionToolset()

    @mcp.tool_plain
    def existing_tool(secret: str) -> str:
        called.append("mcp")
        return "PRIVATE TOOL RESULT"

    @extra.tool_plain
    def scoped_tool(secret: str) -> str:
        called.append("extra")
        return "PRIVATE TOOL RESULT"

    monkeypatch.setattr("anchor.node.mcp.toolsets_for", lambda *args, **kwargs: (mcp,))
    count = 0

    async def streaming(messages, info):
        nonlocal count
        count += 1
        assert {tool.name for tool in info.function_tools} == {"bash", "existing_tool", "scoped_tool"}
        yield {8: DeltaThinkingPart(content="PRIVATE REASONING")}
        yield "PRIVATE ASSISTANT TEXT"
        if count == 1:
            yield {0: DeltaToolCall(name="existing_tool", json_args='{"secret":"PRIVATE ARGS"}'),
                   1: DeltaToolCall(name="scoped_tool", json_args='{"secret":"PRIVATE ARGS"}')}
        else:
            yield {0: DeltaToolCall(name="final_result", json_args='{"summary":"Public')}
            yield {0: DeltaToolCall(json_args=' answer", "route":null, "private":"PRIVATE OTHER FIELD"}')}

    result = asyncio.run(run_node(request(tmp_path, on_output=updates.append, toolsets=(extra,)),
                                   model=FunctionModel(stream_function=streaming)))
    assert result.status == COMPLETED, result.reason
    assert sorted(called) == ["extra", "mcp"]
    assert updates == ["Public", "Public answer"]
    assert result.model_requests == 2


def test_cancellation_closes_stream_records_partial_and_does_not_complete(tmp_path):
    from pydantic_ai_harness.step_persistence import continue_run

    control = tmp_path / "control"
    stopping = threading.Event()
    updates, closed = [], []

    def emit(text):
        updates.append(text)
        stopping.set()

    async def streaming(messages, info):
        try:
            yield {0: DeltaToolCall(name="final_result", json_args='{"summary":"unfinished')}
            await asyncio.Event().wait()
        finally:
            closed.append(True)

    req = request(tmp_path, on_output=emit, cancelled=stopping.is_set, conversation_id="member-a",
                  trace=tmp_path / "trace.jsonl", max_requests=2)

    async def run():
        return await asyncio.wait_for(run_node(req, model=FunctionModel(stream_function=streaming),
                                               recovery_store=control), timeout=3)

    result = asyncio.run(run())
    assert result.status == FAILED and result.reason == "stopped on request"
    assert result.model_requests == 1 and load_budget(control).requests_used == 1
    assert updates == ["unfinished"] and closed == [True]
    assert not completion_path(control).exists()
    assert '"state": "interrupted"' in req.trace.read_text()

    async def read_history():
        store = open_store(control)
        runs = await store.list_runs(conversation_id="member-a")
        return await continue_run(store, run_id=runs[0].run_id, include_interrupted=True)

    history = asyncio.run(read_history())
    assert history

    async def resumed(messages, info):
        assert any(isinstance(part, UserPromptPart) and part.content == req.task
                   for msg in messages[:-1] for part in msg.parts)
        yield {0: DeltaToolCall(name="final_result", json_args='{"summary":"continued safely"}')}

    result = asyncio.run(run_node(replace(req, task="Continue with this new message", cancelled=None,
                                         on_output=updates.append, previous_steps=(control,)),
                                   model=FunctionModel(stream_function=resumed),
                                   recovery_store=tmp_path / "next-control"))
    assert result.status == COMPLETED, result.reason
    assert result.submission == "continued safely" and result.model_requests == 1


def test_streaming_request_budget_is_charged_once_and_stops_next_request(tmp_path):
    calls = []

    async def streaming(messages, info):
        calls.append(1)
        yield {0: DeltaToolCall(name="bash", json_args='{"command":"true"}')}

    control = tmp_path / "control"
    req = request(tmp_path, on_output=lambda text: pytest.fail("tool call is not public output"), max_requests=1)
    result = asyncio.run(run_node(req, model=FunctionModel(stream_function=streaming), recovery_store=control))
    assert result.status == BUDGET_EXHAUSTED
    assert result.model_requests == 1 and calls == [1]
    assert load_budget(control).requests_used == 1
    assert not completion_path(control).exists()
    recovered = asyncio.run(run_node(replace(req, recovery=result.recovery),
                                    model=FunctionModel(stream_function=streaming), recovery_store=control))
    assert recovered.status == BUDGET_EXHAUSTED and recovered.model_requests == 0
    assert calls == [1] and load_budget(control).requests_used == 1


def test_explicit_nonstream_model_fallback_emits_validated_summary(tmp_path):
    updates = []

    def model(messages, info):
        return ModelResponse(parts=[ToolCallPart(tool_name="final_result", args={"summary": "done"})])

    result = asyncio.run(run_node(request(tmp_path, on_output=updates.append), model=FunctionModel(model)))
    assert result.status == COMPLETED and result.model_requests == 1
    assert updates == ["done"]


def test_stream_failure_does_not_retry_as_nonstream(tmp_path):
    calls = []

    def nonstream(messages, info):
        pytest.fail("a streaming transport failure cannot fall back to a second request")

    async def streaming(messages, info):
        calls.append(1)
        raise RuntimeError("transport failed")
        yield "unreachable"

    control = tmp_path / "control"
    result = asyncio.run(run_node(request(tmp_path, on_output=lambda text: None),
                                   model=FunctionModel(nonstream, stream_function=streaming), recovery_store=control))
    assert result.status == FAILED and "transport failed" in result.reason
    assert result.model_requests == 1 and calls == [1]
    assert load_budget(control).requests_used == 1
