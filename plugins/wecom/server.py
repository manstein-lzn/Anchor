"""Small stdio MCP server for the WeCom application API."""
from __future__ import annotations

import json
import os
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from pathlib import Path

API_BASE = "https://qyapi.weixin.qq.com"
_token_lock = threading.Lock()
_token: tuple[str, float] | None = None


def _config(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise ValueError(f"{name} is not configured")
    return value


def _json_request(path: str, *, params: dict[str, str] | None = None,
                  body: dict | None = None) -> dict:
    api_base = os.environ.get("WECOM_API_BASE_URL", API_BASE).rstrip("/")
    url = f"{api_base}{path}"
    if params:
        url += "?" + urllib.parse.urlencode(params)
    data = json.dumps(body, ensure_ascii=False).encode() if body is not None else None
    request = urllib.request.Request(url, data=data, method="POST" if body is not None else "GET",
                                     headers={"Content-Type": "application/json"} if body is not None else {})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            result = json.loads(response.read())
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
        raise RuntimeError(f"WeCom API request failed: {type(exc).__name__}") from exc
    if not isinstance(result, dict):
        raise RuntimeError("WeCom API returned a non-object response")
    if result.get("errcode", 0) != 0:
        raise RuntimeError(f"WeCom API error {result.get('errcode')}: {result.get('errmsg', 'unknown error')}")
    return result


def _access_token() -> str:
    global _token
    now = time.time()
    with _token_lock:
        if _token and _token[1] > now + 60:
            return _token[0]
        result = _json_request("/cgi-bin/gettoken", params={
            "corpid": _config("WECOM_CORP_ID"), "corpsecret": _config("WECOM_SECRET")})
        token = result.get("access_token")
        if not isinstance(token, str) or not token:
            raise RuntimeError("WeCom API did not return access_token")
        _token = (token, now + float(result.get("expires_in", 7200)))
        return token


def _target(arguments: dict) -> dict:
    target = {key: str(arguments[key]) for key in ("touser", "toparty", "totag")
              if arguments.get(key) not in (None, "")}
    if not target:
        raise ValueError("one of touser, toparty, or totag is required")
    return target


def send_message(arguments: dict, msgtype: str) -> dict:
    content = arguments.get("content")
    if not isinstance(content, str) or not content.strip():
        raise ValueError("content is required")
    body = {**_target(arguments), "msgtype": msgtype, "agentid": int(_config("WECOM_AGENT_ID")),
            msgtype: {"content": content}}
    return _json_request("/cgi-bin/message/send", params={"access_token": _access_token()}, body=body)


def get_user(userid: str) -> dict:
    if not isinstance(userid, str) or not userid.strip():
        raise ValueError("userid is required")
    return _json_request("/cgi-bin/user/get", params={"access_token": _access_token(), "userid": userid})


TOOLS = [
    {"name": "wecom_send_text", "description": "Send a text message through a WeCom application.",
     "inputSchema": {"type": "object", "properties": {
         "content": {"type": "string"}, "touser": {"type": "string"},
         "toparty": {"type": "string"}, "totag": {"type": "string"}}, "required": ["content"]}},
    {"name": "wecom_send_markdown", "description": "Send a Markdown message through a WeCom application.",
     "inputSchema": {"type": "object", "properties": {
         "content": {"type": "string"}, "touser": {"type": "string"},
         "toparty": {"type": "string"}, "totag": {"type": "string"}}, "required": ["content"]}},
    {"name": "wecom_get_user", "description": "Get a WeCom member by userid.",
     "inputSchema": {"type": "object", "properties": {"userid": {"type": "string"}},
                     "required": ["userid"]}},
]


def _handle(message: dict) -> dict | None:
    request_id = message.get("id")
    if request_id is None:
        return None
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion": message.get("params", {}).get("protocolVersion", "2024-11-05"),
                  "capabilities": {"tools": {}},
                  "serverInfo": {"name": "wecom", "version": "1.0.0"}}
    elif method == "ping":
        result = {}
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        params = message.get("params", {})
        name, arguments = params.get("name"), params.get("arguments", {})
        try:
            if name == "wecom_send_text":
                value = send_message(arguments, "text")
            elif name == "wecom_send_markdown":
                value = send_message(arguments, "markdown")
            elif name == "wecom_get_user":
                value = get_user(arguments.get("userid"))
            else:
                return {"jsonrpc": "2.0", "id": request_id,
                        "error": {"code": -32602, "message": "unknown tool"}}
            result = {"content": [{"type": "text", "text": json.dumps(value, ensure_ascii=False)}],
                      "isError": False}
        except Exception as exc:  # noqa: BLE001 - report tool errors through MCP
            result = {"content": [{"type": "text", "text": str(exc)}], "isError": True}
    else:
        return {"jsonrpc": "2.0", "id": request_id,
                "error": {"code": -32601, "message": "method not found"}}
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def main() -> None:
    # Graph MCP processes receive only manifest-approved environment variables. They do not
    # mount Anchor or its root .env into the sandbox. Direct CLI launches can load that file.
    if Path(".env").is_file():
        from anchor.runtime.secrets import load_dotenv
        load_dotenv()
    for line in sys.stdin:
        message = None
        try:
            message = json.loads(line)
            response = _handle(message)
            if response is not None:
                sys.stdout.write(json.dumps(response, separators=(",", ":"), ensure_ascii=False) + "\n")
                sys.stdout.flush()
        except Exception as exc:  # noqa: BLE001 - keep stdio protocol clean
            request_id = message.get("id") if isinstance(message, dict) else None
            if request_id is not None:
                sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": request_id,
                                             "error": {"code": -32700, "message": str(exc)}}) + "\n")
                sys.stdout.flush()


if __name__ == "__main__":
    main()
