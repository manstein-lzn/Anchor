"""Enterprise WeChat AI-bot WebSocket channel adapter.

The adapter owns the platform connection only. Anchor decides what the message means through
``ANCHOR_CHANNEL_WEBHOOK_URL`` and returns ``{"text": "..."}`` (or ``{"reply": "..."}`).
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import mimetypes
import os
import re
import urllib.error
import urllib.request
from dataclasses import replace
from pathlib import Path
from typing import Any, Awaitable, Callable

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
    if body.get("msgtype") == "mixed":
        mixed = body.get("mixed")
        items = mixed.get("msg_item", []) if isinstance(mixed, dict) else []
        texts = []
        for item in items if isinstance(items, list) else []:
            if isinstance(item, dict) and isinstance(item.get("text"), dict):
                content = item["text"].get("content")
                if isinstance(content, str):
                    texts.append(content)
        return "\n".join(texts)
    return ""


def _attachment_specs(body: dict[str, Any]) -> list[dict[str, str]]:
    """Extract platform download facts without exposing the temporary URL to Anchor."""
    msgtype = body.get("msgtype")
    candidates: list[dict[str, Any]] = []
    if msgtype in {"image", "file"} and isinstance(body.get(msgtype), dict):
        candidates.append({"kind": msgtype, **body[msgtype]})
    elif msgtype == "mixed":
        mixed = body.get("mixed")
        items = mixed.get("msg_item", []) if isinstance(mixed, dict) else []
        for item in items if isinstance(items, list) else []:
            if not isinstance(item, dict):
                continue
            kind = item.get("msgtype")
            payload = item.get(kind) if kind in {"image", "file"} else None
            if isinstance(payload, dict):
                candidates.append({"kind": kind, **payload})
    result = []
    for item in candidates:
        url, aes_key = item.get("url"), item.get("aeskey") or item.get("aes_key")
        if isinstance(url, str) and url.startswith(("https://", "http://")):
            result.append({"kind": str(item["kind"]), "url": url,
                           "aes_key": aes_key if isinstance(aes_key, str) else ""})
    return result


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
        attachments=tuple({"kind": item["kind"]} for item in _attachment_specs(body)),
    )


def _safe_filename(value: str | None, kind: str, index: int) -> str:
    name = Path(value or "").name
    name = re.sub(r"[^\w.()\- ]", "_", name, flags=re.UNICODE).strip(" .")
    if not name:
        name = f"attachment-{index}{'.jpg' if kind == 'image' else '.bin'}"
    return name[:180]


async def _download_attachments(event: ChannelEvent, frame: dict[str, Any], client: Any,
                                state_path: Path) -> ChannelEvent:
    specs = _attachment_specs(frame.get("body", {}))
    if not specs:
        return event
    event_dir = state_path.parent / "events" / hashlib.sha256(event.event_id.encode()).hexdigest()
    event_dir.mkdir(parents=True, exist_ok=True)
    saved: list[dict[str, Any]] = []
    total = 0
    for index, item in enumerate(specs, 1):
        data, downloaded_name = await client.download_file(item["url"], item["aes_key"] or None)
        if not isinstance(data, bytes) or len(data) > 20 * 1024 * 1024:
            raise ValueError("WeCom attachment exceeds the 20 MiB limit")
        total += len(data)
        if total > 50 * 1024 * 1024:
            raise ValueError("WeCom attachments exceed the 50 MiB event limit")
        name = _safe_filename(downloaded_name, item["kind"], index)
        target = event_dir / f"{index:02d}-{name}"
        target.write_bytes(data)
        saved.append({"kind": item["kind"], "path": str(target), "name": name,
                      "mime_type": mimetypes.guess_type(name)[0] or
                      ("image/*" if item["kind"] == "image" else "application/octet-stream"),
                      "size": len(data)})
    return replace(event, attachments=tuple(saved))


Handler = Callable[[ChannelEvent], Awaitable[str | None]]


class WeComWebSocketGateway:
    """Run one long-lived Enterprise WeChat bot connection."""

    def __init__(self, handler: Handler, *, state_path: Path):
        self.handler = handler
        self.ledger = EventLedger(state_path)
        self.client: Any = None

    async def run(self) -> None:  # noqa: C901
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
            stream_id = "anchor-" + hashlib.sha256(event.event_id.encode()).hexdigest()[:32]
            if not self.ledger.claim(event):
                reply = self.ledger.pending_reply(event)
                if reply is None:
                    return
                try:
                    if reply:
                        await self.client.reply_stream(
                            frame, stream_id=stream_id, content=reply, finish=True
                        )
                    self.ledger.complete(event)
                except Exception as exc:  # noqa: BLE001 - ready reply remains retryable
                    self.ledger.fail_delivery(event, f"{type(exc).__name__}: {exc}")
                return
            # Admit input immediately; a slow progress ACK must not let a later message jump ahead.
            try:
                event = await _download_attachments(event, frame, self.client, self.ledger.path)
            except Exception as exc:  # noqa: BLE001 - failed downloads remain retryable
                self.ledger.fail(event, f"{type(exc).__name__}: {exc}")
                try:
                    await self.client.reply_stream(frame, stream_id=stream_id,
                        content="附件下载失败，请稍后重试。", finish=True)
                except Exception as delivery:  # noqa: BLE001 - original failure remains retryable
                    print(json.dumps({"reply_error": type(delivery).__name__}), flush=True)
                return
            handler_task = asyncio.create_task(self.handler(event))
            try:
                await self.client.reply_stream(frame, stream_id=stream_id, content="正在处理…", finish=False)
            except asyncio.CancelledError:
                handler_task.cancel()
                await asyncio.gather(handler_task, return_exceptions=True)
                raise
            except Exception as exc:  # noqa: BLE001 - progress delivery must not prevent admission
                print(json.dumps({"progress_error": type(exc).__name__}), flush=True)
            try:
                reply = await handler_task
                self.ledger.prepare_reply(event, reply or "")
            except Exception as exc:  # noqa: BLE001 - the event remains retryable
                self.ledger.fail(event, f"{type(exc).__name__}: {exc}")
                try:
                    await self.client.reply_stream(frame, stream_id=stream_id,
                        content="本次处理未能完成，请稍后补充一条消息继续；已执行的操作不会自动撤销。", finish=True)
                except Exception as delivery:  # noqa: BLE001 - original failure remains retryable
                    print(json.dumps({"reply_error": type(delivery).__name__}), flush=True)
                return
            try:
                await self.client.reply_stream(
                    frame, stream_id=stream_id, content=reply or "已根据你的补充继续处理。", finish=True
                )
                self.ledger.complete(event)
            except Exception as exc:  # noqa: BLE001 - preserve the reply for delivery retries
                self.ledger.fail_delivery(event, f"{type(exc).__name__}: {exc}")

        connection = {"reconnecting": False}
        self.client.on("reconnecting", lambda attempt: connection.update(reconnecting=True))
        self.client.on("authenticated", lambda: connection.update(reconnecting=False))
        self.client.on("error", lambda error: print(
            json.dumps({"wecom_connection_error": type(error).__name__}), flush=True))
        self.client.on("message", on_message)
        try:
            await self.client.connect()
            while True:
                await asyncio.sleep(1)
                # SDK 1.0.2 retries abnormal disconnects, but a normal close exits its reader
                # without scheduling reconnect. Use only its public lifecycle/status interface.
                if not getattr(self.client, "is_connected", True) and not connection["reconnecting"]:
                    self.client.disconnect()
                    await self.client.connect()
        finally:
            self.client.disconnect()


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
        with urllib.request.urlopen(request, timeout=130) as response:
            value = json.loads(response.read())
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
        raise RuntimeError(f"Anchor channel handler failed: {type(exc).__name__}") from exc
    if not isinstance(value, dict):
        raise RuntimeError("Anchor channel handler returned a non-object response")
    reply = value.get("text", value.get("reply"))
    return reply if isinstance(reply, str) and reply.strip() else None


async def _handle_event(event: ChannelEvent) -> str | None:
    return await asyncio.to_thread(_post_event, event)


async def main() -> None:
    load_dotenv()
    state = Path(os.environ.get("WECOM_CHANNEL_STATE", ".local/wecom-channel"))
    await WeComWebSocketGateway(_handle_event, state_path=state / "events.sqlite").run()


if __name__ == "__main__":
    asyncio.run(main())
