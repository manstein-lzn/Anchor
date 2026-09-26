from __future__ import annotations

import pytest
import threading
import json
from http.client import HTTPConnection
from http.server import ThreadingHTTPServer
from pydantic_ai import Agent
from pydantic_ai.messages import ModelResponse, TextPart
from pydantic_ai.models.function import FunctionModel

from anchor.session import SessionStore
from anchor.serve import Handler, Scheduler


def test_session_lifecycle_and_events_are_recoverable(tmp_path):
    store = SessionStore(tmp_path)
    session = store.create("research")

    assert session.status == "active"
    assert store.get("research").conversation_id == "research"
    assert [event.kind for event in store.events("research")] == ["session.created"]

    waiting = store.set_status("research", "waiting_user", reason="需要确认研究范围")
    assert waiting.waiting_reason == "需要确认研究范围"
    resumed = store.set_status("research", "active")
    assert resumed.status == "active"
    attached = store.attach_run("research", "run-1")
    assert attached.run_ids == ["run-1"]
    assert [event.seq for event in store.events("research")] == [1, 2, 3, 4]

    store.set_status("research", "archived")
    with pytest.raises(ValueError, match="archived"):
        store.set_status("research", "active")


def test_session_confirmation_is_bound_to_the_pending_call(tmp_path):
    store = SessionStore(tmp_path)
    store.create("confirm")
    store.set_pending("confirm", [
        {"tool_call_id": "call-1", "key": "call-1", "action": "graph.delete",
         "target": "demo", "proposal": {"delete": True}},
        {"tool_call_id": "call-2", "key": "call-2", "action": "graph.run",
         "target": "demo", "proposal": {"graph": "demo"}},
    ])
    assert store.get("confirm").status == "waiting_user"
    with pytest.raises(ValueError, match="matching"):
        store.decide_approval("confirm", "missing", True)
    with pytest.raises(ValueError, match="does not match"):
        store.decide_approval("confirm", "call-1", True, "graph.update")
    store.decide_approval("confirm", "call-1", True, "graph.delete")
    assert store.approval_decisions("confirm") == {"call-1": True}
    assert store.get("confirm").status == "waiting_user", "a second call is still waiting"
    store.decide_approval("confirm", "call-2", False, "graph.run")
    assert store.approval_decisions("confirm") == {"call-1": True, "call-2": False}
    assert store.get("confirm").status == "active"
    assert [event.kind for event in store.events("confirm")] == [
        "session.created", "session.confirmation.requested",
        "session.confirmation.requested", "session.confirmation.granted",
        "session.confirmation.rejected",
    ]


def test_approval_payload_and_operation_intent_survive_restart_without_replay(tmp_path):
    store = SessionStore(tmp_path)
    store.create("recover")
    proposal = {"current": {"nodes": ["old"]}, "definition": {"nodes": ["new"]}}
    store.set_pending("recover", [
        {"tool_call_id": "call-1", "key": "call-1", "action": "graph.update",
         "target": "demo", "proposal": proposal}])
    assert SessionStore(tmp_path).get("recover").approval["proposal"] == proposal
    store.decide_approval("recover", "call-1", True, "graph.update")

    assert store.begin_operation("recover", "graph.update", "call-1") == ("started", None)
    restarted = SessionStore(tmp_path)
    assert restarted.begin_operation("recover", "graph.update", "call-1") == ("uncertain", None)
    restarted.finish_operation("recover", "call-1", {"http_status": 200})
    assert restarted.begin_operation("recover", "graph.update", "call-1") == (
        "completed", {"http_status": 200})
    # A second confirmed call keeps its own record instead of overwriting the first.
    assert restarted.begin_operation("recover", "graph.run", "call-2") == ("started", None)
    assert restarted.begin_operation("recover", "graph.update", "call-1") == (
        "completed", {"http_status": 200})
    restarted.finish_operation("recover", "call-2", {"http_status": 202})
    assert restarted.begin_operation("recover", "graph.run", "call-2") == (
        "completed", {"http_status": 202})
    with pytest.raises(ValueError, match="pending"):
        restarted.finish_operation("recover", "call-9", {})
    assert set(restarted.get("recover").operations) == {"call-1", "call-2"}


def test_a_legacy_single_slot_approval_is_dropped_on_read(tmp_path):
    """The old flow's pending request named no framework call, so it cannot be resumed."""
    store = SessionStore(tmp_path)
    store.create("legacy")
    path = tmp_path / "sessions" / "legacy" / "session.json"
    raw = json.loads(path.read_text(encoding="utf-8"))
    raw.update({"approval": {"action": "graph.delete", "key": "old", "status": "requested"},
                "status": "waiting_user"})
    path.write_text(json.dumps(raw), encoding="utf-8")
    session = store.get("legacy")
    assert session.approval is None
    assert session.status == "active"


def test_concurrent_session_events_keep_unique_order(tmp_path):
    store = SessionStore(tmp_path)
    store.create("parallel")
    threads = [threading.Thread(target=store.append,
                                args=("parallel", "test.concurrent", {"n": index}))
               for index in range(32)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    events = store.events("parallel")
    assert [event.seq for event in events] == list(range(1, 34))


def test_session_delete_requires_archive_and_removes_conversation(tmp_path):
    store = SessionStore(tmp_path)
    store.create("remove-me")
    with pytest.raises(ValueError, match="archive"):
        store.delete("remove-me")

    store.set_status("remove-me", "archived")
    store.delete("remove-me")
    assert not (tmp_path / "sessions" / "remove-me").exists()
    import asyncio
    assert asyncio.run(store.conversation_store().listing()) == []


def test_scheduler_exposes_session_lifecycle_and_run_attachment(tmp_path):
    workspace = tmp_path / "workspaces" / "demo"
    run = workspace / "runs" / "run-1"
    run.mkdir(parents=True)
    (workspace / "graph.json").write_text('{"nodes": []}', encoding="utf-8")
    (run / "run.json").write_text('{"status": "finished"}', encoding="utf-8")
    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")

    body, status = scheduler.create_session("pilot")
    assert status == 201 and '"id": "pilot"' in body
    body, status = scheduler.attach_session_run("pilot", "run-1")
    assert status == 200 and "run-1" in body
    body, status = scheduler.set_session_status("pilot", "archived")
    assert status == 200 and "archived" in body
    body, status = scheduler.delete_session("pilot")
    assert status == 200 and '"deleted": true' in body


def test_scheduler_confirmation_endpoint_requires_pending_request(tmp_path):
    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")
    scheduler.create_session("pilot")
    body, status = scheduler.confirm_session("pilot", "graph.delete", "missing")
    assert status == 409 and "matching" in body
    scheduler.sessions.set_pending("pilot", [
        {"tool_call_id": "key-1", "key": "key-1", "action": "graph.delete",
         "target": "demo", "proposal": {"delete": True}}])
    body, status = scheduler.confirm_session("pilot", "graph.delete", "key-1")
    assert status == 200 and '"confirmed": true' in body


def test_pilot_conversation_persists_messages_and_can_resume_a_failed_turn(tmp_path, monkeypatch):
    from anchor import pilot

    calls = {"count": 0}

    def answer(messages, info):
        calls["count"] += 1
        if calls["count"] == 2:
            raise RuntimeError("provider interrupted")
        return ModelResponse(parts=[TextPart(content=f"reply {calls['count']}")])

    model = FunctionModel(answer)
    monkeypatch.setattr(pilot, "_agent", lambda _: Agent(model, output_type=str))
    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")
    scheduler.create_session("pilot")

    body, status = scheduler.pilot_message("pilot", "hello")
    assert status == 200 and "reply 1" in body
    body, status = scheduler.pilot_message("pilot", "second question")
    assert status == 502
    assert scheduler.sessions.get("pilot").status == "interrupted"
    # A failed turn does not lock the conversation: the next message is a new turn that carries the
    # question the provider never answered, so the model still knows what was asked.
    body, status = scheduler.pilot_message("pilot", "third question")
    assert status == 200 and "reply 3" in body
    assert scheduler.sessions.get("pilot").status == "active"
    assert SessionStore(tmp_path).get("pilot").title == "hello"

    body, status = scheduler.pilot_messages("pilot")
    assert status == 200
    messages = __import__("json").loads(body)["messages"]
    assert messages == [
        {"role": "user", "text": "hello"}, {"role": "assistant", "text": "reply 1"},
        {"role": "user", "text": "second question"}, {"role": "user", "text": "third question"},
        {"role": "assistant", "text": "reply 3"},
    ]


def test_pilot_stop_cancels_an_inflight_model_call(tmp_path, monkeypatch):
    import asyncio
    from anchor import pilot

    started = threading.Event()
    release = threading.Event()

    async def answer(messages, info):
        started.set()
        await asyncio.to_thread(release.wait)
        return ModelResponse(parts=[TextPart(content="late reply")])

    monkeypatch.setattr(pilot, "_agent", lambda _: Agent(FunctionModel(answer), output_type=str))
    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")
    scheduler.create_session("stop-me")
    outcome = []
    worker = threading.Thread(target=lambda: outcome.append(scheduler.pilot_message("stop-me", "hello")))
    worker.start()
    assert started.wait(30), "Pilot did not reach the model"
    assert scheduler.stop_pilot("stop-me")[1] == 202
    release.set()
    worker.join(5)

    assert not worker.is_alive()
    assert outcome[0][1] == 502
    assert scheduler.sessions.get("stop-me").status == "interrupted"


def test_pilot_waiting_user_can_resume_with_an_answer(tmp_path, monkeypatch):
    from anchor import pilot
    calls = {"count": 0}

    def answer(messages, info):
        calls["count"] += 1
        return ModelResponse(parts=[TextPart(content="已按你的回答继续")])

    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")
    scheduler.create_session("wait")
    # Exercise the lifecycle boundary directly; waiting is intentionally explicit and durable.
    scheduler.sessions.set_status("wait", "waiting_user", reason="请告诉我范围")
    monkeypatch.setattr(pilot, "_agent", lambda _: Agent(FunctionModel(answer), output_type=str))
    body, status = scheduler.pilot_message("wait", "限定在近五年")
    assert status == 200 and "已按你的回答继续" in body
    assert scheduler.sessions.get("wait").status == "active"


def test_pilot_http_create_send_and_read_history(tmp_path, monkeypatch):
    from anchor import pilot

    monkeypatch.setattr(pilot, "_agent", lambda _: Agent(
        FunctionModel(lambda messages, info: ModelResponse(parts=[TextPart(content="welcome")])),
        output_type=str))
    Handler.scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    connection = HTTPConnection(*server.server_address)
    try:
        connection.request("POST", "/sessions", json.dumps({"id": "http"}),
                           {"Content-Type": "application/json"})
        assert connection.getresponse().status == 201
        Handler.scheduler.sessions.set_pending("http", [
            {"tool_call_id": "key-http", "key": "key-http", "action": "graph.delete",
             "target": "demo", "proposal": {"delete": True}}])
        connection.request("POST", "/sessions/http/confirm", json.dumps({
            "action": "graph.delete", "approval_key": "key-http",
        }), {"Content-Type": "application/json"})
        assert connection.getresponse().status == 200
        connection.request("POST", "/sessions/http/messages", json.dumps({"message": "hi"}),
                           {"Content-Type": "application/json"})
        assert connection.getresponse().status == 200
        connection.request("GET", "/sessions/http/messages")
        response = connection.getresponse()
        assert response.status == 200
        assert json.loads(response.read())["messages"] == [
            {"role": "user", "text": "hi"}, {"role": "assistant", "text": "welcome"}]
    finally:
        connection.close()
        server.shutdown()
        server.server_close()
        thread.join()


def test_a_recreated_session_does_not_inherit_the_deleted_ones_record(tmp_path, monkeypatch):
    """The harness file record outlives the Session; a reused id must not resurrect that conversation."""
    from pydantic_ai.messages import ModelRequest, ModelResponse, TextPart, UserPromptPart
    from anchor import pilot

    seen: list[list[str]] = []

    def answer(messages, info):
        seen.append([part.content for message in messages if isinstance(message, ModelRequest)
                     for part in message.parts if isinstance(part, UserPromptPart)])
        return ModelResponse(parts=[TextPart(content='好的')])

    monkeypatch.setattr(pilot, "_agent", lambda _: Agent(FunctionModel(answer), output_type=str))
    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")
    body, status = scheduler.create_session("reused")
    assert status == 201, body
    assert scheduler.pilot_message("reused", "被删除会话里的旧问题")[1] == 200
    scheduler.sessions.set_status("reused", "archived")
    scheduler.sessions.delete("reused")
    assert not (tmp_path / "sessions/reused").exists()
    assert (tmp_path / "state/pilot-steps").is_dir(), "the file record survives, by design"

    body, status = scheduler.create_session("reused")
    assert status == 201, body
    assert scheduler.pilot_message("reused", "新会话的问题")[1] == 200
    assert seen[-1] == ["新会话的问题"], seen


def test_an_unreadable_record_does_not_block_the_next_message(tmp_path, monkeypatch):
    """A damaged file record must not lock the conversation; the saved history is still there."""
    from pydantic_ai.messages import ModelResponse, TextPart
    from anchor import pilot

    def answer(messages, info):
        return ModelResponse(parts=[TextPart(content='好的')])

    monkeypatch.setattr(pilot, "_agent", lambda _: Agent(FunctionModel(answer), output_type=str))
    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")
    scheduler.create_session("broken")
    assert scheduler.pilot_message("broken", "第一句")[1] == 200
    record = sorted((tmp_path / "state/pilot-steps").iterdir())[-1]
    newest = max((record / "snapshots").glob("*.json"), key=lambda item: int(item.stem))
    newest.write_text("{not json", encoding="utf-8")
    scheduler.sessions.set_status("broken", "interrupted", reason="模拟中断")

    assert scheduler.pilot_message("broken", "第二句")[1] == 200
    assert scheduler.sessions.get("broken").status == "active"
    messages = __import__("json").loads(scheduler.pilot_messages("broken")[0])["messages"]
    assert [item["text"] for item in messages] == ["第一句", "好的", "第二句", "好的"]


def test_the_message_endpoint_refuses_while_a_decision_is_pending(tmp_path, monkeypatch):
    from pydantic_ai.messages import ModelResponse, TextPart
    from anchor import pilot

    def answer(messages, info):
        return ModelResponse(parts=[TextPart(content='不该走到这里')])

    monkeypatch.setattr(pilot, "_agent", lambda _: Agent(FunctionModel(answer), output_type=str))
    scheduler = Scheduler(tmp_path, tmp_path / "runtime.json")
    scheduler.create_session("gated")
    scheduler.sessions.set_pending("gated", [{"tool_call_id": "call-1", "key": "call-1",
                                              "action": "graph_delete", "target": "demo",
                                              "proposal": {"graph": "demo"}}])
    body, status = scheduler.pilot_message("gated", "顺便做点别的")
    assert status == 409 and "pending operation" in body, body
    assert scheduler.sessions.get("gated").status == "waiting_user"
