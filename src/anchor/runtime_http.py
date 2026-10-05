"""Explicit HTTP adapter to the Rust-owned Graph and Run application."""

from __future__ import annotations

from contextlib import contextmanager
from http.client import HTTPException
import json
import math
import os
from typing import Iterator
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

JSON_RESPONSE_LIMIT = 4 * 1024 * 1024


class RuntimeHTTPError(RuntimeError):
    def __init__(self, message: str, status: int = 503):
        super().__init__(message)
        self.status = status

    def response(self) -> tuple[str, int]:
        return json.dumps({"error": str(self), "backend": "rust"}, ensure_ascii=False), self.status


class _NoRedirects(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class RuntimeHTTPClient:
    """No retries or redirects: an unanswered mutation may already have executed."""

    def __init__(self, url: str, *, api_key: str = "", timeout: float = 15.0):
        parsed = urlsplit(url)
        try:
            parsed.port
        except ValueError as exc:
            raise ValueError("ANCHOR_RUNTIME_URL has an invalid port") from exc
        if (parsed.scheme not in {"http", "https"} or not parsed.hostname or
                parsed.username is not None or parsed.password is not None or parsed.query or parsed.fragment or
                any(ord(char) < 32 for char in url)):
            raise ValueError("ANCHOR_RUNTIME_URL must be an absolute HTTP(S) URL without credentials or query")
        if not math.isfinite(timeout) or timeout <= 0:
            raise ValueError("ANCHOR_RUNTIME_TIMEOUT_SECONDS must be positive and finite")
        if any(ord(char) < 32 for char in api_key):
            raise ValueError("ANCHOR_RUNTIME_API_KEY contains an invalid character")
        self.url = url.rstrip("/")
        self.api_key = api_key
        self.timeout = timeout

    @classmethod
    def from_env(cls) -> RuntimeHTTPClient | None:
        backend = os.environ.get("ANCHOR_RUNTIME_BACKEND", "python").strip().lower()
        if backend == "python":
            return None
        if backend != "rust":
            raise ValueError("ANCHOR_RUNTIME_BACKEND must be python or rust")
        url = os.environ.get("ANCHOR_RUNTIME_URL", "").strip()
        if not url:
            raise ValueError("ANCHOR_RUNTIME_URL is required for the Rust backend")
        try:
            timeout = float(os.environ.get("ANCHOR_RUNTIME_TIMEOUT_SECONDS", "15"))
        except ValueError as exc:
            raise ValueError("ANCHOR_RUNTIME_TIMEOUT_SECONDS must be positive and finite") from exc
        return cls(url, api_key=os.environ.get("ANCHOR_RUNTIME_API_KEY", ""), timeout=timeout)

    def _open(self, method: str, path: str, body: dict | None = None):
        if not path.startswith("/") or path.startswith("//"):
            raise ValueError("runtime request path must be relative to the configured endpoint")
        headers = {"Accept": "application/json"}
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        if self.api_key:
            headers["Authorization"] = f"Bearer {self.api_key}"
        request = Request(self.url + path, data=data, headers=headers, method=method)
        try:
            return build_opener(_NoRedirects()).open(request, timeout=self.timeout)
        except HTTPError as response:
            return response
        except (URLError, OSError, HTTPException, ValueError) as exc:
            raise RuntimeHTTPError("Rust runtime is unavailable; no Python execution was started") from exc

    def request(self, method: str, path: str, body: dict | None = None) -> tuple[dict, int]:
        try:
            with self._open(method, path, body) as response:
                status = response.status
                payload = response.read(JSON_RESPONSE_LIMIT + 1)
        except (OSError, URLError, HTTPException) as exc:
            raise RuntimeHTTPError("Rust runtime response was interrupted") from exc
        if len(payload) > JSON_RESPONSE_LIMIT:
            raise RuntimeHTTPError("Rust runtime JSON response exceeds the size limit", 502)
        if status == 204 and not payload:
            return {}, status
        try:
            value = json.loads(payload)
        except (ValueError, UnicodeDecodeError) as exc:
            raise RuntimeHTTPError("Rust runtime returned an invalid JSON response", 502) from exc
        if not isinstance(value, dict):
            raise RuntimeHTTPError("Rust runtime returned an invalid JSON response", 502)
        return value, status

    @contextmanager
    def download(self, path: str) -> Iterator:
        with self._open("GET", path + "?download=1") as response:
            if response.status != 200:
                try:
                    value = json.loads(response.read(JSON_RESPONSE_LIMIT + 1))
                except (ValueError, UnicodeDecodeError):
                    value = {}
                message = value.get("error") if isinstance(value, dict) else None
                raise RuntimeHTTPError(message or "Rust runtime file download failed", response.status)
            yield response


def resource_path(kind: str, identifier: str) -> str:
    if not identifier or identifier in {".", ".."} or "/" in identifier or "\\" in identifier:
        raise RuntimeHTTPError("invalid resource id", 400)
    if any(ord(char) < 32 for char in identifier):
        raise RuntimeHTTPError("invalid resource id", 400)
    return f"/{kind}/{quote(identifier, safe='')}"


def files_path(run: str, node: str, name: str | None = None) -> str:
    path = resource_path("runs", run)
    if (not node or "\\" in node or any(part in {"", ".", ".."} for part in node.split("/")) or
            any(ord(char) < 32 for char in node)):
        raise RuntimeHTTPError("invalid node id", 400)
    path += f"/files/{quote(node, safe='')}"
    if name is not None:
        if (not name or "\\" in name or any(part in {"", ".", ".."} for part in name.split("/")) or
                any(ord(char) < 32 for char in name)):
            raise RuntimeHTTPError("invalid file path", 400)
        path += f"/{quote(name, safe='/')}"
    return path
