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
import json
import shutil
import subprocess
import tempfile
from dataclasses import dataclass
from typing import Protocol
from uuid import UUID

from pydantic import ConfigDict

from anchor.domain.models import DomainModel
from anchor.domain.operations import OperationStatus, ToolOperation
from anchor.runtime.artifacts import ArtifactStore
from anchor.state.errors import OperationConflict
from anchor.runtime.capabilities import CapabilityRegistry, CapabilityRegistryError
from anchor.runtime.research_tools import (
    RESEARCH_TOOLS, ResearchRequest, ResearchToolError, execute_research,
)


logger = logging.getLogger("anchor.tool_gateway")

OUTPUT_LIMIT = 1_000_000
DEFAULT_TIMEOUT_SECONDS = 30.0
HTTP_TOOLS = frozenset({"http.post"})
_HTTP_BODY_LIMIT = 2000
WORKSPACE_MOUNT = "/tmp"
# NOTE: bubblewrap creates mount points top-down, so the workspace reuses the
# existing /tmp (mounted over read-only /) instead of a fresh directory.

_SECRET_KEY_HINTS = ("secret", "password", "token", "api_key", "apikey", "private_key")


class ToolDenied(RuntimeError):
    """Raised when policy refuses a tool call. Never executes anything."""

    def __init__(self, code: str, message: str) -> None:
        self.code = code
        super().__init__(f"{code}: {message}")


class HttpOutcomeUnknown(RuntimeError):
    """The request may have reached the remote; the side effect is unknown.

    Timeouts and transport failures after the request was sent cannot prove the
    remote did not act, so the operation becomes `outcome_unknown` and requires
    operator reconciliation instead of an automatic retry.
    """


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
                "--setenv", "PATH", "/usr/bin:/bin",
                "--setenv", "HOME", WORKSPACE_MOUNT,
                "--", *argv,
            ]
            try:
                completed = subprocess.run(
                    cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    timeout=timeout_seconds, check=False, env={})
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
    url: str | None = None
    body: dict = {}


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

    @staticmethod
    def _validate_http(call: ToolCall, *, allow_private: bool) -> None:
        from urllib.parse import urlsplit
        if not call.url or urlsplit(call.url).scheme not in ("http", "https"):
            raise ToolDenied("invalid_url", "http.post requires an http(s) url")
        host = urlsplit(call.url).hostname or ""
        if not host:
            raise ToolDenied("invalid_url", "http.post url has no host")
        if allow_private:
            return
        import ipaddress
        import socket
        try:
            addresses = {item[4][0] for item in socket.getaddrinfo(host, None)}
        except OSError:
            raise ToolDenied("unresolved_host", f"http.post cannot resolve {host}") from None
        for address in addresses:
            ip = ipaddress.ip_address(address)
            if ip.is_private or ip.is_loopback or ip.is_link_local or ip.is_reserved:
                raise ToolDenied("private_network",
                                 f"http.post refuses private/loopback host {host}; "
                                 "set allow_private_network only for a trusted internal endpoint")

    def _execute_http(self, call: ToolCall, *, timeout_seconds: float):
        import httpx2
        try:
            return httpx2.post(call.url, json=call.body or {}, timeout=timeout_seconds,
                               follow_redirects=False, trust_env=False)
        except (httpx2.TimeoutException, httpx2.TransportError) as exc:
            raise HttpOutcomeUnknown(str(exc)) from exc

    def execute(self, lease, *, agent_ref: str, tool_ref: str,
                arguments: dict, operation_id: UUID,
                timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
                approved: bool = False) -> ToolCallResult:
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
        if tool.side_effect and not approved:
            raise ToolDenied("approval_required",
                             f"side-effect tool {tool_ref} requires an explicit approval gate")
        if not isinstance(arguments, dict):
            raise ToolDenied("invalid_arguments", "tool arguments must be an object")
        for key in arguments:
            lowered = key.lower()
            if any(hint in lowered for hint in _SECRET_KEY_HINTS):
                raise ToolDenied("credential_in_arguments",
                                 "credentials must use the secret mechanism, never arguments")
        try:
            call = (ResearchRequest.model_validate(arguments) if tool_ref in RESEARCH_TOOLS
                    else ToolCall.model_validate(arguments))
        except ValueError as exc:
            raise ToolDenied("invalid_arguments", f"tool arguments rejected: {exc}") from None
        if tool_ref in HTTP_TOOLS:
            self._validate_http(call, allow_private=tool.allow_private_network)
        argv = None if tool_ref in RESEARCH_TOOLS or tool_ref in HTTP_TOOLS else self._build_argv(tool_ref, call)

        operation = ToolOperation.register(
            operation_id=operation_id, claim_id=lease.claim_id,
            node_run_id=lease.node_run_id, run_id=lease.run_id,
            tool_ref=tool_ref, arguments=arguments)
        # A model retry gets a new NodeRun/lease, but an identical successful
        # read operation in the same Run is already durable evidence. Reuse
        # it before registering a second network request. Failed reads are
        # intentionally not reused so transient 429/5xx errors can recover.
        prior_node_ids = {str(node.id): node.node_id for node in self.store.list_node_runs(lease.run_id)}
        for prior in self.store.list_tool_operations(lease.run_id):
            if (prior_node_ids.get(str(prior.node_run_id)) == lease.node_id
                    and prior.tool_ref == tool_ref
                    and prior.request_hash == operation.request_hash
                    and prior.status is OperationStatus.SUCCEEDED):
                return ToolCallResult(operation_id=operation_id, status=prior.status,
                                      result_ref=prior.result_ref, error_code=prior.error_code)
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
        if tool_ref in HTTP_TOOLS:
            try:
                response = self._execute_http(call, timeout_seconds=timeout_seconds)
            except HttpOutcomeUnknown as exc:
                finished = self.store.finish_tool_operation(
                    operation_id, lease.claim_id, status=OperationStatus.OUTCOME_UNKNOWN,
                    error_code="transport_unknown")
                logger.warning("tool %s outcome unknown [transport_unknown]: %s", tool_ref, exc)
                return ToolCallResult(operation_id=operation_id, status=finished.status,
                                      error_code=finished.error_code)
            body = json.dumps({"status": response.status_code,
                               "body": response.text[:_HTTP_BODY_LIMIT]})
            ref = self.artifacts.put_text(body, media_type="application/json")
            if 200 <= response.status_code < 300:
                finished = self.store.finish_tool_operation(
                    operation_id, lease.claim_id, status=OperationStatus.SUCCEEDED,
                    result_ref=ref)
            else:
                finished = self.store.finish_tool_operation(
                    operation_id, lease.claim_id, status=OperationStatus.FAILED,
                    result_ref=ref, error_code=f"http_{response.status_code}")
            return ToolCallResult(operation_id=operation_id, status=finished.status,
                                  result_ref=finished.result_ref, error_code=finished.error_code)
        if tool_ref in RESEARCH_TOOLS:
            try:
                text = execute_research(tool_ref, call, timeout_seconds=timeout_seconds)
                result = SandboxResult(0, text.encode("utf-8"), b"", False)
            except ResearchToolError as exc:
                failure_ref = self.artifacts.put_text(json.dumps({
                    "error": str(exc), "error_code": exc.code,
                    "retryable": exc.retryable, "tool": tool_ref,
                    "evidence_available": False,
                }), media_type="application/json")
                finished = self.store.finish_tool_operation(
                    operation_id, lease.claim_id, status=OperationStatus.FAILED,
                    result_ref=failure_ref, error_code=exc.code)
                logger.warning("tool %s failed [%s]: %s", tool_ref, exc.code, exc)
                return ToolCallResult(operation_id=operation_id, status=finished.status,
                                      result_ref=finished.result_ref,
                                      error_code=finished.error_code)
            except Exception as exc:  # noqa: BLE001 - a tool boundary must turn any
                # failure into a structured result instead of crashing the worker.
                failure_ref = self.artifacts.put_text(json.dumps({
                    "error": str(exc), "error_code": "retrieval_failed",
                    "retryable": False, "tool": tool_ref,
                    "evidence_available": False,
                }), media_type="application/json")
                finished = self.store.finish_tool_operation(
                    operation_id, lease.claim_id, status=OperationStatus.FAILED,
                    result_ref=failure_ref, error_code="retrieval_failed")
                logger.warning("tool %s failed [retrieval_failed]: %s", tool_ref, exc)
                return ToolCallResult(operation_id=operation_id, status=finished.status,
                                      result_ref=finished.result_ref,
                                      error_code=finished.error_code)
        else:
            result = self.backend.run(argv, timeout_seconds=timeout_seconds)
        if result.timed_out:
            finished = self.store.finish_tool_operation(
                operation_id, lease.claim_id, status=OperationStatus.FAILED,
                error_code="timeout")
            return ToolCallResult(operation_id=operation_id, status=finished.status,
                                  error_code=finished.error_code)
        if result.returncode != 0:
            failure_ref = None
            if tool_ref in RESEARCH_TOOLS:
                failure_ref = self.artifacts.put_text(json.dumps({
                    "error": result.stderr.decode("utf-8", "replace")[-1000:],
                    "tool": tool_ref, "evidence_available": False,
                }), media_type="application/json")
            finished = self.store.finish_tool_operation(
                operation_id, lease.claim_id, status=OperationStatus.FAILED,
                result_ref=failure_ref, error_code=f"exit_{result.returncode}")
            logger.warning("tool %s exited %s: %s", tool_ref, result.returncode,
                           result.stderr.decode("utf-8", "replace")[-500:])
            return ToolCallResult(operation_id=operation_id, status=finished.status,
                                  result_ref=finished.result_ref, error_code=finished.error_code)
        text = result.stdout.decode("utf-8", "replace")
        ref = self.artifacts.put_text(
            text, media_type="application/json" if tool_ref in RESEARCH_TOOLS else "text/plain")
        finished = self.store.finish_tool_operation(
            operation_id, lease.claim_id, status=OperationStatus.SUCCEEDED,
            result_ref=ref)
        return ToolCallResult(operation_id=operation_id, status=finished.status,
                              result_ref=finished.result_ref)
