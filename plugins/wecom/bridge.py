"""WeCom callback bridge: verify/decrypt callbacks and trigger an Anchor Graph Webhook."""
from __future__ import annotations

import base64
import hashlib
import json
import os
import struct
import threading
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse
from xml.etree import ElementTree

from anchor.runtime.secrets import load_dotenv


def _env(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise ValueError(f"{name} is not configured")
    return value


def _signature(token: str, timestamp: str, nonce: str, ciphertext: str) -> str:
    return hashlib.sha1("".join(sorted((token, timestamp, nonce, ciphertext))).encode()).hexdigest()


def _decrypt(ciphertext: str) -> str:
    try:
        from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
    except ImportError as exc:
        raise RuntimeError("cryptography is required for encrypted WeCom callbacks") from exc
    key = base64.b64decode(_env("WECOM_ENCODING_AES_KEY") + "=")
    encrypted = base64.b64decode(ciphertext)
    decryptor = Cipher(algorithms.AES(key), modes.CBC(key[:16])).decryptor()
    padded = decryptor.update(encrypted) + decryptor.finalize()
    padding = padded[-1]
    if not 1 <= padding <= 32 or padded[-padding:] != bytes([padding]) * padding:
        raise ValueError("invalid WeCom callback padding")
    plain = padded[:-padding]
    if len(plain) < 20:
        raise ValueError("invalid WeCom callback payload")
    size = struct.unpack("!I", plain[16:20])[0]
    message = plain[20:20 + size]
    if len(message) != size:
        raise ValueError("invalid WeCom callback message length")
    corp_id = plain[20 + size:].decode()
    if corp_id != _env("WECOM_CORP_ID"):
        raise ValueError("WeCom callback corp id does not match")
    return message.decode()


def _xml_message(xml: str) -> dict:
    root = ElementTree.fromstring(xml)
    values = {child.tag: child.text or "" for child in root}
    msg_type = values.get("MsgType", "")
    return {"source": "wecom", "event_id": values.get("MsgId") or values.get("Event", ""),
            "user_id": values.get("FromUserName", ""), "conversation_id": values.get("ToUserName", ""),
            "message_type": msg_type, "text": values.get("Content", ""),
            "event": values.get("Event", ""), "raw": values}


def _trigger(input_data: dict) -> None:
    url = _env("ANCHOR_WEBHOOK_URL")
    request = urllib.request.Request(url, data=json.dumps({"input": input_data}).encode(), method="POST",
                                     headers={"Authorization": f"Bearer {_env('ANCHOR_API_KEY')}",
                                              "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            if response.status not in (200, 202):
                raise RuntimeError(f"Anchor webhook returned HTTP {response.status}")
    except (urllib.error.URLError, TimeoutError) as exc:
        raise RuntimeError(f"Anchor webhook request failed: {type(exc).__name__}") from exc


def _decode_callback(query: dict[str, list[str]], body: bytes | None = None) -> str:
    token = _env("WECOM_TOKEN")
    timestamp, nonce, signature = query.get("timestamp", [""])[0], query.get("nonce", [""])[0], query.get("msg_signature", [""])[0]
    if body is None:
        encrypted = query.get("echostr", [""])[0]
    else:
        values = {child.tag: child.text or "" for child in ElementTree.fromstring(body)}
        encrypted = values.get("Encrypt", "")
    if not encrypted or signature != _signature(token, timestamp, nonce, encrypted):
        raise ValueError("invalid WeCom callback signature")
    return _decrypt(encrypted)


class Handler(BaseHTTPRequestHandler):
    def _reply(self, status: int, body: str) -> None:
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:
        try:
            self._reply(200, _decode_callback(parse_qs(urlparse(self.path).query)))
        except (ValueError, RuntimeError) as exc:
            self._reply(400, str(exc))

    def do_POST(self) -> None:
        try:
            length = int(self.headers.get("Content-Length", "0"))
            xml = _decode_callback(parse_qs(urlparse(self.path).query), self.rfile.read(length))
            input_data = _xml_message(xml)
            threading.Thread(target=_trigger, args=(input_data,), daemon=True).start()
            self._reply(200, "success")
        except (ValueError, RuntimeError, ElementTree.ParseError) as exc:
            self._reply(400, str(exc))

    def log_message(self, *_args) -> None:
        return


def main() -> None:
    load_dotenv()
    host = os.environ.get("WECOM_LISTEN_HOST", "127.0.0.1")
    port = int(os.environ.get("WECOM_LISTEN_PORT", "8090"))
    ThreadingHTTPServer((host, port), Handler).serve_forever()


if __name__ == "__main__":
    main()
