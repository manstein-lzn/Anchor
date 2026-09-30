"""Small authenticated local RPC to the already-connected channel process."""
from __future__ import annotations

import asyncio
import hashlib
import hmac
import json
import os
from pathlib import Path
import socket
from typing import Any, Awaitable, Callable

from anchor.channel import ChannelEvent, EventLedger

MAX_REQUEST = 64 * 1024


def request(path: Path, token: str, payload: dict) -> dict:
    data = json.dumps({**payload, "token": token}, ensure_ascii=False).encode() + b"\n"
    if len(data) > MAX_REQUEST:
        raise ValueError("channel request is too large")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(20)
        client.connect(str(path))
        client.sendall(data)
        with client.makefile("rb") as stream:
            response = stream.readline(MAX_REQUEST + 1)
    if len(response) > MAX_REQUEST:
        raise ValueError("channel response is too large")
    result = json.loads(response)
    if result.get("error"):
        raise RuntimeError(result["error"])
    return result


class ControlServer:
    """No TCP listener and no node access to the gateway's credentials.

    Sends are claimed before dispatch. A lost ACK is unknown, never auto-retried.
    The existing ledger stores this fact; it is not a second task scheduler.
    """

    def __init__(self, path: Path, token: str, ledger: EventLedger,
                 send: Callable[[str, dict], Awaitable[Any]]):
        self.path, self.token, self.ledger, self.send = path, token, ledger, send
        self.server: asyncio.Server | None = None

    async def start(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        # This path is private to the supervisor-owned gateway, whose old process has exited.
        if self.path.exists():
            self.path.unlink()
        self.server = await asyncio.start_unix_server(self._handle, path=str(self.path), limit=MAX_REQUEST)
        self.path.chmod(0o600)

    async def close(self) -> None:
        if self.server:
            self.server.close()
            await self.server.wait_closed()
        self.path.unlink(missing_ok=True)

    async def _handle(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        try:
            data = await asyncio.wait_for(reader.readline(), timeout=5)
            if len(data) > MAX_REQUEST:
                raise ValueError("channel request is too large")
            payload = json.loads(data)
            token = payload.pop("token", "")
            if not isinstance(token, str) or not hmac.compare_digest(token, self.token):
                raise ValueError("unauthorized channel request")
            result = await self.dispatch(payload)
        except Exception as exc:  # noqa: BLE001 - report protocol failure without echoing credentials
            result = {"error": str(exc) if isinstance(exc, (ValueError, RuntimeError)) else type(exc).__name__}
        writer.write(json.dumps(result, ensure_ascii=False).encode() + b"\n")
        try:
            await writer.drain()
        finally:
            writer.close()
            await writer.wait_closed()

    async def dispatch(self, payload: dict) -> dict:
        if set(payload) != {"operation", "request_id", "userid", "content"} or payload["operation"] != "send":
            raise ValueError("unsupported channel operation")
        userid, content, identifier = payload["userid"], payload["content"], payload["request_id"]
        if not all(isinstance(value, str) and value.strip() for value in (userid, content, identifier)):
            raise ValueError("userid, content and request_id are required")
        if len(content.encode()) > 20480 or len(userid) > 200 or len(identifier) > 500:
            raise ValueError("message or identifier exceeds platform limits")
        allowed = {v.strip() for v in os.environ.get("ANCHOR_WECOM_SEND_USERS", "").split(",") if v.strip()}
        if userid == "@all" or (userid not in allowed and "*" not in allowed):
            raise ValueError("recipient is not allowed")
        event = ChannelEvent(source="wecom-outbound", event_id=identifier, sender_id="anchor",
                             conversation_id=userid, text=content)
        digest = hashlib.sha256(json.dumps([userid, content], ensure_ascii=False).encode()).hexdigest()
        previous = self.ledger.claim_send(event, digest)
        if previous is not None:
            return previous
        try:
            ack = await self.send(userid, {"msgtype": "markdown", "markdown": {"content": content}})
            if not isinstance(ack, dict) or ack.get("errcode") != 0:
                raise RuntimeError("platform did not confirm message acceptance")
        except Exception as exc:  # noqa: BLE001 - preserve unknown sends without replay
            self.ledger.fail(event, "delivery uncertain: " + type(exc).__name__)
            raise RuntimeError("delivery not confirmed; do not resend automatically") from exc
        self.ledger.complete(event)
        return {"accepted": True, "request_id": identifier}
