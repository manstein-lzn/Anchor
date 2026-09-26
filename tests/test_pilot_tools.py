from __future__ import annotations

import asyncio
import json
import re
from pathlib import Path

from pydantic_ai import Agent, DeferredToolRequests
from pydantic_ai.messages import ModelRequest, ModelResponse, TextPart, ToolCallPart, ToolReturnPart
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


def _run_once(scheduler, model, history=None, deferred=None):
    agent = Agent(FunctionModel(model), output_type=[str, DeferredToolRequests])
    _register_tools(agent)
    kwargs = {"message_history": history} if history is not None else {}
    if deferred is not None:
        kwargs["deferred_tool_results"] = deferred
    return asyncio.run(agent.run("操作", deps=PilotDeps(scheduler, "pilot"), **kwargs))


def test_graph_run_requires_confirmation_and_starts_only_once(tmp_path):
    scheduler = _Scheduler(tmp_path)
    store = SessionStore(tmp_path)
    store.create("pilot")
    scheduler.sessions = store
    model = _tool_then_answer("graph_run", {"graph": "academic"})

    first = _run_once(scheduler, model)
    assert isinstance(first.output, DeferredToolRequests)
    assert [call.tool_name for call in first.output.approvals] == ["graph_run"]
    assert scheduler.triggers == 0, "a Run must not start before the user confirms"

    call_id = first.output.approvals[0].tool_call_id
    approved = first.output.build_results(approvals={call_id: True})
    settled = _run_once(scheduler, model, history=first.all_messages(), deferred=approved)
    assert settled.output == "已处理"
    assert scheduler.triggers == 1
    assert store.get("pilot").run_ids == ["r1"]
    assert "run.started" in [event.kind for event in store.events("pilot")]

    # Replaying the same approved call after a crash reads the recorded result, not the side effect.
    _run_once(scheduler, model, history=first.all_messages(), deferred=approved)
    assert scheduler.triggers == 1
    assert store.get("pilot").run_ids == ["r1"]


def test_run_control_requires_confirmation_and_is_recorded_once(tmp_path):
    scheduler = _Scheduler(tmp_path)
    store = SessionStore(tmp_path)
    store.create("pilot")
    scheduler.sessions = store
    model = _tool_then_answer("run_stop", {"run": "r1"})

    first = _run_once(scheduler, model)
    assert isinstance(first.output, DeferredToolRequests)
    assert [call.tool_name for call in first.output.approvals] == ["run_stop"]
    assert scheduler.controls == []

    approved = first.output.build_results(approvals={first.output.approvals[0].tool_call_id: True})
    _run_once(scheduler, model, history=first.all_messages(), deferred=approved)
    assert scheduler.controls == [("r1", "stop")]
    assert "run.stop.asked" in [event.kind for event in store.events("pilot")]

    _run_once(scheduler, model, history=first.all_messages(), deferred=approved)
    assert scheduler.controls == [("r1", "stop")]


def test_graph_create_requires_user_approval_and_consumes_it_once(tmp_path):
    scheduler = _Scheduler(tmp_path)
    store = SessionStore(tmp_path)
    store.create("pilot")
    scheduler.sessions = store
    definition = {"nodes": []}
    model = _tool_then_answer("graph_create", {"name": "new", "definition": definition})

    first = _run_once(scheduler, model)
    assert isinstance(first.output, DeferredToolRequests)
    assert scheduler.created == 0
    call_id = first.output.approvals[0].tool_call_id

    # A refusal is a real outcome the model can read, not a silent no-op.
    denied = first.output.build_results(approvals={call_id: False})
    assert _run_once(scheduler, model, history=first.all_messages(), deferred=denied).output == "已处理"
    assert scheduler.created == 0

    approved = first.output.build_results(approvals={call_id: True})
    _run_once(scheduler, model, history=first.all_messages(), deferred=approved)
    assert scheduler.created == 1
    _run_once(scheduler, model, history=first.all_messages(), deferred=approved)
    assert scheduler.created == 1


def test_two_pending_approvals_do_not_overwrite_each_other(tmp_path):
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
        return ModelResponse(parts=[ToolCallPart(tool_name="graph_run", args={"graph": name})
                                    for name in ("first", "second")])

    first = _run_once(scheduler, model)
    assert isinstance(first.output, DeferredToolRequests)
    graphs = {call.tool_call_id: call.args["graph"] for call in first.output.approvals}
    assert sorted(graphs.values()) == ["first", "second"]
    assert scheduler.triggers == 0

    approved = first.output.build_results(approvals={call_id: True for call_id in graphs})
    assert _run_once(scheduler, model, history=first.all_messages(),
                     deferred=approved).output == "都处理完了"
    # Deferred calls resolve concurrently, so the two independent Runs need not start in call order.
    assert sorted(scheduler.triggered) == ["first", "second"]
    # Each confirmed call keeps its own outcome, so replaying either one reads its own record.
    assert set(store.get("pilot").operations) == set(graphs)
    for call_id, graph in graphs.items():
        assert store.begin_operation("pilot", "graph.run", call_id) == (
            "completed", {"run": "r1", "graph": graph, "http_status": 202, "session": "pilot"})
    assert scheduler.triggers == 2


def test_two_approved_edits_of_one_graph_do_not_both_apply(tmp_path):
    from anchor.pilot import approval_precondition

    scheduler = _Scheduler(tmp_path)
    store = SessionStore(tmp_path)
    store.create("pilot")
    scheduler.sessions = store
    workspace = tmp_path / "workspaces" / "demo"
    workspace.mkdir(parents=True)
    graph = workspace / "graph.json"
    graph.write_text(json.dumps({"nodes": ["original"]}), encoding="utf-8")

    def model(messages, info):
        answered = any(isinstance(part, ToolReturnPart)
                       for message in messages if isinstance(message, ModelRequest)
                       for part in message.parts)
        if answered:
            return ModelResponse(parts=[TextPart(content="处理完了")])
        return ModelResponse(parts=[
            ToolCallPart(tool_name="graph_update",
                         args={"graph": "demo", "definition": {"nodes": [name]}})
            for name in ("first", "second")])

    first = _run_once(scheduler, model)
    assert len(first.output.approvals) == 2
    # This is what `serve` records while the run is paused; both calls saw the same original file.
    store.set_pending("pilot", [
        {"tool_call_id": call.tool_call_id, "key": call.tool_call_id, "action": call.tool_name,
         "precondition": approval_precondition(scheduler, call.tool_name, call.args)}
        for call in first.output.approvals])
    approved = first.output.build_results(
        approvals={call.tool_call_id: True for call in first.output.approvals})
    assert _run_once(scheduler, model, history=first.all_messages(),
                     deferred=approved).output == "处理完了"
    # Serialized check-and-act: the second edit sees the first one's file and refuses instead of
    # silently overwriting it.
    assert json.loads(graph.read_text(encoding="utf-8"))["nodes"] in (["first"], ["second"])
    assert len(store.get("pilot").operations) == 1
