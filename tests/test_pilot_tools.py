from __future__ import annotations

import asyncio
import json
import re
from pathlib import Path

from pydantic_ai import Agent, DeferredToolRequests
from pydantic_ai.messages import (ModelRequest, ModelResponse, TextPart, ToolCallPart, ToolReturnPart,
                                  UserPromptPart)
from pydantic_ai.models.function import FunctionModel

from anchor.pilot import PilotDeps, _register_tools
from anchor.session import SessionStore


class _Library:
    def catalog(self):
        return [{"id": "academic", "available": True}]

    def detail(self, plugin):
        return {"id": plugin, "instructions": "use it"}


class _Sessions:
    def __init__(self):
        self.attached = []
        self.events = []

    def attach_run(self, session, run):
        self.attached.append((session, run))

    def append(self, session, kind, data):
        self.events.append((session, kind, data))

    def get(self, session):
        from anchor.session import Session
        from datetime import datetime, timezone
        now = datetime.now(timezone.utc)
        return Session(id=session, conversation_id=session, created_at=now, updated_at=now)


class _Scheduler:
    def __init__(self, root: Path):
        self.root = root
        self.library = _Library()
        self.sessions = _Sessions()
        self.running = {}
        self.created = 0
        self.triggers = 0
        self.triggered = []
        self.controls = []
        self.deleted = []

    def workspaces(self):
        return []

    def workspace(self, name):
        path = self.root / "workspaces" / name
        return path if path.is_dir() else None

    def runs(self):
        return [{"run": "r1", "status": "finished"}]

    def run(self, graph, run):
        return {"run": run, "state": {"status": "finished"}}

    def trigger(self, graph, objective):
        self.triggers += 1
        self.triggered.append(graph)
        return json.dumps({"run": "r1", "graph": graph}), 202

    def create(self, name, definition):
        self.created += 1
        return json.dumps({"graph": name, "saved": True}), 200

    def save(self, name, definition):
        workspace = self.root / "workspaces" / name
        workspace.mkdir(parents=True, exist_ok=True)
        (workspace / "graph.json").write_text(json.dumps(definition), encoding="utf-8")
        return json.dumps({"graph": name, "saved": True}), 200

    def control_run(self, run, action):
        self.controls.append((run, action))
        return json.dumps({"run": run, "asked": action}), 202

    def delete_graph(self, name):
        self.deleted.append(name)
        return json.dumps({"graph": name, "deleted": True}), 200

    def read_file(self, run, node, path):
        return json.dumps({"path": path, "text": "evidence"}), 200


def test_pilot_registers_explicit_tools_and_can_call_them(tmp_path):
    scheduler = _Scheduler(tmp_path)
    calls = {"count": 0}

    def model(messages, info):
        calls["count"] += 1
        names = {item.name for item in info.function_tools}
        assert all(re.fullmatch(r"[a-zA-Z0-9_-]+", name) for name in names)
        assert "_run_control" not in names
        if calls["count"] == 1:
            assert {item.name for item in info.function_tools} >= {"graph_list", "graph_run",
                                                                  "plugin_read", "run_stop"}
            return ModelResponse(parts=[ToolCallPart(tool_name="run_list", args={})])
        return ModelResponse(parts=[TextPart(content="已读取运行历史")])

    agent = Agent(FunctionModel(model), output_type=str)
    _register_tools(agent)
    result = asyncio.run(agent.run("请查看运行历史", deps=PilotDeps(scheduler, "pilot")))

    assert result.output == "已读取运行历史"
    assert calls["count"] == 2


def _tool_then_answer(name: str, args: dict):
    """Call one control tool once, then answer; a replayed run starts from a clean history."""
    def model(messages, info):
        answered = any(isinstance(part, ToolReturnPart)
                       for message in messages if isinstance(message, ModelRequest)
                       for part in message.parts)
        return (ModelResponse(parts=[TextPart(content="已处理")]) if answered
                else ModelResponse(parts=[ToolCallPart(tool_name=name, args=args)]))
    return model


def _run_once(scheduler, model, history=None, deferred=None, prompt: str | None = "操作"):
    agent = Agent(FunctionModel(model), output_type=[str, DeferredToolRequests])
    _register_tools(agent)
    kwargs = {"message_history": history} if history is not None else {}
    if deferred is not None:
        kwargs["deferred_tool_results"] = deferred
    return asyncio.run(agent.run(prompt, deps=PilotDeps(scheduler, "pilot"), **kwargs))


def _replay(call_id: str, name: str, args: dict):
    """A history that hands the framework the same tool call again, as a crash retry would."""
    return [
        ModelRequest(parts=[UserPromptPart(content="操作")]),
        ModelResponse(parts=[ToolCallPart(tool_name=name, args=args, tool_call_id=call_id)]),
    ]


def test_graph_run_executes_once_for_the_users_request_and_is_recorded(tmp_path):
    scheduler = _Scheduler(tmp_path)
    store = SessionStore(tmp_path)
    store.create("pilot")
    scheduler.sessions = store
    model = _tool_then_answer("graph_run", {"graph": "academic"})

    settled = _run_once(scheduler, model)
    assert settled.output == "已处理"
    assert scheduler.triggers == 1
    assert store.get("pilot").run_ids == ["r1"]
    assert "run.started" in [event.kind for event in store.events("pilot")]

    # Replaying the same call after a crash reads the recorded result, not the side effect.
    call_id = next(iter(store.get("pilot").operations))
    _run_once(scheduler, _tool_then_answer("graph_run", {"graph": "academic"}),
              history=_replay(call_id, "graph_run", {"graph": "academic"}), prompt=None)
    assert scheduler.triggers == 1
    assert store.get("pilot").run_ids == ["r1"]


def test_run_control_executes_and_is_recorded_once(tmp_path):
    scheduler = _Scheduler(tmp_path)
    store = SessionStore(tmp_path)
    store.create("pilot")
    scheduler.sessions = store
    model = _tool_then_answer("run_stop", {"run": "r1"})

    assert _run_once(scheduler, model).output == "已处理"
    assert scheduler.controls == [("r1", "stop")]
    assert "run.stop.asked" in [event.kind for event in store.events("pilot")]

    call_id = next(iter(store.get("pilot").operations))
    _run_once(scheduler, model, history=_replay(call_id, "run_stop", {"run": "r1"}), prompt=None)
    assert scheduler.controls == [("r1", "stop")]


def test_graph_create_executes_and_is_recorded_once(tmp_path):
    scheduler = _Scheduler(tmp_path)
    store = SessionStore(tmp_path)
    store.create("pilot")
    scheduler.sessions = store
    definition = {"nodes": []}
    model = _tool_then_answer("graph_create", {"name": "new", "definition": definition})

    assert _run_once(scheduler, model).output == "已处理"
    assert scheduler.created == 1

    call_id = next(iter(store.get("pilot").operations))
    _run_once(scheduler, model, history=_replay(call_id, "graph_create",
                                                {"name": "new", "definition": definition}), prompt=None)
    assert scheduler.created == 1


def test_a_deleting_call_still_waits_for_the_user(tmp_path):
    """The destructive call keeps the framework's human gate; the ordinary ones no longer pay for it."""
    scheduler = _Scheduler(tmp_path)
    store = SessionStore(tmp_path)
    store.create("pilot")
    scheduler.sessions = store
    model = _tool_then_answer("graph_delete", {"graph": "academic"})

    first = _run_once(scheduler, model)
    assert isinstance(first.output, DeferredToolRequests)
    assert [call.tool_name for call in first.output.approvals] == ["graph_delete"]
    assert scheduler.deleted == []

    call_id = first.output.approvals[0].tool_call_id
    denied = first.output.build_results(approvals={call_id: False})
    assert _run_once(scheduler, model, history=first.all_messages(), deferred=denied).output == "已处理"
    assert scheduler.deleted == [], "a refused deletion must not run"

    approved = first.output.build_results(approvals={call_id: True})
    _run_once(scheduler, model, history=first.all_messages(), deferred=approved)
    assert scheduler.deleted == ["academic"]
    _run_once(scheduler, model, history=first.all_messages(), deferred=approved)
    assert scheduler.deleted == ["academic"], "a replayed approval must not delete twice"


def test_two_calls_in_one_response_each_keep_their_own_record(tmp_path):
    scheduler = _Scheduler(tmp_path)
    store = SessionStore(tmp_path)
    store.create("pilot")
    scheduler.sessions = store

    def model(messages, info):
        answered = any(isinstance(part, ToolReturnPart)
                       for message in messages if isinstance(message, ModelRequest)
                       for part in message.parts)
        if answered:
            return ModelResponse(parts=[TextPart(content="都处理完了")])
        return ModelResponse(parts=[ToolCallPart(tool_name="graph_run", args={"graph": name},
                                                 tool_call_id=f"call-{name}")
                                    for name in ("first", "second")])

    assert _run_once(scheduler, model).output == "都处理完了"
    # The two calls run concurrently, so they need not start in call order.
    assert sorted(scheduler.triggered) == ["first", "second"]
    # Each call keeps its own outcome, so replaying either one reads its own record.
    assert set(store.get("pilot").operations) == {"call-first", "call-second"}
    for name in ("first", "second"):
        assert store.begin_operation("pilot", "graph.run", f"call-{name}") == (
            "completed", {"run": "r1", "graph": name, "http_status": 202, "session": "pilot"})
    assert scheduler.triggers == 2


def test_an_operation_whose_outcome_is_unknown_is_not_replayed(tmp_path):
    scheduler = _Scheduler(tmp_path)
    store = SessionStore(tmp_path)
    store.create("pilot")
    scheduler.sessions = store
    # What a crash between the side effect and its record leaves behind.
    store.begin_operation("pilot", "graph.run", "call-first")
    model = _tool_then_answer("graph_run", {"graph": "first"})

    settled = _run_once(scheduler, model, history=_replay("call-first", "graph_run", {"graph": "first"}),
                        prompt=None)
    assert settled.output == "已处理"
    assert scheduler.triggers == 0, "an unknown outcome must not be replayed"
    returned = [part.content for message in settled.all_messages() if isinstance(message, ModelRequest)
                for part in message.parts if isinstance(part, ToolReturnPart)]
    assert returned and returned[0]["uncertain"] is True
