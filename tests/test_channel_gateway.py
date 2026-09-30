import asyncio
import json
import sys
import threading
from concurrent.futures import ThreadPoolExecutor
from http.client import HTTPConnection
from http.server import ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace

import pytest
from pydantic_ai.messages import ModelResponse, ToolCallPart, ToolReturnPart, UserPromptPart
from pydantic_ai.models.function import FunctionModel

from anchor.channel import ChannelEvent, EventLedger
from anchor.serve import Handler, Scheduler
from anchor.simple import run as runner
from plugins.wecom import ws_gateway
from plugins.wecom.setup import install


@pytest.fixture
def channel_server(tmp_path, monkeypatch):
    key = "c" * 40
    monkeypatch.setenv("ANCHOR_API_KEY", key)
    monkeypatch.setenv("ANCHOR_API_KEYS", json.dumps([key]))
    monkeypatch.setenv("ANCHOR_WECOM_GRAPH", "wecom-assistant")
    monkeypatch.setenv("ANCHOR_WECOM_REPLY_NODE", "assistant")
    monkeypatch.setenv("ANCHOR_WECOM_USERS", "alice,bob")
    install(tmp_path)
    config = tmp_path / "runtime.json"
    config.write_text("{}")
    monkeypatch.setattr(runner, "_config", lambda _: ({"models.default": {}}, None))
    monkeypatch.setattr(runner, "_secret", lambda *_: "unused")
    pilot_calls = []

    def wrong_executor(self, *args, **kwargs):
        pilot_calls.append(args)
        return json.dumps({"error": "must not call Pilot"}), 502

    monkeypatch.setattr(Scheduler, "pilot_message", wrong_executor)
    scheduler = Scheduler(tmp_path, config)
    handler = type("TestHandler", (Handler,), {"scheduler": scheduler})
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    monkeypatch.setenv("ANCHOR_CHANNEL_WEBHOOK_URL",
                       f"http://127.0.0.1:{server.server_port}/v1/channels/wecom/events")

    def call(body, auth=f"Bearer {key}", path="/v1/channels/wecom/events"):
        client = HTTPConnection(*server.server_address, timeout=30)
        try:
            headers = {"Content-Type": "application/json"}
            if auth is not None:
                headers["Authorization"] = auth
            payload = body if isinstance(body, bytes) else json.dumps(body, ensure_ascii=False).encode()
            client.request("POST", path, payload, headers)
            response = client.getresponse()
            return response.status, json.loads(response.read())
        finally:
            client.close()

    try:
        yield scheduler, call
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
        assert not pilot_calls, "a channel message must execute a Graph without calling Pilot"


def _event(event_id="msg-1", sender="alice", text="你好"):
    return {"source": "wecom", "event_id": event_id, "sender_id": sender,
            "conversation_id": sender, "text": text}


def _complete(text):
    return ModelResponse(parts=[ToolCallPart("final_result", {"summary": text})])


def _use_model(monkeypatch, function):
    monkeypatch.setattr(runner, "model_for", lambda *a, **kw: FunctionModel(function))


def _prompts(messages):
    return [part.content for message in messages for part in message.parts if isinstance(part, UserPromptPart)]


def test_event_ledger_deduplicates_completed_events_and_retries_failures(tmp_path: Path):
    ledger = EventLedger(tmp_path / "events.sqlite")
    event = ChannelEvent("wecom", "m1", "alice", "alice", text="hello")
    assert ledger.claim(event)
    assert not ledger.claim(event)
    ledger.fail(event, "temporary")
    assert ledger.claim(event)
    ledger.prepare_reply(event, "answer")
    assert not ledger.claim(event)
    assert ledger.pending_reply(event) == "answer"
    ledger.fail_delivery(event, "timeout")
    assert not ledger.claim(event)
    assert ledger.pending_reply(event) == "answer"
    ledger.complete(event)
    assert ledger.pending_reply(event) is None
    assert not ledger.claim(event)


def test_websocket_entrypoint_loads_dotenv_without_overriding_shell(tmp_path, monkeypatch):
    (tmp_path / ".env").write_text("WECOM_BOT_ID=from-file\nWECOM_BOT_SECRET=secret-from-file\n", encoding="utf-8")
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("WECOM_BOT_ID", "from-shell")
    monkeypatch.delenv("WECOM_BOT_SECRET", raising=False)
    loaded = {}

    async def capture_run(self):
        loaded["bot_id"] = ws_gateway._required("WECOM_BOT_ID")
        loaded["secret"] = ws_gateway._required("WECOM_BOT_SECRET")

    monkeypatch.setattr(ws_gateway.WeComWebSocketGateway, "run", capture_run)
    asyncio.run(ws_gateway.main())
    assert loaded == {"bot_id": "from-shell", "secret": "secret-from-file"}


def test_graph_channel_deduplicates_and_continues_native_history_after_reopen(channel_server, tmp_path, monkeypatch):
    calls = []

    def answer(messages, info):
        calls.append(messages)
        assert {tool.name for tool in info.function_tools} == {"bash"}
        return _complete("已收到你的消息")

    _use_model(monkeypatch, answer)
    scheduler, call = channel_server
    event = _event()
    with ThreadPoolExecutor(max_workers=2) as pool:
        replies = list(pool.map(lambda _: call({"event": event}), range(2)))
    status, body = replies[0]
    assert status == 200 and body["text"] == "已收到你的消息", body
    assert replies[1] == replies[0]
    assert call({"event": event}) == replies[0]
    assert len(calls) == 1
    assert call({"event": {**event, "text": "different"}})[0] == 409
    session, = scheduler.sessions.list()
    assert session.graph == "wecom-assistant" and session.channel["sender_id"] == "alice"
    pilot_session = scheduler.sessions.create()
    visible = json.loads(scheduler.sessions_list()[0])["sessions"]
    assert [item["id"] for item in visible] == [pilot_session.id]
    run_dir = tmp_path / "workspaces" / session.graph / "runs" / body["run"]
    state = runner.RunState.load(run_dir)
    assert state.status == "finished" and state.executed == ["assistant"]
    assert list((run_dir / "control").rglob("events.jsonl"))
    reopened = Scheduler(tmp_path, tmp_path / "runtime.json")
    assert reopened.channel_message(event) == (json.dumps(body, ensure_ascii=False), 200)
    response, status = reopened.channel_message(_event("msg-2", text="继续"))
    assert status == 200, response
    assert json.loads(response)["session"] == body["session"]
    assert json.loads(response)["run"] != body["run"]
    assert len(calls) == 2
    assert len(_prompts(calls[-1])) == 2
    assert "你好" in _prompts(calls[-1])[0] and "继续" in _prompts(calls[-1])[1]
    assert json.loads(reopened.pilot_messages(session.id)[0])["messages"] == [
        {"role": "user", "text": "你好"}, {"role": "assistant", "text": "已收到你的消息"},
        {"role": "user", "text": "继续"}, {"role": "assistant", "text": "已收到你的消息"}]


def test_channel_rejects_untrusted_users_groups_and_overrides_before_execution(channel_server, monkeypatch):
    scheduler, call = channel_server
    assert call({"event": _event()}, auth=None)[0] == 401
    assert call({"event": _event()}, auth="Bearer wrong")[0] == 401
    monkeypatch.setenv("ANCHOR_API_KEY", "different-client-key")
    assert call({"event": _event()}, auth="Bearer different-client-key")[0] == 401
    assert call(b"{")[0] == 400
    assert call({"event": []})[0] == 400
    assert call({"event": _event(), "graph": "other"})[0] == 400
    assert call({"event": {**_event(), "source": "other"}})[0] == 400
    assert call({"event": {**_event(), "sender_id": ""}})[0] == 400
    assert call({"event": _event(sender="mallory")})[0] == 403
    assert call({"event": {**_event(), "metadata": {"chat_type": "group"}}})[0] == 400
    assert call({"event": {**_event(), "message_type": "image"}})[0] == 400
    scheduler.wecom_graph = "missing"
    assert call({"event": _event()})[0] == 503
    assert scheduler.sessions.list() == []


def test_same_graph_runs_for_two_users_concurrently_and_admin_cannot_mutate_it(channel_server, monkeypatch):
    calls = []
    entered = threading.Barrier(3)
    release = threading.Event()

    def answer(messages, info):
        prompts = _prompts(messages)
        calls.append(prompts)
        if len(prompts) == 1:
            entered.wait(timeout=10)
            assert release.wait(timeout=10)
        return _complete("alice" if "alice-private" in prompts[0] else "bob")

    _use_model(monkeypatch, answer)
    scheduler, call = channel_server
    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(call, {"event": _event(name + "-1", name, name + "-private")})
                   for name in ("alice", "bob")]
        try:
            entered.wait(timeout=10)
            assert len(scheduler.channel_runs) == 2
            assert scheduler.delete_graph("wecom-assistant")[1] == 409
            assert scheduler.save("wecom-assistant", {})[1] == 409
            assert scheduler.trigger("wecom-assistant", None)[1] == 409
            assert all(scheduler.delete_run(run)[1] == 409 for run in scheduler.channel_runs)
        finally:
            release.set()
        replies = [future.result() for future in futures]
    assert all(status == 200 for status, _ in replies), replies
    assert replies[0][1]["session"] != replies[1][1]["session"]
    status, continued = call({"event": _event("alice-2", text="alice-next")})
    assert status == 200 and continued["text"] == "alice", continued
    assert "alice-private" in calls[-1][0] and "bob-private" not in str(calls[-1])
    assert len(scheduler.sessions.list()) == 2


def test_next_graph_turn_reads_previous_artifact_read_only_and_replays_original_reply(channel_server, monkeypatch):
    observed = []

    def answer(messages, info):
        prompts = _prompts(messages)
        returns = [p for m in messages for p in m.parts if isinstance(p, ToolReturnPart) and p.tool_name == "bash"]
        if len(prompts) == 1:
            if not returns:
                return ModelResponse(parts=[ToolCallPart("bash", {"command": "printf 'PRIVATE_NOTE' > note.txt"})])
            return _complete("研究范围？")
        if len(returns) == 1:
            return ModelResponse(parts=[ToolCallPart("bash", {"command":
                "cat /previous/note.txt; if touch /previous/forbidden 2>/dev/null; then echo BAD; else echo READ_ONLY; fi"})])
        observed.append(returns[-1].content)
        return _complete("已根据上一轮记录继续")

    _use_model(monkeypatch, answer)
    scheduler, call = channel_server
    first = call({"event": _event()})
    assert first[0] == 200 and first[1]["text"] == "研究范围？", first
    second = call({"event": _event("msg-2", text="只看 2020 年之后")})
    assert second[0] == 200 and second[1]["text"] == "已根据上一轮记录继续", second
    assert "PRIVATE_NOTE" in str(observed) and "READ_ONLY" in str(observed) and "BAD" not in str(observed)
    assert call({"event": _event()}) == first
    assert len(scheduler.runs()) == 2


def test_completed_graph_result_survives_lost_turn_settlement(channel_server, monkeypatch):
    _use_model(monkeypatch, lambda *_: _complete("结果已经持久化"))
    scheduler, call = channel_server
    first = call({"event": _event()})
    assert first[0] == 200
    session = first[1]["session"]
    turn, = scheduler.turns.list(session)
    with scheduler.turns.connect() as db:
        db.execute("UPDATE turns SET status='interrupted' WHERE id=?", (turn["id"],))
    assert call({"event": _event()}) == first
    assert len(scheduler.runs()) == 1


def test_setup_preserves_operator_graph(tmp_path):
    target = install(tmp_path)
    target.write_text('{"operator": true}')
    assert install(tmp_path).read_text() == '{"operator": true}'


def test_websocket_delivery_retries_saved_graph_reply(channel_server, tmp_path, monkeypatch):
    calls = []

    def answer(messages, info):
        calls.append(messages)
        return _complete("已收到你的消息")

    _use_model(monkeypatch, answer)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("WECOM_BOT_ID", "test-bot")
    monkeypatch.setenv("WECOM_BOT_SECRET", "test-secret")
    monkeypatch.setenv("WECOM_CHANNEL_STATE", str(tmp_path / "channel"))
    frame = {"cmd": "aibot_msg_callback", "headers": {"req_id": "req-1"},
             "body": {"msgid": "msg-1", "msgtype": "text", "from": {"userid": "alice"},
                      "text": {"content": "你好"}}}

    class Client:
        disconnected = False

        def __init__(self, options):
            self.callbacks = {}
            self.replies = []
            self.ready = asyncio.Event()

        def on(self, event, callback):
            self.callbacks[event] = callback

        async def connect(self):
            for _ in range(3):
                await self.callbacks["message"](frame)
            self.ready.set()

        async def reply_stream(self, frame, **reply):
            self.replies.append(reply)
            if reply["finish"] and len([r for r in self.replies if r["finish"]]) == 1:
                raise TimeoutError("temporary delivery error")

        def disconnect(self):
            self.disconnected = True

    async def check():
        client = Client(None)
        monkeypatch.setitem(sys.modules, "aibot", SimpleNamespace(
            WSClient=lambda _: client, WSClientOptions=lambda **options: options))
        task = asyncio.create_task(ws_gateway.main())
        try:
            await asyncio.wait_for(client.ready.wait(), timeout=20)
            assert len(client.replies) == 3
            assert not client.replies[0]["finish"]
            assert all(reply["content"] == "已收到你的消息" and reply["finish"] for reply in client.replies[1:])
            assert len({r["stream_id"] for r in client.replies}) == 1
            assert len(calls) == 1
        finally:
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task
        assert client.disconnected

    asyncio.run(check())


def test_new_message_cancels_work_and_keeps_native_history_and_partial_files(channel_server, monkeypatch):
    entered = threading.Event()
    seen = []

    async def answer(messages, info):
        prompts = _prompts(messages)
        returns = [p for m in messages for p in m.parts if isinstance(p, ToolReturnPart) and p.tool_name == "bash"]
        if "NEW_INPUT" not in prompts[-1]:
            if not returns:
                return ModelResponse(parts=[ToolCallPart("bash", {"command": "echo PARTIAL_WORK > note.txt"})])
            entered.set()
            await asyncio.Event().wait()
        seen.append(messages)
        if len(returns) == 1:
            return ModelResponse(parts=[ToolCallPart("bash", {"command": "cat /previous/note.txt"})])
        assert "PARTIAL_WORK" in str(returns[-1].content)
        return _complete("按补充要求完成")

    _use_model(monkeypatch, answer)
    scheduler, call = channel_server
    with ThreadPoolExecutor(max_workers=2) as pool:
        first = pool.submit(call, {"event": _event(text="ORIGINAL_INPUT")})
        assert entered.wait(10)
        second = call({"event": _event("second", text="NEW_INPUT")})
        old = first.result()
    assert old[0] == 200 and old[1]["superseded"] and old[1]["text"] == ""
    assert second[0] == 200 and second[1]["text"] == "按补充要求完成", second
    assert "ORIGINAL_INPUT" in _prompts(seen[-1])[0]
    assert call({"event": _event(text="ORIGINAL_INPUT")}) == old
    assert len(scheduler.runs()) == 2
    turns = scheduler.turns.list(second[1]["session"])
    assert [t["status"] for t in turns] == ["completed", "stopped"]
    assert not scheduler.channel_runs and not scheduler.channel_tail


def test_three_message_burst_preserves_all_input_and_only_latest_replies(channel_server, monkeypatch):
    entered = threading.Event()
    calls = []

    async def answer(messages, info):
        calls.append(messages)
        if "THIRD" not in _prompts(messages)[-1]:
            entered.set()
            try:
                await asyncio.Event().wait()
            finally:
                await asyncio.sleep(0.3)
        assert "FIRST" in str(_prompts(messages)) and "SECOND" in _prompts(messages)[-1]
        return _complete("latest")

    _use_model(monkeypatch, answer)
    scheduler, call = channel_server
    with ThreadPoolExecutor(max_workers=2) as pool:
        first = pool.submit(call, {"event": _event(text="FIRST")})
        assert entered.wait(10)
        session, = scheduler.sessions.list()
        body, status = scheduler.create_turn(session.id, "burst-second", "SECOND")
        assert status == 202
        middle = json.loads(body)["turn"]
        last = call({"event": _event("third", text="THIRD")})
        assert first.result()[1]["superseded"]
    assert last[0] == 200 and last[1]["text"] == "latest", last
    assert scheduler.turns.get(session.id, middle["id"])["status"] == "stopped"
    assert len(calls) == 2 and len(scheduler.runs()) == 3


def test_channel_graph_calls_real_stdio_plugin_in_sandbox(channel_server, tmp_path, monkeypatch):
    plugin = tmp_path / "library/plugins/probe"
    plugin.mkdir(parents=True)
    (plugin / "plugin.json").write_text(json.dumps({
        "name": "Probe", "description": "Local business query", "mcpServers": {
            "probe": {"command": "python3", "args": ["server.py"]}}}))
    (plugin / "server.py").write_text('import json, sys\nfor line in sys.stdin:\n    q=json.loads(line)\n    if "id" not in q: continue\n    method=q.get("method")\n    if method=="initialize":\n        result={"protocolVersion":q["params"]["protocolVersion"], "capabilities":{"tools":{}}, "serverInfo":{"name":"probe","version":"1"}}\n    elif method=="tools/list":\n        result={"tools":[{"name":"lookup","description":"Read the business reference", "inputSchema":{"type":"object","properties":{}}}]}\n    elif method=="tools/call":\n        result={"content":[{"type":"text","text":"BUSINESS_RESULT_742"}],"isError":False}\n    else: result={}\n    print(json.dumps({"jsonrpc":"2.0","id":q["id"],"result":result}),flush=True)\n')
    graph = tmp_path / "workspaces/wecom-assistant/graph.json"
    raw = json.loads(graph.read_text())
    raw["nodes"][0]["plugins"] = ["probe"]
    graph.write_text(json.dumps(raw))
    used = []

    def answer(messages, info):
        name = next(t.name for t in info.function_tools if t.name.endswith("lookup"))
        returns = [p for m in messages for p in m.parts if isinstance(p, ToolReturnPart) and p.tool_name == name]
        if not returns:
            return ModelResponse(parts=[ToolCallPart(name, {})])
        used.append(returns[-1].content)
        return _complete(str(returns[-1].content))

    _use_model(monkeypatch, answer)
    scheduler, call = channel_server
    response = call({"event": _event(text="查询业务数据")})
    assert response[0] == 200 and "BUSINESS_RESULT_742" in response[1]["text"], response
    assert len(used) == 1
    run_dir = scheduler.run_dir(response[1]["run"])
    assert "probe" in (run_dir / "plugins.json").read_text()


def test_channel_graph_reads_downloaded_attachment_read_only(channel_server, tmp_path, monkeypatch):
    attachment_dir = tmp_path / "state/channels/wecom/events/media-1"
    attachment_dir.mkdir(parents=True)
    (attachment_dir / "01-note.txt").write_text("ATTACHMENT_FACT", encoding="utf-8")
    seen = []

    def answer(messages, info):
        bash = next(tool.name for tool in info.function_tools if tool.name == "bash")
        returns = [part for message in messages for part in message.parts
                   if isinstance(part, ToolReturnPart) and part.tool_name == bash]
        if not returns:
            return ModelResponse(parts=[ToolCallPart(bash, {"command":
                "cat /in/channel/01-note.txt; if touch /in/channel/should-not-write 2>/dev/null; "
                "then echo BAD; else echo READ_ONLY; fi"})])
        seen.append(returns[-1].content)
        return _complete(str(returns[-1].content))

    _use_model(monkeypatch, answer)
    scheduler, call = channel_server
    response = call({"event": {**_event(text=""), "message_type": "image",
                               "attachments": [{"kind": "image", "path": str(attachment_dir / "01-note.txt"),
                                                 "name": "01-note.txt", "mime_type": "text/plain", "size": 15}]}})
    assert response[0] == 200 and "ATTACHMENT_FACT" in response[1]["text"]
    assert "READ_ONLY" in str(seen) and "BAD" not in str(seen)
    assert not (attachment_dir / "should-not-write").exists()


def test_history_survives_skipped_node_and_failure_before_snapshot(channel_server, tmp_path, monkeypatch):
    scheduler, call = channel_server
    graph_path = tmp_path / "workspaces/wecom-assistant/graph.json"
    raw = json.loads(graph_path.read_text())
    raw["agents"]["branch"] = {"model": "branch", "instructions": "Branch worker"}
    raw["nodes"].append({"id": "branch", "agent": "branch"})
    raw["edges"] = [{"from": "branch", "to": "assistant"}]
    raw["entry"] = "branch"
    graph_path.write_text(json.dumps(raw))
    seen = []
    monkeypatch.setattr(runner, "_config", lambda _: ({"models.default": {"label": "reply"},
                                                      "branch": {"label": "branch"}}, None))

    def branch(messages, info):
        seen.append(_prompts(messages))
        return _complete("branch finished")

    monkeypatch.setattr(runner, "model_for", lambda profile, **_: FunctionModel(
        branch if profile["label"] == "branch" else lambda *_: _complete("ok")))
    assert call({"event": _event(text="FIRST_BRANCH")})[0] == 200
    raw["entry"] = "assistant"
    raw["nodes"] = [node for node in raw["nodes"] if node["id"] != "branch"]
    raw["edges"] = []
    assert scheduler.save("wecom-assistant", raw)[1] == 200
    assert call({"event": _event("skip", text="SKIPPED_BRANCH")})[0] == 200
    raw["entry"] = "branch"
    raw["nodes"].append({"id": "branch", "agent": "branch"})
    raw["edges"] = [{"from": "branch", "to": "assistant"}]
    assert scheduler.save("wecom-assistant", raw)[1] == 200
    assert call({"event": _event("return", text="RETURN_BRANCH")})[0] == 200
    assert len(seen) == 2 and "FIRST_BRANCH" in seen[-1][0] and "RETURN_BRANCH" in seen[-1][-1]
    # Fail model construction before a native step snapshot exists, then continue again.
    factory = runner.model_for
    monkeypatch.setattr(runner, "model_for", lambda *_a, **_kw: (_ for _ in ()).throw(ValueError("setup failed")))
    assert call({"event": _event("failed", text="FAILED_SETUP_INPUT")})[0] == 502
    monkeypatch.setattr(runner, "model_for", factory)
    assert call({"event": _event("recover", text="RECOVER_INPUT")})[0] == 200
    assert "FIRST_BRANCH" in seen[-1][0] and "RETURN_BRANCH" in str(seen[-1])
    assert "FAILED_SETUP_INPUT" in seen[-1][-1]


def test_installed_websocket_sdk_authenticates_and_reconnects_without_reexecuting(channel_server, tmp_path, monkeypatch):
    import aibot
    from websockets.asyncio.server import serve

    _use_model(monkeypatch, lambda *_: _complete("SDK_GRAPH_REPLY"))
    monkeypatch.setenv("WECOM_BOT_ID", "local-bot")
    monkeypatch.setenv("WECOM_BOT_SECRET", "local-secret")
    original_options = aibot.WSClientOptions
    import aibot.ws
    original_connect = aibot.ws.websockets.connect

    def loopback_connect(uri, **kwargs):
        # SDK always supplies TLS options; this fixture alone uses unencrypted loopback.
        assert uri.startswith("ws://127.0.0.1:")
        kwargs.pop("ssl", None)
        return original_connect(uri, **kwargs)

    monkeypatch.setattr(aibot.ws.websockets, "connect", loopback_connect)

    async def check():
        finished = asyncio.Event()
        auth_frames, replies = [], []
        frame = {"cmd": "aibot_msg_callback", "headers": {"req_id": "callback-1"},
                 "body": {"msgid": "sdk-msg", "msgtype": "text", "chattype": "single",
                          "from": {"userid": "alice"}, "text": {"content": "你好"}}}

        async def platform(ws):
            auth = json.loads(await ws.recv())
            assert auth["cmd"] == "aibot_subscribe"
            assert auth["body"] == {"bot_id": "local-bot", "secret": "local-secret"}
            auth_frames.append(auth)
            await ws.send(json.dumps({"headers": auth["headers"], "errcode": 0}))
            await ws.send(json.dumps(frame))
            if len(auth_frames) > 1:
                await asyncio.sleep(0.2)
                finished.set()
                await ws.wait_closed()
                return
            async for wire in ws:
                message = json.loads(wire)
                await ws.send(json.dumps({"headers": message["headers"], "errcode": 0}))
                if message.get("cmd") == "aibot_respond_msg":
                    replies.append(message["body"]["stream"])
                    if replies[-1]["finish"]:
                        await asyncio.sleep(0.1)
                        await ws.close()

        async with serve(platform, "127.0.0.1", 0) as server:
            port = server.sockets[0].getsockname()[1]
            monkeypatch.setattr(aibot, "WSClientOptions", lambda **kw: original_options(
                **kw, ws_url=f"ws://127.0.0.1:{port}", reconnect_interval=50))
            gateway = ws_gateway.WeComWebSocketGateway(ws_gateway._handle_event, state_path=tmp_path / "sdk.sqlite")
            task = asyncio.create_task(gateway.run())
            try:
                await asyncio.wait_for(finished.wait(), 20)
                assert len(auth_frames) == 2
                assert [r["finish"] for r in replies] == [False, True]
                assert replies[-1]["content"] == "SDK_GRAPH_REPLY"
                assert len(channel_server[0].runs()) == 1
            finally:
                task.cancel()
                with pytest.raises(asyncio.CancelledError):
                    await task

    asyncio.run(check())


def test_delayed_progress_ack_does_not_reverse_message_admission(tmp_path, monkeypatch):
    monkeypatch.setenv("WECOM_BOT_ID", "bot")
    monkeypatch.setenv("WECOM_BOT_SECRET", "secret")

    async def check():
        admitted = []
        first_admitted = asyncio.Event()
        release_ack = asyncio.Event()
        ready = asyncio.Event()

        async def handler(event):
            admitted.append(event.event_id)
            if event.event_id == "first":
                first_admitted.set()
            return "answer"

        class Client:
            def __init__(self, options):
                self.callbacks = {}

            def on(self, event, callback):
                self.callbacks[event] = callback

            async def connect(self):
                def frame(name):
                    return {"cmd": "aibot_msg_callback", "headers": {"req_id": name},
                            "body": {"msgid": name, "msgtype": "text", "from": {"userid": "alice"},
                                     "text": {"content": name}}}
                first = asyncio.create_task(self.callbacks["message"](frame("first")))
                await asyncio.wait_for(first_admitted.wait(), 2)
                await self.callbacks["message"](frame("second"))
                release_ack.set()
                await first
                ready.set()

            async def reply_stream(self, frame, **reply):
                if frame["body"]["msgid"] == "first" and not reply["finish"]:
                    await release_ack.wait()

            def disconnect(self):
                pass

        monkeypatch.setitem(sys.modules, "aibot", SimpleNamespace(
            WSClient=Client, WSClientOptions=lambda **options: options))
        gateway = ws_gateway.WeComWebSocketGateway(handler, state_path=tmp_path / "ack.sqlite")
        task = asyncio.create_task(gateway.run())
        try:
            await asyncio.wait_for(ready.wait(), 5)
            assert admitted == ["first", "second"]
        finally:
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task

    asyncio.run(check())
