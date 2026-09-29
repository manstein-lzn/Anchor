"""Small stdio MCP bridge for Docmost's native page attachment upload endpoint."""
from __future__ import annotations

import json
import os
from pathlib import Path
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid

MAX_UPLOAD_BYTES = 20 * 1024 * 1024
UPLOAD_ROOT = Path("/in/publish/assets")
MIME_TYPES = {".svg": "image/svg+xml", ".png": "image/png", ".jpg": "image/jpeg",
              ".jpeg": "image/jpeg", ".webp": "image/webp"}
SERVER_URL = "https://docmost.cwise.dev/api/files/upload"
def _attachment(path: str, page_id: str, attachment_id: str | None = None) -> dict:
    try:
        uuid.UUID(page_id)
        if attachment_id is not None:
            uuid.UUID(attachment_id)
    except ValueError as exc:
        raise ValueError("pageId and attachmentId must be UUIDs") from exc
    if not Path(path).is_absolute():
        raise ValueError("path must refer to a file under /in/publish/assets")
    candidate = Path(path).resolve(strict=True)
    root = UPLOAD_ROOT.resolve(strict=True)
    if not candidate.is_relative_to(root) or not candidate.is_file():
        raise ValueError("path must refer to a file under /in/publish/assets")
    suffix = candidate.suffix.lower()
    if suffix not in MIME_TYPES or candidate.name in (".", ".."):
        raise ValueError("only SVG, PNG, JPEG, and WebP report images can be uploaded")
    data = candidate.read_bytes()
    if not data or len(data) > MAX_UPLOAD_BYTES:
        raise ValueError("image must be non-empty and at most 20 MiB")

    token = os.environ.get("DOCMOST_API_KEY")
    if not token:
        raise ValueError("DOCMOST_API_KEY is not configured")
    boundary = "----AnchorDocmost" + uuid.uuid4().hex
    fields = []
    if attachment_id:
        fields.append(("attachmentId", attachment_id))
    fields.append(("pageId", page_id))
    body = b"".join(
        f"--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n".encode()
        for name, value in fields
    )
    body += (f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; "
             f"filename=\"{candidate.name}\"\r\nContent-Type: {MIME_TYPES[suffix]}\r\n\r\n").encode()
    body += data + f"\r\n--{boundary}--\r\n".encode()
    request = urllib.request.Request(
        SERVER_URL, data=body, method="POST",
        headers={"Authorization": f"Bearer {token}",
                 "Content-Type": f"multipart/form-data; boundary={boundary}"},
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            result = json.load(response)
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
        raise RuntimeError(f"Docmost image upload failed: {type(exc).__name__}") from exc
    if result.get("pageId") != page_id or result.get("mimeType") not in MIME_TYPES.values():
        raise RuntimeError("Docmost returned attachment metadata for a different page or file type")
    url = f"/api/files/{result['id']}/{urllib.parse.quote(result['fileName'])}"
    return {"attachmentId": result["id"], "fileName": result["fileName"], "url": url,
            "mimeType": result["mimeType"], "pageId": result["pageId"]}


TOOL = {
    "name": "upload_page_image",
    "description": "Upload a report image from /in/publish/assets to a Docmost page and return its Markdown URL. Set attachmentId to replace an existing image on that page.",
    "inputSchema": {
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Absolute path under /in/publish/assets"},
            "pageId": {"type": "string", "description": "Target Docmost page UUID"},
            "attachmentId": {"type": "string", "description": "Optional existing attachment UUID to replace"},
        },
        "required": ["path", "pageId"],
    },
}


def _handle(message: dict) -> dict | None:
    method = message.get("method")
    request_id = message.get("id")
    if request_id is None:
        return None
    if method == "initialize":
        result = {"protocolVersion": message.get("params", {}).get("protocolVersion", "2024-11-05"),
                  "capabilities": {"tools": {}},
                  "serverInfo": {"name": "docmost-attachments", "version": "1.0.0"}}
    elif method == "ping":
        result = {}
    elif method == "tools/list":
        result = {"tools": [TOOL]}
    elif method == "tools/call":
        params = message.get("params", {})
        if params.get("name") != TOOL["name"]:
            return {"jsonrpc": "2.0", "id": request_id,
                    "error": {"code": -32602, "message": "unknown tool"}}
        try:
            arguments = params.get("arguments", {})
            value = _attachment(arguments["path"], arguments["pageId"],
                                arguments.get("attachmentId"))
            result = {"content": [{"type": "text", "text": json.dumps(value)}], "isError": False}
        except Exception as exc:  # noqa: BLE001 - report tool errors through MCP, never as protocol noise
            result = {"content": [{"type": "text", "text": str(exc)}], "isError": True}
    else:
        return {"jsonrpc": "2.0", "id": request_id,
                "error": {"code": -32601, "message": "method not found"}}
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def main() -> None:
    for line in sys.stdin:
        message = None
        try:
            message = json.loads(line)
            response = _handle(message)
            if response is not None:
                sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
                sys.stdout.flush()
        except Exception as exc:  # noqa: BLE001 - keep stdio protocol clean
            request_id = message.get("id") if isinstance(message, dict) else None
            if request_id is not None:
                sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": request_id,
                                             "error": {"code": -32700, "message": str(exc)}}) + "\n")
                sys.stdout.flush()


if __name__ == "__main__":
    main()
