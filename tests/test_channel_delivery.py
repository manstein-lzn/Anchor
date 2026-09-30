"""Graph -> scoped channel tools / SSE -> gateway, without sending to real members."""
import asyncio
import base64
import io
import json
import threading
from concurrent.futures import ThreadPoolExecutor
from types import SimpleNamespace

import pytest
from PIL import Image
from pydantic_ai import BinaryContent
from pydantic_ai.messages import ModelResponse, ToolCallPart, ToolReturnPart, UserPromptPart
from pydantic_ai.models.function import DeltaToolCall, FunctionModel

from anchor.channel import EventLedger
from anchor.channel.control import ControlServer, request
from anchor.channel.tools import _visible_file, factory
from anchor.simple import run as runner
from plugins.wecom import ws_gateway
from test_channel_gateway import channel_server, _event, _complete, _use_model  # noqa: F401


def png():
    buf = io.BytesIO()
    Image.new("RGB", (12, 12), "red").save(buf, format="PNG")
    return buf.getvalue()


def test_control_auth_ack_and_no_ambiguous_replay(tmp_path, monkeypatch):
    monkeypatch.setenv("ANCHOR_WECOM_SEND_USERS", "alice")
    sent = []

    async def send(userid, body):
        sent.append((userid, body))
        if body["markdown"]["content"] == "uncertain":
            raise TimeoutError("ACK lost after sending")
        return {"errcode": 0}

    async def check():
        control = ControlServer(tmp_path / "c.sock", "local-test-token", EventLedger(tmp_path / "events.sqlite"), send)
        await control.start()
        payload = {"operation": "send", "request_id": "call-1", "userid": "alice", "content": "hello"}
        try:
            with pytest.raises(RuntimeError, match="unauthorized"):
                await asyncio.to_thread(request, control.path, "wrong", payload)
            with pytest.raises(RuntimeError, match="recipient"):
                await asyncio.to_thread(request, control.path, control.token, {**payload, "userid": "bob"})
            ack = await asyncio.to_thread(request, control.path, control.token, payload)
            assert ack["accepted"]
            assert (await asyncio.to_thread(request, control.path, control.token, payload))["duplicate"]
            with pytest.raises(RuntimeError, match="different content"):
                await asyncio.to_thread(request, control.path, control.token, {**payload, "content": "changed"})
            uncertain = {**payload, "request_id": "call-2", "content": "uncertain"}
            for _ in range(2):
                with pytest.raises(RuntimeError, match="not confirmed|unconfirmed"):
                    await asyncio.to_thread(request, control.path, control.token, uncertain)
            assert len(sent) == 2
        finally:
            await control.close()
        assert not control.path.exists()
    asyncio.run(check())


def test_graph_can_send_through_mounted_plugin_and_attach_actual_image(channel_server, monkeypatch):  # noqa: F811
    scheduler, call = channel_server
    sent = []
    scheduler.channel_supervisor = SimpleNamespace(send=lambda platform, payload: sent.append((platform, payload)) or {"accepted": True})
    data = png()

    def answer(messages, info):
        returns = [p for m in messages for p in m.parts if isinstance(p, ToolReturnPart)]
        names = {t.name for t in info.function_tools}
        assert {"wecom_send_message", "wecom_attach_image"} <= names
        if not any(p.tool_name == "bash" for p in returns):
            command = "python3 -c \"import base64;open('/workspace/result.png','wb').write(base64.b64decode('" + base64.b64encode(data).decode() + "'))\""
            return ModelResponse(parts=[ToolCallPart("bash", {"command": command})])
        if not any(p.tool_name == "wecom_send_message" for p in returns):
            return ModelResponse(parts=[ToolCallPart("wecom_send_message", {"userid": "bob", "content": "授权测试通知"}, tool_call_id="send-once")])
        if not any(p.tool_name == "wecom_attach_image" for p in returns):
            return ModelResponse(parts=[ToolCallPart("wecom_attach_image", {"path": "/workspace/result.png"})])
        return _complete("通知已提交，这是结果图片。")

    _use_model(monkeypatch, answer)
    status, response = call({"event": _event(text="给 bob 发通知，再回复图片")})
    assert status == 200, response
    assert len(sent) == 1 and sent[0][0] == "wecom"
    assert sent[0][1]["userid"] == "bob"
    assert base64.b64decode(response["msg_item"][0]["image"]["base64"]) == data
    assert response["text"].startswith("通知已提交")
    repeated = call({"event": _event(text="给 bob 发通知，再回复图片")})
    assert repeated[1] == response and len(sent) == 1


def test_channel_tools_are_not_attached_without_plugin_and_images_cannot_escape(tmp_path):
    scheduler = SimpleNamespace(channel_supervisor=None)
    workspace = tmp_path / "graph"
    directory = tmp_path / "node"
    directory.mkdir()
    outside = tmp_path / "other-user.png"
    outside.write_bytes(png())
    (directory / "link.png").symlink_to(outside)
    assert factory(scheduler, workspace, "run")("assistant", (), directory, ()) == ()
    for name in ("/etc/passwd", "../other-user.png", "/workspace/link.png", "/workspace/.git/config"):
        with pytest.raises(ValueError):
            _visible_file(name, directory, ())


def test_http_sse_delivers_summary_before_graph_completes_with_real_attachment(channel_server, monkeypatch, tmp_path):  # noqa: F811
    scheduler, _ = channel_server
    delivered = threading.Event()
    updates = []
    path = scheduler.root / "state/channels/wecom/events/example/photo.png"
    path.parent.mkdir(parents=True)
    path.write_bytes(png())
    text = path.parent / "notes.txt"
    text.write_text("附件里的待办代号：ALPHA-47", encoding="utf-8")

    async def stream(messages, info):
        parts = [p for m in messages for p in m.parts if isinstance(p, UserPromptPart)]
        assert any(isinstance(p.content, list) and any(isinstance(c, BinaryContent) and c.data == png()
                   for c in p.content) for p in parts)
        assert "ALPHA-47" in str(parts)
        yield {0: DeltaToolCall(name="final_result", json_args='{"summary":"第一段')}
        assert await asyncio.to_thread(delivered.wait, 5), "SSE must arrive while Graph is still running"
        yield {0: DeltaToolCall(json_args='，第二段。"}')}

    monkeypatch.setattr(runner, "model_for", lambda *a, **k: FunctionModel(stream_function=stream))
    raw = _event(text="处理图片和待办")
    raw.update(message_type="mixed", attachments=[
        {"kind": "image", "path": str(path), "name": "photo.png"},
        {"kind": "file", "path": str(text), "name": "notes.txt"}])
    from anchor.channel import ChannelEvent
    event = ChannelEvent(**raw)

    def progress(value):
        updates.append(value)
        delivered.set()

    reply = ws_gateway._post_stream(event, progress)
    assert updates and updates[0] == "第一段"
    assert reply["text"] == "第一段，第二段。"


def test_rich_reply_survives_gateway_retry_and_legacy_ledger_values(tmp_path):
    from anchor.channel.media import make_image_item
    reply = {"text": "结果图片", "msg_item": [make_image_item(png())]}
    delivered = []

    async def send(frame, **kwargs):
        delivered.append(kwargs)

    gateway = ws_gateway.WeComWebSocketGateway(None, state_path=tmp_path / "ledger.sqlite")
    gateway.client = SimpleNamespace(reply_stream=send)
    saved = json.dumps({"channel_reply": reply})
    asyncio.run(gateway._deliver({}, "stable-stream", ws_gateway._saved_reply(saved)))
    assert delivered[0]["finish"] and delivered[0]["msg_item"] == reply["msg_item"]
    assert ws_gateway._saved_reply("旧回复") == {"text": "旧回复"}
    assert ws_gateway._saved_reply('{"other":"text"}') == {"text": '{"other":"text"}'}


@pytest.mark.parametrize("delayed", ["download", "progress_ack"])
def test_gateway_orders_media_admission_and_suppresses_superseded_output(tmp_path, monkeypatch, delayed):
    import sys
    monkeypatch.setenv("WECOM_BOT_ID", "fake-bot")
    monkeypatch.setenv("WECOM_BOT_SECRET", "fake-secret")
    monkeypatch.delenv("ANCHOR_CHANNEL_CONTROL_SOCKET", raising=False)

    async def check():
        ready, release, blocked, newer_done = (asyncio.Event() for _ in range(4))
        admissions, deliveries = [], []

        class Client:
            def __init__(self, _):
                self.callbacks = {}

            def on(self, name, callback):
                self.callbacks[name] = callback

            async def connect(self):
                ready.set()

            def disconnect(self):
                pass

            async def download_file(self, *args):
                blocked.set()
                await release.wait()
                return png(), "picture.png"

            async def reply_stream(self, frame, **kwargs):
                if delayed == "progress_ack" and frame["body"]["msgid"] == "older" and kwargs["content"] == "正在处理…":
                    blocked.set()
                    await release.wait()
                deliveries.append((frame["body"]["msgid"], kwargs["content"]))

        client = Client(None)
        monkeypatch.setitem(sys.modules, "aibot", SimpleNamespace(WSClient=lambda _: client, WSClientOptions=lambda **k: k))

        async def handler(event, progress, admitted):
            admissions.append(event.event_id)
            admitted()
            if event.event_id == "older":
                await newer_done.wait()
                await progress("OLD BUSINESS OUTPUT")
                return {"text": "OLD FINAL BUSINESS OUTPUT"}
            newer_done.set()
            return {"text": "NEW ANSWER"}

        def frame(identifier):
            return {"cmd": "aibot_msg_callback", "headers": {"req_id": identifier},
                    "body": {"msgid": identifier, "msgtype": "text", "from": {"userid": "alice"}, "text": {"content": identifier}}}

        older = frame("older")
        if delayed == "download":
            older["body"].update(msgtype="image", image={"url": "https://example.test/picture", "aeskey": "fixture"})
        gateway = ws_gateway.WeComWebSocketGateway(None, state_path=tmp_path / "ledger.sqlite", stream_handler=handler)
        task = asyncio.create_task(gateway.run())
        try:
            await asyncio.wait_for(ready.wait(), 2)
            old = asyncio.create_task(client.callbacks["message"](older))
            await asyncio.wait_for(blocked.wait(), 2)
            new = asyncio.create_task(client.callbacks["message"](frame("newer")))
            if delayed == "download":
                await asyncio.sleep(.05)
                assert admissions == []
                release.set()
            else:
                await asyncio.wait_for(new, 2)
                release.set()
            await asyncio.wait_for(asyncio.gather(old, new), 3)
            assert admissions == ["older", "newer"]
            assert ("newer", "NEW ANSWER") in deliveries
            assert not any("OLD" in text for _, text in deliveries)
        finally:
            release.set()
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task
    asyncio.run(check())


def test_queued_send_rechecks_cancellation(tmp_path, monkeypatch):
    monkeypatch.setenv("ANCHOR_WECOM_SEND_USERS", "alice")
    stopped, release, entered = (threading.Event() for _ in range(3))
    sent = []
    scheduler = SimpleNamespace(channel_supervisor=SimpleNamespace(send=lambda *args: sent.append(args)))
    tools = factory(scheduler, tmp_path, "run", cancelled=stopped.is_set)("node", ("wecom",), tmp_path, ())
    send = tools[0].tools["wecom_send_message"].function

    def block():
        entered.set()
        release.wait(5)

    async def check():
        loop = asyncio.get_running_loop()
        loop.set_default_executor(ThreadPoolExecutor(max_workers=1))
        blocker = asyncio.create_task(asyncio.to_thread(block))
        while not entered.is_set():
            await asyncio.sleep(.01)
        task = asyncio.create_task(send(SimpleNamespace(tool_call_id="call"), "alice", "hello"))
        await asyncio.sleep(.05)
        stopped.set()
        release.set()
        with pytest.raises(ValueError, match="stopped"):
            await task
        await blocker
        assert not sent
    asyncio.run(check())


@pytest.mark.parametrize('old_status', ['ready', 'failed', 'processing'])
def test_gateway_does_not_replay_stale_business_after_restart(tmp_path, monkeypatch, old_status):
    import sys
    from anchor.channel import ChannelEvent
    monkeypatch.setenv('WECOM_BOT_ID', 'fixture')
    monkeypatch.setenv('WECOM_BOT_SECRET', 'fixture')
    monkeypatch.delenv('ANCHOR_CHANNEL_CONTROL_SOCKET', raising=False)
    path = tmp_path / 'retry.sqlite'
    ledger = EventLedger(path, lease_seconds=-1)
    older = ChannelEvent('wecom', 'older', 'alice', 'alice', text='old')
    newer = ChannelEvent('wecom', 'newer', 'alice', 'alice', text='new')
    assert ledger.claim(older)
    if old_status == 'ready':
        ledger.prepare_reply(older, 'OLD BUSINESS')
    elif old_status == 'failed':
        ledger.fail(older, 'temporary transport failure')
    assert ledger.claim(newer)
    assert ledger.is_latest(newer)
    deliveries, executed = [], []

    async def check():
        ready = asyncio.Event()
        class Client:
            def __init__(self, _):
                self.callbacks = {}
            def on(self, name, callback):
                self.callbacks[name] = callback
            async def connect(self):
                ready.set()
            def disconnect(self):
                pass
            async def reply_stream(self, frame, **kwargs):
                deliveries.append(kwargs)
        client = Client(None)
        monkeypatch.setitem(sys.modules, 'aibot', SimpleNamespace(WSClient=lambda _: client, WSClientOptions=lambda **k: k))
        async def handle(event):
            executed.append(event)
            return 'OLD BUSINESS'
        gateway = ws_gateway.WeComWebSocketGateway(handle, state_path=path)
        gateway.ledger.lease_seconds = -1
        task = asyncio.create_task(gateway.run())
        try:
            await asyncio.wait_for(ready.wait(), 2)
            await client.callbacks['message']({'cmd':'aibot_msg_callback','headers':{'req_id':'older'},
                'body':{'msgid':'older','from':{'userid':'alice'},'msgtype':'text','text':{'content':'old'}}})
            assert deliveries == [] and executed == []
            assert gateway.ledger.is_latest(newer)
        finally:
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task
    asyncio.run(check())
    # A replay must never rebind the saved reply to a different member.
    wrong = ChannelEvent('wecom', 'newer', 'bob', 'bob')
    ledger.prepare_reply(newer, 'private answer')
    assert not ledger.claim(wrong)
    assert ledger.pending_reply(wrong) is None
    assert not ledger.is_latest(wrong)


def test_installed_sdk_control_send_stream_and_rich_reply_on_wire(tmp_path, monkeypatch):
    import aibot
    import aibot.ws
    from websockets.asyncio.server import serve
    from anchor.channel.media import make_image_item
    monkeypatch.setenv('WECOM_BOT_ID', 'fixture-bot')
    monkeypatch.setenv('WECOM_BOT_SECRET', 'fixture-secret')
    monkeypatch.setenv('ANCHOR_WECOM_SEND_USERS', 'alice')
    socket_path = tmp_path / 'control.sock'
    monkeypatch.setenv('ANCHOR_CHANNEL_CONTROL_SOCKET', str(socket_path))
    monkeypatch.setenv('ANCHOR_CHANNEL_CONTROL_TOKEN', 'fixture-token')
    original_options = aibot.WSClientOptions
    original_connect = aibot.ws.websockets.connect
    def loopback(uri, **kwargs):
        assert uri.startswith('ws://127.0.0.1:')
        kwargs.pop('ssl', None)
        return original_connect(uri, **kwargs)
    monkeypatch.setattr(aibot.ws.websockets, 'connect', loopback)

    async def check():
        finished = asyncio.Event()
        sends, replies = [], []
        frame = {'cmd':'aibot_msg_callback','headers':{'req_id':'callback-rich'},
                 'body':{'msgid':'rich','from':{'userid':'alice'},'msgtype':'text','text':{'content':'fixture'}}}
        async def platform(ws):
            auth = json.loads(await ws.recv())
            assert auth['cmd'] == 'aibot_subscribe'
            await ws.send(json.dumps({'headers':auth['headers'],'errcode':0}))
            await ws.send(json.dumps(frame))
            async for raw in ws:
                message = json.loads(raw)
                await ws.send(json.dumps({'headers':message['headers'],'errcode':0}))
                if message['cmd'] == 'aibot_send_msg':
                    sends.append(message['body'])
                elif message['cmd'] == 'aibot_respond_msg':
                    replies.append(message['body']['stream'])
                    if replies[-1]['finish']:
                        finished.set()
        async def handle(event, progress, admitted):
            admitted()
            ack = await asyncio.to_thread(request, socket_path, 'fixture-token', {
                'operation':'send','request_id':'one-send','userid':'alice','content':'authorized fixture'})
            assert ack['accepted']
            await progress('正文正在生成')
            return {'text':'完整回复','msg_item':[make_image_item(png())]}
        async with serve(platform, '127.0.0.1', 0) as server:
            port = server.sockets[0].getsockname()[1]
            monkeypatch.setattr(aibot, 'WSClientOptions', lambda **kw: original_options(**kw, ws_url=f'ws://127.0.0.1:{port}'))
            gateway = ws_gateway.WeComWebSocketGateway(None, state_path=tmp_path/'sdk.sqlite', stream_handler=handle)
            task = asyncio.create_task(gateway.run())
            try:
                await asyncio.wait_for(finished.wait(), 10)
                assert len(sends) == 1
                assert sends[0]['chatid'] == 'alice'
                assert sends[0]['markdown']['content'] == 'authorized fixture'
                assert any(r['content'] == '正文正在生成' and not r['finish'] for r in replies)
                final = replies[-1]
                assert final['finish'] and final['content'] == '完整回复'
                assert base64.b64decode(final['msg_item'][0]['image']['base64']) == png()
                assert len({r['id'] for r in replies}) == 1
            finally:
                task.cancel()
                with pytest.raises(asyncio.CancelledError):
                    await task
    asyncio.run(check())
