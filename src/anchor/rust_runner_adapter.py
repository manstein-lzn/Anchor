"""Bounded client for the Rust Runner compatibility host.

This module is deliberately an adapter, not a scheduler.  The caller owns
Graph admission and Run lifecycle; the Rust host owns the compatibility Run
record.  The protocol carries a frozen snapshot and JSON input only, never
host paths, commands, credentials, or sandbox policy.
"""

from __future__ import annotations

import json
import os
import struct
import subprocess
from collections.abc import Mapping, Sequence
from typing import Any

PROTOCOL_VERSION = 1
MAX_FRAME = 1024 * 1024
_STATUSES = {
    "ready",
    "running",
    "paused",
    "budget_stopped",
    "waiting_call",
    "completed",
    "stopped",
    "failed",
}


class RustRunnerProtocolError(RuntimeError):
    """The host rejected or violated the compatibility protocol."""


class RustRunnerAdapter:
    """Invoke one configured Rust host process per request.

    Spawning per request keeps process ownership explicit while the host's
    FileRunStore provides idempotent Run identity and recovery.  A later
    Scheduler integration can add admission/lifecycle orchestration around
    this adapter without adding a second queue here.
    """

    def __init__(
        self,
        command: Sequence[str],
        *,
        environment: Mapping[str, str] | None = None,
        cwd: str | None = None,
        timeout: float = 30.0,
    ) -> None:
        if not command or any(not isinstance(part, str) or not part for part in command):
            raise ValueError("Rust Runner command must be a non-empty argv")
        if timeout <= 0:
            raise ValueError("Rust Runner timeout must be positive")
        self._command = tuple(command)
        self._environment = dict(environment or {})
        self._cwd = cwd
        self._timeout = timeout

    def start_or_resume(
        self,
        *,
        request_id: str,
        run_id: str,
        snapshot: Mapping[str, Any],
        input: Any,
    ) -> dict[str, Any]:
        return self._request(
            {
                "op": "start_or_resume",
                "version": PROTOCOL_VERSION,
                "request_id": request_id,
                "run_id": run_id,
                "snapshot": dict(snapshot),
                "input": input,
            }
        )

    def status(self, *, request_id: str, run_id: str) -> dict[str, Any]:
        return self._request(
            {
                "op": "status",
                "version": PROTOCOL_VERSION,
                "request_id": request_id,
                "run_id": run_id,
            }
        )

    def start_bundle(
        self, *, request_id: str, run_id: str, input: Any
    ) -> dict[str, Any]:
        """Start the server-configured Rust-native Graph bundle.

        The bundle location is configured in the host environment; it is
        intentionally absent from the request so callers cannot select paths.
        """
        return self._request(
            {
                "op": "start_bundle",
                "version": PROTOCOL_VERSION,
                "request_id": request_id,
                "run_id": run_id,
                "input": input,
            }
        )

    def _request(self, request: dict[str, Any]) -> dict[str, Any]:
        self._validate_request(request)
        payload = _encode_frame(request)
        environment = os.environ.copy()
        environment.update(self._environment)
        try:
            completed = subprocess.run(
                self._command,
                input=payload,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                cwd=self._cwd,
                env=environment,
                timeout=self._timeout,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise RustRunnerProtocolError(f"Rust Runner process failed: {error}") from error
        if completed.returncode != 0:
            detail = completed.stderr.decode("utf-8", "replace")[-2000:]
            raise RustRunnerProtocolError(
                f"Rust Runner exited with {completed.returncode}: {detail}"
            )
        response = _decode_single_frame(completed.stdout)
        _validate_response(response, request["request_id"])
        return response

    @staticmethod
    def _validate_request(request: Mapping[str, Any]) -> None:
        request_id = request.get("request_id")
        run_id = request.get("run_id")
        if not isinstance(request_id, str) or not request_id:
            raise RustRunnerProtocolError("request_id must be a non-empty string")
        if not isinstance(run_id, str) or not run_id or run_id in {".", ".."} or any(
            not (char.isascii() and (char.isalnum() or char in "-_."))
            for char in run_id
        ):
            raise RustRunnerProtocolError("run_id contains an unsafe character")
        if request["op"] == "start_or_resume" and not isinstance(
            request.get("snapshot"), dict
        ):
            raise RustRunnerProtocolError("snapshot must be a JSON object")


def _encode_frame(value: Mapping[str, Any]) -> bytes:
    try:
        data = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise RustRunnerProtocolError(f"request is not JSON serializable: {error}") from error
    if not data or len(data) > MAX_FRAME:
        raise RustRunnerProtocolError("request exceeds the 1 MiB frame limit")
    return struct.pack(">I", len(data)) + data


def _decode_single_frame(data: bytes) -> dict[str, Any]:
    if len(data) < 4:
        raise RustRunnerProtocolError("host returned an incomplete frame")
    size = struct.unpack(">I", data[:4])[0]
    if not size or size > MAX_FRAME or len(data) != size + 4:
        raise RustRunnerProtocolError("host returned an invalid frame")
    try:
        value = json.loads(data[4:].decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RustRunnerProtocolError(f"host returned invalid JSON: {error}") from error
    if not isinstance(value, dict):
        raise RustRunnerProtocolError("host response must be a JSON object")
    return value


def _validate_response(response: Mapping[str, Any], request_id: str) -> None:
    if response.get("version") != PROTOCOL_VERSION:
        raise RustRunnerProtocolError("unsupported host response version")
    if response.get("request_id") != request_id:
        raise RustRunnerProtocolError("host response request_id mismatch")
    kind = response.get("kind")
    if kind == "run":
        expected = {"kind", "version", "request_id", "run_id", "status", "error"}
        if set(response) != expected or not isinstance(response["run_id"], str):
            raise RustRunnerProtocolError("invalid run response shape")
        if response["status"] not in _STATUSES:
            raise RustRunnerProtocolError("invalid Run status")
    elif kind == "missing":
        if set(response) != {"kind", "version", "request_id", "run_id"}:
            raise RustRunnerProtocolError("invalid missing response shape")
    elif kind == "rejected":
        if set(response) != {"kind", "version", "request_id", "reason"}:
            raise RustRunnerProtocolError("invalid rejected response shape")
        if not isinstance(response["reason"], str):
            raise RustRunnerProtocolError("rejection reason must be text")
    else:
        raise RustRunnerProtocolError("unknown host response kind")
