"""ToolGateway: policy-checked, sandboxed tool execution.

Boundary: Anchor owns the *policy* (deny by default, scope checks, secret
hygiene, ledger-first execution). Isolation primitives are *assembled*:
commands run behind a `SandboxBackend` — bubblewrap for real unprivileged
isolation today, stronger runtimes (nsjail, gVisor, Firecracker) later
without changing the gateway.

v1 rules:
- Tool results leave the sandbox only through gateway-ferried artifacts.
  Tools write solely to their private `/workspace`; everything else is
  read-only and the network is absent.
- Read-only tools execute. Tools marked `side_effect` are denied until the
  approval machinery lands (P1); unknown tools and out-of-scope agents are
  denied. No credentials ever enter arguments or backend environments.
"""

from __future__ import annotations

import logging
import shutil
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol
from uuid import UUID

from pydantic import ConfigDict

from anchor.domain.models import DomainModel
from anchor.domain.operations import OperationStatus, ToolOperation
from anchor.runtime.artifacts import ArtifactStore
from anchor.state.errors import OperationConflict
from anchor.runtime.capabilities import CapabilityRegistry, CapabilityRegistryError


logger = logging.getLogger("anchor.tool_gateway")

OUTPUT_LIMIT = 1_000_000
DEFAULT_TIMEOUT_SECONDS = 30.0
WORKSPACE_MOUNT = "/tmp"
# NOTE: bubblewrap creates mount points top-down, so the workspace reuses the
# existing /tmp (mounted over read-only /) instead of a fresh directory.

_SECRET_KEY_HINTS = ("secret", "password", "token", "api_key", "apikey", "private_key")


class ToolDenied(RuntimeError):
    """Raised when policy refuses a tool call. Never executes anything."""

    def __init__(self, code: str, message: str) -> None:
        self.code = code
        super().__init__(f"{code}: {message}")


@dataclass(frozen=True)
class SandboxResult:
    returncode: int
    stdout: bytes
    stderr: bytes
    timed_out: bool


class SandboxBackend(Protocol):
    name: str

    def run(self, argv: list[str], *, timeout_seconds: float) -> SandboxResult:
        """Run argv isolated; stdout/stderr already size-capped."""
        ...


def _capped(data: bytes) -> bytes:
    if len(data) > OUTPUT_LIMIT:
        return data[:OUTPUT_LIMIT] + b"\n[truncated:output-limit]"
    return data


class BubblewrapBackend:
    """Unprivileged isolation via bubblewrap: no network, read-only root,
    private writable workspace, scrubbed environment."""

    name = "bubblewrap"

    def __init__(self, binary: str = "bwrap") -> None:
        if shutil.which(binary) is None:
            raise RuntimeError(f"sandbox binary not found: {binary}")
        self.binary = binary

    def run(self, argv: list[str], *, timeout_seconds: float) -> SandboxResult:
        with tempfile.TemporaryDirectory(prefix="anchor-tool-") as workspace:
            cmd = [
                self.binary,
                "--unshare-all", "--die-with-parent",
                "--ro-bind", "/", "/",
                "--bind", workspace, WORKSPACE_MOUNT,
                "--proc", "/proc", "--dev", "/dev",
                "--chdir", WORKSPACE_MOUNT,
                "--setenv", "TMPDIR", WORKSPACE_MOUNT,
                "--clearenv",
                "--setenv", "PATH", "/usr/bin:/bin",
                "--setenv", "HOME", WORKSPACE_MOUNT,
                "--", *argv,
            ]
            try:
                completed = subprocess.run(
                    cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    timeout=timeout_seconds, check=False)
            except subprocess.TimeoutExpired as exc:
                return SandboxResult(returncode=124, stdout=_capped(exc.stdout or b""),
                                     stderr=_capped(exc.stderr or b""), timed_out=True)
            return SandboxResult(returncode=completed.returncode,
                                 stdout=_capped(completed.stdout),
                                 stderr=_capped(completed.stderr), timed_out=False)


class SubprocessBackend:
    """Development-only fallback: restricted cwd/env/timeout, NO isolation.

    Never use for untrusted tools. Its existence keeps the gateway testable
    where bubblewrap is unavailable.
    """

    name = "subprocess-dev"

    def run(self, argv: list[str], *, timeout_seconds: float) -> SandboxResult:
        with tempfile.TemporaryDirectory(prefix="anchor-tool-") as workspace:
            try:
                completed = subprocess.run(
                    argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    timeout=timeout_seconds, check=False, cwd=workspace,
                    env={"PATH": "/usr/bin:/bin", "HOME": workspace})
            except subprocess.TimeoutExpired as exc:
                return SandboxResult(returncode=124, stdout=_capped(exc.stdout or b""),
                                     stderr=_capped(exc.stderr or b""), timed_out=True)
            return SandboxResult(returncode=completed.returncode,
                                 stdout=_capped(completed.stdout),
                                 stderr=_capped(completed.stderr), timed_out=False)


@dataclass(frozen=True)
class ToolCallResult:
    operation_id: UUID
    status: OperationStatus
    result_ref: str | None = None
    error_code: str | None = None


class ToolCall(DomainModel):
    """Validated tool arguments; credentials are rejected, never forwarded.

    Unknown keys are ignored so node snapshots (which always carry input
    context alongside mapped arguments) pass through; the secret scan above
    still inspects every key.
    """

    model_config = ConfigDict(extra="ignore")

    args: list[str] = []
    path: str | None = None


class ToolGateway:
    """Policy boundary in front of sandboxed tool execution."""

    def __init__(self, store, registry: CapabilityRegistry,
                 artifacts: ArtifactStore, backend: SandboxBackend) -> None:
        self.store = store
        self.registry = registry
        self.artifacts = artifacts
        self.backend = backend

    def _build_argv(self, tool_ref: str, call: ToolCall) -> list[str]:
        if tool_ref == "echo":
            return ["/bin/echo", *call.args]
        if tool_ref == "fs.read":
            if not call.path or not call.path.startswith("/") or "/../" in call.path:
                raise ToolDenied("invalid_path", "fs.read requires an absolute path without '..'")
            return ["/bin/cat", call.path]
        raise ToolDenied("unknown_tool", f"no executable mapping for tool: {tool_ref}")

    def execute(self, lease, *, agent_ref: str, tool_ref: str,
                arguments: dict, operation_id: UUID,
                timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS) -> ToolCallResult:
        try:
            tool = self.registry.tool(tool_ref)
        except CapabilityRegistryError:
            raise ToolDenied("unknown_tool", f"tool is not registered: {tool_ref}") from None
        try:
            agent = self.registry.agent(agent_ref)
        except CapabilityRegistryError:
            raise ToolDenied("unknown_agent", f"agent is not registered: {agent_ref}") from None
        if tool_ref not in agent.tool_refs:
            raise ToolDenied("out_of_scope", f"agent {agent_ref} may not use tool {tool_ref}")
        if tool.side_effect:
            raise ToolDenied("approval_required",
                             f"side-effect tool {tool_ref} needs tool-level approval (pending)")
        for key in arguments:
            lowered = key.lower()
            if any(hint in lowered for hint in _SECRET_KEY_HINTS):
                raise ToolDenied("credential_in_arguments",
                                 "credentials must use the secret mechanism, never arguments")
        if not isinstance(arguments, dict):
            raise ToolDenied("invalid_arguments", "tool arguments must be an object")
        try:
            call = ToolCall.model_validate(arguments)
        except ValueError as exc:
            raise ToolDenied("invalid_arguments", f"tool arguments rejected: {exc}") from None
        argv = self._build_argv(tool_ref, call)

        operation = ToolOperation.register(
            operation_id=operation_id, claim_id=lease.claim_id,
            node_run_id=lease.node_run_id, run_id=lease.run_id,
            tool_ref=tool_ref, arguments=arguments)
        try:
            stored = self.store.register_tool_operation(operation)
        except OperationConflict:
            # Same identity, rebuilt arguments (e.g. fresh timestamps after
            # a crash): return the persisted outcome instead of re-executing.
            for record in self.store.list_tool_operations(lease.run_id):
                if record.operation_id == operation_id:
                    stored = record
                    break
            else:
                raise
        if stored.status is not OperationStatus.REGISTERED:
            return ToolCallResult(operation_id=operation_id, status=stored.status,
                                  result_ref=stored.result_ref, error_code=stored.error_code)
        self.store.start_tool_operation(operation_id, lease.claim_id)
        result = self.backend.run(argv, timeout_seconds=timeout_seconds)
        if result.timed_out:
            finished = self.store.finish_tool_operation(
                operation_id, lease.claim_id, status=OperationStatus.FAILED,
                error_code="timeout")
            return ToolCallResult(operation_id=operation_id, status=finished.status,
                                  error_code=finished.error_code)
        if result.returncode != 0:
            finished = self.store.finish_tool_operation(
                operation_id, lease.claim_id, status=OperationStatus.FAILED,
                error_code=f"exit_{result.returncode}")
            logger.warning("tool %s exited %s: %s", tool_ref, result.returncode,
                           result.stderr.decode("utf-8", "replace")[-500:])
            return ToolCallResult(operation_id=operation_id, status=finished.status,
                                  error_code=finished.error_code)
        text = result.stdout.decode("utf-8", "replace")
        ref = self.artifacts.put_text(text, media_type="text/plain")
        finished = self.store.finish_tool_operation(
            operation_id, lease.claim_id, status=OperationStatus.SUCCEEDED,
            result_ref=ref)
        return ToolCallResult(operation_id=operation_id, status=finished.status,
                              result_ref=finished.result_ref)
