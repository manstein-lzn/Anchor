"""Scoped tools contributed by a mounted WeCom channel, using its one host gateway."""
from __future__ import annotations

import asyncio
import hashlib
import json
import os
from pathlib import Path
from typing import Any, Callable

from pydantic_ai import RunContext
from pydantic_ai.toolsets import FunctionToolset

from anchor.channel.media import read_image_item


def reply_path(workspace: Path, run_id: str, node: str) -> Path:
    return workspace / "runs" / run_id / "control" / node / "channel-reply.json"


def _visible_file(path: str, directory: Path, resources: tuple[tuple[str, str], ...]) -> Path:
    virtual = Path(path)
    if not virtual.is_absolute():
        virtual = Path("/workspace") / virtual
    if ".." in virtual.parts or ".git" in virtual.parts:
        raise ValueError("image path must stay inside an allowed workspace/input")
    for host, mount in sorted(((str(directory), "/workspace"), *resources), key=lambda p: len(p[1]), reverse=True):
        base = Path(mount)
        if virtual.is_relative_to(base):
            relative = virtual.relative_to(base)
            root = Path(host).resolve()
            target = root / relative
            if any(part.is_symlink() for part in (target, *target.parents) if part != root and part.is_relative_to(root)):
                raise ValueError("symlink images are not allowed")
            if not target.resolve().is_relative_to(root) or not target.is_file():
                raise ValueError("image must be a file in this node's workspace/input")
            return target
    raise ValueError("image path is not visible to this node")


def factory(scheduler: Any, workspace: Path, identifier: str, *, reply_node: str = "",
            cancelled: Callable[[], bool] | None = None) -> Callable:
    """Called by the Graph runner only with trusted node/plugin/mount facts."""
    def attach(node: str, plugins: tuple[str, ...], directory: Path,
               resources: tuple[tuple[str, str], ...]) -> tuple:
        if "wecom" not in plugins:
            return ()
        toolset = FunctionToolset(id="wecom-bot")

        def check_active() -> None:
            if cancelled and cancelled():
                raise ValueError("this Run has been stopped; channel operation cancelled")

        @toolset.tool
        async def wecom_send_message(ctx: RunContext[Any], userid: str, content: str) -> dict:
            """主动向明确指定且获用户授权的企业成员发送 Markdown。不是回复当前消息。

            userid 必须是已知的真实成员 ID，不能猜测姓名对应 ID。不支持群聊或 @all。
            accepted 仅表示平台已确认接收，不表示对方已读；错误或超时不得自动重发。
            """
            check_active()
            allowed = {s.strip() for s in (os.environ.get("ANCHOR_WECOM_SEND_USERS") or
                       os.environ.get("ANCHOR_WECOM_USERS", "")).split(",") if s.strip()}
            if userid == "@all" or (userid not in allowed and "*" not in allowed):
                raise ValueError("recipient is not allowed")
            supervisor = scheduler.channel_supervisor
            if supervisor is None:
                raise ValueError("WeCom gateway is unavailable; no message was sent")
            request_id = hashlib.sha256(json.dumps(
                [identifier, node, ctx.tool_call_id], ensure_ascii=False).encode()).hexdigest()
            def dispatch() -> dict:
                check_active()
                return supervisor.send("wecom", {
                    "operation": "send", "request_id": request_id, "userid": userid, "content": content})
            return await asyncio.to_thread(dispatch)

        if node == reply_node:
            @toolset.tool_plain(sequential=True)
            def wecom_attach_image(path: str) -> dict:
                """将当前工作区或本轮只读输入中的 PNG/JPEG 附到本轮最终答复。

                先保存真实图片再传入路径，每张不超过 10 MiB。此工具只准备图片，不立即发送。
                最终 summary 写给用户的正文，图片由网关随正文一起回复。
                """
                check_active()
                target = _visible_file(path, directory, resources)
                item = read_image_item(target)
                output = reply_path(workspace, identifier, node)
                values = json.loads(output.read_text()) if output.exists() else []
                if any(value["image"]["md5"] == item["image"]["md5"] for value in values):
                    return {"attached": True, "duplicate": True, "name": target.name}
                if len(values) >= 10 or sum(len(v["image"]["base64"]) for v in values) + len(item["image"]["base64"]) > 14 * 1024 * 1024:
                    raise ValueError("reply images exceed the count/total size limit")
                values.append(item)
                output.parent.mkdir(parents=True, exist_ok=True)
                temp = output.with_suffix(".tmp")
                temp.write_text(json.dumps(values))
                temp.replace(output)
                return {"attached": True, "name": target.name, "count": len(values)}
        return (toolset,)
    return attach
