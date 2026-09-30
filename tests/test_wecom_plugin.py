import base64
import hashlib
import json
import struct
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from anchor.library import Library


def _load(name, path):
    import importlib.util
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_wecom_plugin_manifest_and_mcp_schema(monkeypatch):
    root = Path(__file__).parents[1]
    monkeypatch.setenv("WECOM_CORP_ID", "corp")
    monkeypatch.setenv("WECOM_AGENT_ID", "7")
    monkeypatch.setenv("WECOM_SECRET", "secret")
    library = Library(root)
    record = library.plugin("wecom")[0]
    assert record["skills"] == ["skills/wecom/SKILL.md"]
    assert record["mcpServers"] == {"wecom": {"transport": "stdio"}}
    server = dict(library.mcp_servers("wecom"))["wecom"]
    assert server["env"] == {"WECOM_CORP_ID": "corp", "WECOM_AGENT_ID": "7", "WECOM_SECRET": "secret",
                              "WECOM_API_BASE_URL": "https://qyapi.weixin.qq.com"}


def test_wecom_mcp_sends_text_and_reuses_token(monkeypatch):
    module = _load("wecom_server", Path(__file__).parents[1] / "plugins/wecom/server.py")
    calls = []

    class API(BaseHTTPRequestHandler):
        def do_GET(self):
            calls.append((self.path, None))
            data = json.dumps({"errcode": 0, "access_token": "token", "expires_in": 7200}).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_POST(self):
            calls.append((self.path, json.loads(self.rfile.read(int(self.headers["Content-Length"])))) )
            data = b'{"errcode":0,"errmsg":"ok","msgid":"m1"}'
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), API)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    module.API_BASE = f"http://127.0.0.1:{server.server_port}"
    module._token = None
    monkeypatch.setenv("WECOM_CORP_ID", "corp")
    monkeypatch.setenv("WECOM_AGENT_ID", "7")
    monkeypatch.setenv("WECOM_SECRET", "secret")
    try:
        first = module._handle({"id": 1, "method": "tools/call", "params": {
            "name": "wecom_send_text", "arguments": {"content": "hello", "touser": "alice"}}})
        second = module._handle({"id": 2, "method": "tools/call", "params": {
            "name": "wecom_send_markdown", "arguments": {"content": "**hi**", "touser": "alice"}}})
        assert first["result"]["isError"] is False and second["result"]["isError"] is False
        assert len([item for item in calls if item[0].startswith("/cgi-bin/gettoken")]) == 1
        sent = [item[1] for item in calls if item[0].startswith("/cgi-bin/message/send")]
        assert sent[0]["text"] == {"content": "hello"}
        assert sent[1]["markdown"] == {"content": "**hi**"}
        assert sent[0]["agentid"] == 7 and sent[0]["touser"] == "alice"
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def test_wecom_bridge_decrypts_and_triggers_anchor(monkeypatch):
    module = _load("wecom_bridge", Path(__file__).parents[1] / "plugins/wecom/bridge.py")
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

    key = bytes(range(32))
    key_text = base64.b64encode(key).decode().rstrip("=")
    token = "bridge-token"
    corp = "corp"
    message = "<xml><ToUserName>app</ToUserName><FromUserName>alice</FromUserName><CreateTime>1</CreateTime><MsgType>text</MsgType><Content>hello</Content><MsgId>9</MsgId></xml>"
    plain = b"r" * 16 + struct.pack("!I", len(message.encode())) + message.encode() + corp.encode()
    padding = 32 - len(plain) % 32
    encryptor = Cipher(algorithms.AES(key), modes.CBC(key[:16])).encryptor()
    encrypted = encryptor.update(plain + bytes([padding]) * padding) + encryptor.finalize()
    ciphertext = base64.b64encode(encrypted).decode()
    monkeypatch.setenv("WECOM_ENCODING_AES_KEY", key_text)
    monkeypatch.setenv("WECOM_CORP_ID", corp)
    monkeypatch.setenv("WECOM_TOKEN", token)
    timestamp, nonce = "100", "nonce"
    signature = hashlib.sha1("".join(sorted((token, timestamp, nonce, ciphertext))).encode()).hexdigest()
    assert module._decrypt(ciphertext) == message
    assert module._decode_callback({"timestamp": [timestamp], "nonce": [nonce],
                                    "msg_signature": [signature], "echostr": [ciphertext]}) == message
    assert module._xml_message(message)["text"] == "hello"

    received = []

    class Anchor(BaseHTTPRequestHandler):
        def do_POST(self):
            received.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
            self.send_response(202)
            self.end_headers()

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Anchor)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    monkeypatch.setenv("ANCHOR_WEBHOOK_URL", f"http://127.0.0.1:{server.server_port}/hook")
    monkeypatch.setenv("ANCHOR_API_KEY", "a" * 40)
    module._trigger(module._xml_message(message))
    assert received[0]["input"]["user_id"] == "alice"
    assert received[0]["input"]["text"] == "hello"
    server.shutdown()
    server.server_close()
    thread.join()


def test_wecom_websocket_normalizes_text_message():
    module = _load("wecom_ws_gateway", Path(__file__).parents[1] / "plugins/wecom/ws_gateway.py")
    frame = {
        "cmd": "aibot_msg_callback",
        "headers": {"req_id": "request-1"},
        "body": {"msgid": "message-1", "msgtype": "text", "from": {"userid": "alice"},
                 "text": {"content": "hello"}},
    }
    event = module.normalize_message(frame)
    assert event is not None
    assert event.event_id == "message-1"
    assert event.sender_id == "alice"
    assert event.conversation_id == "alice"
    assert event.text == "hello"
