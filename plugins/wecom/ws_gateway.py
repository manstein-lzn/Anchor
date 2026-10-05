"""Enterprise WeChat AI-bot WebSocket channel adapter.

The adapter owns the platform connection only. Anchor decides what the message means through
``ANCHOR_CHANNEL_WEBHOOK_URL`` and returns ``{"text": "..."}`` (or ``{"reply": "..."}`).
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import mimetypes
import os
import re
import time
import urllib.error
import urllib.request
import uuid
from dataclasses import replace
from pathlib import Path
from typing import Any, Awaitable, Callable

from anchor.channel import ChannelEvent, EventLedger
from anchor.channel.media import MAX_IMAGE_BYTES, make_image_item
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
    if len(specs) > 16:
        raise ValueError("too many WeCom attachments")
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


Handler = Callable[[ChannelEvent], Awaitable[str | dict | None]]


def _reply_value(value: str | dict | None) -> dict:
    return value if isinstance(value, dict) else {"text": value or ""}


def _saved_reply(reply: str) -> dict:
    try:
        saved = json.loads(reply)
        if isinstance(saved, dict) and "channel_reply" in saved:
            return saved["channel_reply"]
    except ValueError:
        pass
    return {"text": reply}


def _platform_text(text: str) -> str:
    if len(text.encode()) <= 20480:
        return text
    suffix = "\n\n（回复超出企业微信消息长度，完整内容保留在 Anchor 运行记录中。）"
    return text.encode()[:20480 - len(suffix.encode())].decode("utf-8", errors="ignore") + suffix


class WeComWebSocketGateway:
    """Run one long-lived Enterprise WeChat bot connection."""

    def __init__(self, handler: Handler, *, state_path: Path, stream_handler: Callable | None = None):
        self.handler = handler
        self.stream_handler = stream_handler
        self.ledger = EventLedger(state_path)
        self.client: Any = None
        self._admission: dict[str, asyncio.Lock] = {}

    async def _deliver(self, frame: dict, stream_id: str, reply: dict) -> None:
        kwargs = {"stream_id": stream_id, "content": _platform_text(
            reply.get("text") or "已根据你的补充继续处理。"), "finish": True}
        await self.client.reply_stream(frame, **kwargs)
        # Long connections silently ignore stream.msg_item. Upload media and reply using
        # the original callback instead (official protocol, document/path/101463).
        items = reply.get("msg_item") or []
        if not items:
            return
        event = normalize_message(frame)
        if event is None:
            raise ValueError("image reply requires a trusted message callback")
        for index, item in enumerate(items):
            if not self.ledger.is_latest(event):
                return
            encoded = item["image"]["base64"]
            if len(encoded) > 4 * ((MAX_IMAGE_BYTES + 2) // 3):
                raise ValueError("reply image exceeds the 10 MiB limit")
            data = base64.b64decode(encoded, validate=True)
            canonical = make_image_item(data)
            if item != canonical:
                raise ValueError("reply image content or digest is invalid")
            media_id = await self._upload_image(data, canonical["image"]["md5"])
            if not self.ledger.is_latest(event):
                return
            delivery = replace(event, source="wecom-reply-image", event_id=f"{event.event_id}:{index}")
            digest = hashlib.sha256(json.dumps([
                event.sender_id, event.conversation_id, hashlib.sha256(data).hexdigest(),
            ]).encode()).hexdigest()
            # Uploading is invisible to the user and can be retried. Once sending starts,
            # persist the outcome using the same non-replayable contract as tool sends.
            if self.ledger.claim_send(delivery, digest):
                continue
            try:
                ack = await self.client.reply(frame, {"msgtype": "image", "image": {"media_id": media_id}})
                self._check_media_ack(ack)
                self.ledger.complete(delivery)
            except Exception as exc:
                self.ledger.fail(delivery, type(exc).__name__)
                raise

    @staticmethod
    def _check_media_ack(ack: Any) -> dict:
        if not isinstance(ack, dict) or ack.get("errcode") != 0:
            raise RuntimeError("WeCom media operation was not acknowledged")
        return ack.get("body") or {}

    async def _upload_image(self, data: bytes, md5: str) -> str:
        async def command(name: str, body: dict) -> dict:
            frame = {"headers": {"req_id": "anchor-media-" + uuid.uuid4().hex}}
            return self._check_media_ack(await self.client.reply(frame, body, cmd=name))

        chunk_size = 512 * 1024
        upload = await command("aibot_upload_media_init", {
            "type": "image", "filename": "reply.png" if data.startswith(b"\x89PNG") else "reply.jpg",
            "total_size": len(data), "total_chunks": (len(data) + chunk_size - 1) // chunk_size,
            "md5": md5,
        })
        upload_id = upload.get("upload_id")
        if not isinstance(upload_id, str) or not upload_id:
            raise RuntimeError("WeCom upload acknowledgement omitted upload_id")
        for index, start in enumerate(range(0, len(data), chunk_size)):
            await command("aibot_upload_media_chunk", {
                "upload_id": upload_id, "chunk_index": index,
                "base64_data": base64.b64encode(data[start:start + chunk_size]).decode("ascii"),
            })
        finished = await command("aibot_upload_media_finish", {"upload_id": upload_id})
        media_id = finished.get("media_id")
        if not isinstance(media_id, str) or not media_id:
            raise RuntimeError("WeCom upload acknowledgement omitted media_id")
        return media_id

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
        control = None
        if os.environ.get("ANCHOR_CHANNEL_CONTROL_SOCKET"):
            from anchor.channel.control import ControlServer
            control = ControlServer(Path(_required("ANCHOR_CHANNEL_CONTROL_SOCKET")),
                                    _required("ANCHOR_CHANNEL_CONTROL_TOKEN"), self.ledger,
                                    self.client.send_message)

        async def on_message(frame: dict[str, Any]) -> None:  # noqa: C901 - ordered admission and delivery phases
            event = normalize_message(frame)
            if event is None:
                return
            stream_id = "anchor-" + hashlib.sha256(event.event_id.encode()).hexdigest()[:32]
            if not self.ledger.claim(event):
                reply = self.ledger.pending_reply(event)
                if reply is None:
                    return
                if not self.ledger.is_latest(event):
                    self.ledger.complete(event)
                    return
                try:
                    if reply:
                        # Old ledger rows stored plain text. New rows wrap the complete rich reply.
                        await self._deliver(frame, stream_id, _saved_reply(reply))
                    self.ledger.complete(event)
                except Exception as exc:  # noqa: BLE001 - ready reply remains retryable
                    self.ledger.fail_delivery(event, f"{type(exc).__name__}: {exc}")
                return
            if not self.ledger.is_latest(event):
                self.ledger.complete(event)
                return
            admission = self._admission.setdefault(event.conversation_id, asyncio.Lock())
            await admission.acquire()
            admitted = False

            def admit() -> None:
                nonlocal admitted
                if not admitted:
                    admitted = True
                    admission.release()

            def current() -> bool:
                return self.ledger.is_latest(event)

            # Admit input immediately; a slow progress ACK must not let a later message jump ahead.
            try:
                event = await _download_attachments(event, frame, self.client, self.ledger.path)
            except asyncio.CancelledError:
                admit()
                raise
            except Exception as exc:  # noqa: BLE001 - failed downloads remain retryable
                admit()
                self.ledger.fail(event, f"{type(exc).__name__}: {exc}")
                try:
                    await self.client.reply_stream(frame, stream_id=stream_id,
                        content="附件下载失败，请稍后重试。", finish=True)
                except Exception as delivery:  # noqa: BLE001 - original failure remains retryable
                    print(json.dumps({"reply_error": type(delivery).__name__}), flush=True)
                return
            progress_ready = asyncio.Event()

            async def progress(text: str) -> None:
                await progress_ready.wait()
                if not current():
                    return
                try:
                    await self.client.reply_stream(frame, stream_id=stream_id,
                                                   content=_platform_text(text), finish=False)
                except Exception as exc:  # noqa: BLE001 - partial delivery cannot rerun the Graph
                    print(json.dumps({"progress_error": type(exc).__name__}), flush=True)

            if not self.stream_handler:
                admit()
            handler_task = asyncio.create_task(self.stream_handler(event, progress, admit)
                if self.stream_handler else self.handler(event))
            handler_task.add_done_callback(lambda _: admit())
            try:
                await self.client.reply_stream(frame, stream_id=stream_id, content="正在处理…", finish=False)
            except asyncio.CancelledError:
                handler_task.cancel()
                await asyncio.gather(handler_task, return_exceptions=True)
                raise
            except Exception as exc:  # noqa: BLE001 - progress delivery must not prevent admission
                print(json.dumps({"progress_error": type(exc).__name__}), flush=True)
            finally:
                progress_ready.set()
            try:
                reply = _reply_value(await handler_task)
                if not current() or reply.get("superseded"):
                    reply = {"text": "已按你的新消息继续处理。", "superseded": True}
                self.ledger.prepare_reply(event, json.dumps({"channel_reply": reply}, ensure_ascii=False))
            except Exception as exc:  # noqa: BLE001 - the event remains retryable
                self.ledger.fail(event, f"{type(exc).__name__}: {exc}")
                try:
                    await self.client.reply_stream(frame, stream_id=stream_id,
                        content="本次处理未能完成，请稍后补充一条消息继续；已执行的操作不会自动撤销。", finish=True)
                except Exception as delivery:  # noqa: BLE001 - original failure remains retryable
                    print(json.dumps({"reply_error": type(delivery).__name__}), flush=True)
                return
            try:
                await self._deliver(frame, stream_id, reply)
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
            if control:
                await control.start()
            await self.client.connect()
            while True:
                await asyncio.sleep(1)
                # SDK 1.0.2 retries abnormal disconnects, but a normal close exits its reader
                # without scheduling reconnect. Use only its public lifecycle/status interface.
                if not getattr(self.client, "is_connected", True) and not connection["reconnecting"]:
                    self.client.disconnect()
                    await self.client.connect()
        finally:
            if control:
                await control.close()
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


def _post_stream(event: ChannelEvent, progress: Callable[[str], None],
                 admitted: Callable[[], None] | None = None) -> dict:
    """Read existing turn projections; a broken transport does not submit another Graph Run."""
    request = urllib.request.Request(_required("ANCHOR_CHANNEL_WEBHOOK_URL"),
        data=json.dumps({"event": event.as_dict()}, ensure_ascii=False).encode(), method="POST",
        headers={"Authorization": f"Bearer {_required('ANCHOR_API_KEY')}",
                 "Content-Type": "application/json", "Accept": "text/event-stream"})
    with urllib.request.urlopen(request, timeout=30) as response:
        if admitted:
            admitted()
        kind = ""
        for line in response:
            line = line.decode("utf-8").rstrip("\r\n")
            if line.startswith("event:"):
                kind = line[6:].strip()
            elif line.startswith("data:"):
                value = json.loads(line[5:])
                if kind == "progress":
                    progress(value["text"])
                elif kind == "reply":
                    if value.get("error"):
                        raise RuntimeError(value["error"])
                    return value
    raise RuntimeError("channel stream closed without a final reply")


async def _handle_stream(event: ChannelEvent, progress: Callable[[str], Awaitable[None]],
                         admitted: Callable[[], None] | None = None) -> dict:
    loop = asyncio.get_running_loop()
    queue: asyncio.Queue[str] = asyncio.Queue(maxsize=1)

    def offer(text: str) -> None:
        if queue.full():
            queue.get_nowait()
        queue.put_nowait(text)

    def changed(text: str) -> None:
        if not loop.is_closed():
            loop.call_soon_threadsafe(offer, text)

    def accepted() -> None:
        if admitted and not loop.is_closed():
            loop.call_soon_threadsafe(admitted)

    task = asyncio.create_task(asyncio.to_thread(_post_stream, event, changed, accepted))
    sent_at = 0.0
    try:
        while not task.done():
            await asyncio.sleep(0.05)
            if not queue.empty() and time.monotonic() - sent_at >= 0.5 and not task.done():
                await progress(queue.get_nowait())
                sent_at = time.monotonic()
        return await task
    finally:
        if not task.done():
            task.cancel()


async def main() -> None:
    load_dotenv()
    state = Path(os.environ.get("WECOM_CHANNEL_STATE", ".local/wecom-channel"))
    await WeComWebSocketGateway(_handle_event, state_path=state / "events.sqlite",
                               stream_handler=_handle_stream).run()


if __name__ == "__main__":
    asyncio.run(main())
