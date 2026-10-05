"""The existing Session/Harness owns dialogue while Rust HTTP owns Graph Runs."""

import asyncio
import json
import time

from pydantic_ai import Agent, DeferredToolRequests
from pydantic_ai.messages import ToolReturnPart
from pydantic_ai.models.function import DeltaToolCall, FunctionModel

from anchor import pilot
from anchor.pilot import _register_tools
from anchor.serve import Scheduler
from test_platform_rust_backend import platform, request  # noqa: F401


def install_model(monkeypatch, stream):
    def agent(_):
        result = Agent(FunctionModel(stream_function=stream), output_type=[str, DeferredToolRequests])
        _register_tools(result)
        return result

    monkeypatch.setattr(pilot, "_agent", agent)


def settle(scheduler, session, turn):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        result = scheduler.turns.get(session, turn)
        if result["status"] != "running":
            return result
        time.sleep(0.02)
    raise AssertionError("Pilot turn did not settle")


def submit(frontend, session, identifier, message=None):
    body = {"request_id": identifier, **({"message": message} if message is not None else {"resume": True})}
    status, response, _ = request(frontend, "POST", f"/sessions/{session}/turns", body)
    assert status == 202, response
    return response["turn"]["id"]


def returned(messages, tool):
    return [part.content for message in messages for part in message.parts
            if isinstance(part, ToolReturnPart) and part.tool_name == tool]


def test_rust_pilot_uses_session_stream_and_same_run_after_restart(platform, monkeypatch):  # noqa: F811
    scheduler, runtime, frontend, _ = platform
    scheduler.create_session("pilot")
    calls = []

    async def start(messages, info):
        calls.append(messages)
        if not returned(messages, "graph_run"):
            yield {0: DeltaToolCall(name="graph_run", json_args='{"graph":"demo"}', tool_call_id="start")}
        elif not returned(messages, "run_pause"):
            yield {0: DeltaToolCall(name="run_pause", json_args='{"run":"opaque-1"}', tool_call_id="pause")}
        else:
            assert returned(messages, "run_pause")[-1]["http_status"] == 202
            yield "已启动并请求暂停。"

    install_model(monkeypatch, start)
    turn = submit(frontend, "pilot", "request-1", "运行并暂停")
    assert settle(scheduler, "pilot", turn)["status"] == "completed"
    assert scheduler.sessions.get("pilot").run_ids == ["opaque-1"]
    assert runtime.runs["opaque-1"]["state"]["status"] == "paused"
    assert scheduler.running == scheduler.control == {}
    count = len(calls)
    assert submit(frontend, "pilot", "request-1", "运行并暂停") == turn
    assert len(calls) == count
    status, events, _ = request(frontend, "GET", f"/sessions/pilot/turns/{turn}/events")
    assert status == 200 and b"graph_run" in events and b"run_pause" in events
    runtime.files[("opaque-1", "module/work", "result.txt")] = b"rust-evidence"
    reopened = Scheduler(scheduler.root, scheduler.config)
    frontend.RequestHandlerClass.scheduler = reopened
    before = request(frontend, "GET", "/sessions/pilot/messages")[1]
    assert "已启动并请求暂停" in json.dumps(before, ensure_ascii=False)
    assert len(calls) == count

    async def inspect(messages, info):
        if not returned(messages, "session_wait"):
            yield {0: DeltaToolCall(name="session_wait", json_args="{}", tool_call_id="inspect")}
        elif not returned(messages, "artifact_read"):
            value = returned(messages, "session_wait")[-1]
            assert value["session"]["run_ids"] == ["opaque-1"]
            assert value["runs"][0]["backend"] == "rust"
            yield {0: DeltaToolCall(name="artifact_read", json_args=json.dumps({
                "run": "opaque-1", "node": "module/work", "path": "result.txt"}), tool_call_id="file")}
        else:
            assert returned(messages, "artifact_read")[-1]["text"] == "rust-evidence"
            yield "已重开原会话并核查 Rust 产物。"

    install_model(monkeypatch, inspect)
    next_turn = submit(frontend, "pilot", "request-2", "查看上次产物")
    assert settle(reopened, "pilot", next_turn)["status"] == "completed"
    assert len(runtime.runs) == 1
    assert not list(scheduler.root.glob("workspaces/*/runs/*/run.json"))
    assert len([item for item in runtime.requests if item["path"] == "/trigger"]) == 1


def test_rust_session_run_attachment_and_validation_use_public_runtime(platform):  # noqa: F811
    scheduler, runtime, frontend, _ = platform
    scheduler.create_session("attached")
    definition = runtime.definition
    status, value, _ = request(frontend, "POST", "/graph-validation", {"definition": definition})
    assert status == 200 and value["valid"] is True
    assert runtime.requests[-1]["path"] == "/graph-validation"
    assert len(runtime.graphs) == 1 and not runtime.runs
    status, value, _ = request(frontend, "POST", "/graph-validation", {"definition": []})
    assert status == 400 and value["valid"] is False
    run = json.loads(scheduler.trigger("demo", None)[0])["run"]
    assert request(frontend, "POST", "/sessions/attached/runs", {"run": run})[0] == 200
    assert scheduler.sessions.get("attached").run_ids == [run]
    runtime.unavailable = True
    assert request(frontend, "POST", "/sessions/attached/runs", {"run": "missing"})[0] == 503
    assert scheduler.sessions.get("attached").run_ids == [run]


def test_rust_pilot_delete_confirmation_rechecks_public_definition(platform, monkeypatch):  # noqa: F811
    scheduler, runtime, frontend, _ = platform
    scheduler.create_session("delete")

    async def stream(messages, info):
        results = returned(messages, "graph_delete")
        if not results:
            yield {0: DeltaToolCall(name="graph_delete", json_args='{"graph":"demo"}', tool_call_id="delete-call")}
        else:
            assert results[-1]["changed"] is True
            yield "目标已改变，未删除。"

    install_model(monkeypatch, stream)
    turn = submit(frontend, "delete", "delete-1", "删除 demo")
    assert settle(scheduler, "delete", turn)["status"] == "waiting_approval"
    approval, = scheduler.sessions.get("delete").approvals
    changed = {**runtime.definition, "objective": "Changed after proposal"}
    assert request(frontend, "PUT", "/graphs/demo", {"definition": changed})[0] == 200
    assert request(frontend, "POST", "/sessions/delete/confirm", {
        "action": "graph_delete", "approval_key": approval["key"]})[0] == 200
    resumed = submit(frontend, "delete", "delete-2")
    assert settle(scheduler, "delete", resumed)["status"] == "completed"
    assert runtime.graphs["demo"] == changed
    assert not any(item["method"] == "DELETE" for item in runtime.requests)


def test_rust_pilot_can_stop_a_dialogue_without_starting_a_graph(platform, monkeypatch):  # noqa: F811
    scheduler, runtime, frontend, _ = platform
    scheduler.create_session("stop")

    async def stream(messages, info):
        yield "正在读取"
        await asyncio.sleep(30)
        yield "完成"

    install_model(monkeypatch, stream)
    turn = submit(frontend, "stop", "stop-1", "稍等一下")
    deadline = time.monotonic() + 10
    while not scheduler.turns.events("stop", turn) and time.monotonic() < deadline:
        time.sleep(0.02)
    assert request(frontend, "POST", "/sessions/stop/stop", {})[0] == 202
    assert settle(scheduler, "stop", turn)["status"] == "stopped"
    assert not runtime.runs


def test_rust_pilot_responses_facade_uses_existing_session_turns(platform, monkeypatch):  # noqa: F811
    scheduler, runtime, _frontend, _ = platform

    async def stream(messages, info):
        yield "普通 Pilot 对话。"

    install_model(monkeypatch, stream)
    first, status = scheduler.response_turn("owner", {"input": "你好"})
    assert status == 200
    assert settle(scheduler, first["session"], first["turn"])["status"] == "completed"
    other, status = scheduler.response_turn("another-owner", {"input": "继续", "previous_response_id": first["id"]})
    assert status == 404 and "error" in other
    next_response, status = scheduler.response_turn("owner", {"input": "继续", "previous_response_id": first["id"]})
    assert status == 200 and next_response["session"] == first["session"]
    assert settle(scheduler, first["session"], next_response["turn"])["status"] == "completed"
    assert not runtime.runs
