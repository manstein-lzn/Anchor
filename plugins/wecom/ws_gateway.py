"""Enterprise WeChat AI-bot WebSocket channel adapter.

The adapter owns the platform connection only. Anchor decides what the message means through
``ANCHOR_CHANNEL_WEBHOOK_URL`` and returns ``{"text": "..."}`` (or ``{"reply": "..."}``).
"""

from __future__ import annotations

import asyncio
import json
import os
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, Awaitable, Callable
from uuid import uuid4

from anchor.channel import ChannelEvent, EventLedger
from anchor.runtime.secrets import load_dotenv


def _required(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise ValueError(f"{name} is not configured")
    return value


def _body_text(body: dict[str, Any]) -> str:
    text = body.get("text")
    if isinstance(text, dict) and isinstance(text.get("content"), str):
        return text["content"]
    return ""


def normalize_message(frame: dict[str, Any]) -> ChannelEvent | None:
    """Convert an ``aibot_msg_callback`` frame into Anchor's channel contract."""
    if frame.get("cmd") != "aibot_msg_callback":
        return None
    body = frame.get("body")
    if not isinstance(body, dict):
        return None
    headers = frame.get("headers")
    headers = headers if isinstance(headers, dict) else {}
    sender = body.get("from")
    sender_id = sender.get("userid", "") if isinstance(sender, dict) else ""
    conversation = body.get("chatid") or sender_id
    event_id = body.get("msgid") or headers.get("req_id")
    if not all(isinstance(value, str) and value for value in (sender_id, conversation, event_id)):
        return None
    return ChannelEvent(
        source="wecom",
        event_id=event_id,
        sender_id=sender_id,
        conversation_id=conversation,
        reply_target=conversation,
        text=_body_text(body),
        message_type=str(body.get("msgtype") or "unknown"),
        metadata={"chat_type": body.get("chattype", "single"), "request_id": headers.get("req_id", "")},
    )


Handler = Callable[[ChannelEvent], Awaitable[str | None]]


class WeComWebSocketGateway:
    """Run one long-lived Enterprise WeChat bot connection."""

    def __init__(self, handler: Handler, *, state_path: Path):
        self.handler = handler
        self.ledger = EventLedger(state_path)
        self.client: Any = None

    async def run(self) -> None:
        try:
            from aibot import WSClient, WSClientOptions
        except ImportError as exc:
            raise RuntimeError(
                "install the channel extra first: pip install 'anchor-agent-product[channels]'"
            ) from exc

        options = WSClientOptions(
            bot_id=_required("WECOM_BOT_ID"),
            secret=_required("WECOM_BOT_SECRET"),
            max_reconnect_attempts=-1,
        )
        self.client = WSClient(options)

        async def on_message(frame: dict[str, Any]) -> None:
            event = normalize_message(frame)
            if event is None:
                return
            if not self.ledger.claim(event):
                reply = self.ledger.pending_reply(event)
                if reply is None:
                    return
                try:
                    if reply:
                        await self.client.reply_stream(
                            frame, stream_id=f"anchor-{uuid4().hex}", content=reply, finish=True
                        )
                    self.ledger.complete(event)
                except Exception as exc:  # noqa: BLE001 - ready reply remains retryable
                    self.ledger.fail_delivery(event, f"{type(exc).__name__}: {exc}")
                return
            try:
                reply = await self.handler(event)
                self.ledger.prepare_reply(event, reply or "")
                if reply:
                    await self.client.reply_stream(
                        frame, stream_id=f"anchor-{uuid4().hex}", content=reply, finish=True
                    )
                self.ledger.complete(event)
            except Exception as exc:  # noqa: BLE001 - the event remains retryable
                self.ledger.fail(event, f"{type(exc).__name__}: {exc}")

        self.client.on("message", on_message)
        await self.client.connect()
        await asyncio.Event().wait()


def _post_event(event: ChannelEvent) -> str | None:
    url = _required("ANCHOR_CHANNEL_WEBHOOK_URL")
    payload = json.dumps({"event": event.as_dict()}, ensure_ascii=False).encode()
    request = urllib.request.Request(
        url,
        data=payload,
        method="POST",
        headers={
            "Authorization": f"Bearer {_required('ANCHOR_API_KEY')}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            value = json.loads(response.read())
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
        raise RuntimeError(f"Anchor channel handler failed: {type(exc).__name__}") from exc
    if not isinstance(value, dict):
        raise RuntimeError("Anchor channel handler returned a non-object response")
    reply = value.get("text", value.get("reply"))
    return reply if isinstance(reply, str) and reply.strip() else None


async def main() -> None:
    load_dotenv()
    state = Path(os.environ.get("WECOM_CHANNEL_STATE", ".local/wecom-channel"))
    await WeComWebSocketGateway(_post_event, state_path=state / "events.sqlite").run()


if __name__ == "__main__":
    asyncio.run(main())
