"""Read-only workspace execution.

A sandbox runs an allowlisted command against a materialized, immutable revision
with no network and a read-only workspace. It cannot write to the tree, which is
what makes "run a command at revision R" a repeatable observation rather than a
mutation. Commands are an explicit allowlist, never a shell string.
"""

from __future__ import annotations

import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

# Conservative by default: reading and inspecting, not arbitrary execution.
# A caller may widen this, but the sandbox is read-only and has no network.
DEFAULT_ALLOWED_COMMANDS = frozenset({
    "cat", "cut", "file", "find", "git", "grep", "head", "ls", "sort",
    "tail", "uniq", "wc",
})
DEFAULT_TIMEOUT_SECONDS = 30.0
DEFAULT_MAX_OUTPUT_BYTES = 1_000_000
SANDBOX_WORKSPACE = "/tmp/ws"


class SandboxDenied(RuntimeError):
    """Policy refused the command before anything executed."""

    def __init__(self, code: str, message: str) -> None:
        self.code = code
        super().__init__(f"{code}: {message}")


@dataclass(frozen=True)
class SandboxSpec:
    workspace: Path
    command: tuple[str, ...]
    timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS
    max_output_bytes: int = DEFAULT_MAX_OUTPUT_BYTES


@dataclass(frozen=True)
class SandboxResult:
    returncode: int
    stdout: str
    stderr: str
    timed_out: bool

    @property
    def ok(self) -> bool:
        return self.returncode == 0 and not self.timed_out


class WorkspaceSandbox(Protocol):
    name: str

    def run(self, spec: SandboxSpec) -> SandboxResult: ...


def _validate(spec: SandboxSpec, allowed: frozenset[str]) -> None:
    if not spec.command:
        raise SandboxDenied("empty_command", "a command is required")
    program = Path(spec.command[0]).name
    if program not in allowed:
        raise SandboxDenied("command_not_allowed",
                            f"{program!r} is not in the allowlist {sorted(allowed)}")
    if not spec.workspace.is_dir():
        raise SandboxDenied("workspace_missing", f"workspace is not a directory: {spec.workspace}")
    if spec.timeout_seconds <= 0:
        raise SandboxDenied("invalid_timeout", "timeout_seconds must be positive")
    for argument in spec.command:
        if "\x00" in argument:
            raise SandboxDenied("invalid_argument", "command arguments must not contain NUL")


def _decode(data: bytes, limit: int) -> str:
    text = data[:limit].decode("utf-8", errors="replace")
    if len(data) > limit:
        text += f"\n[truncated:{len(data) - limit}-bytes]"
    return text


class BubblewrapWorkspaceSandbox:
    """Unprivileged isolation: no network, read-only workspace, no writes."""

    name = "bubblewrap"

    def __init__(self, *, allowed_commands: frozenset[str] = DEFAULT_ALLOWED_COMMANDS,
                 binary: str = "bwrap") -> None:
        if shutil.which(binary) is None:
            raise RuntimeError(f"sandbox binary not found: {binary}")
        self.binary = binary
        self.allowed_commands = allowed_commands

    def run(self, spec: SandboxSpec) -> SandboxResult:
        _validate(spec, self.allowed_commands)
        argv = [
            self.binary, "--unshare-all", "--die-with-parent",
            "--ro-bind", "/", "/",
            "--tmpfs", "/tmp",
            "--dir", SANDBOX_WORKSPACE,
            "--ro-bind", str(spec.workspace), SANDBOX_WORKSPACE,
            "--chdir", SANDBOX_WORKSPACE,
            "--proc", "/proc", "--dev", "/dev",
            "--setenv", "PATH", "/usr/bin:/bin",
            "--setenv", "HOME", "/tmp",
            "--setenv", "TMPDIR", "/tmp",
            "--", *spec.command,
        ]
        try:
            completed = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       timeout=spec.timeout_seconds, check=False, env={})
        except subprocess.TimeoutExpired as exc:
            return SandboxResult(124, _decode(exc.stdout or b"", spec.max_output_bytes),
                                 _decode(exc.stderr or b"", spec.max_output_bytes), True)
        return SandboxResult(completed.returncode,
                             _decode(completed.stdout, spec.max_output_bytes),
                             _decode(completed.stderr, spec.max_output_bytes), False)


class SubprocessWorkspaceSandbox:
    """Development fallback: cwd and timeout only, NO isolation.

    Never use it for untrusted commands. It exists so the sandbox contract is
    testable where bubblewrap is unavailable.
    """

    name = "subprocess"

    def __init__(self, *, allowed_commands: frozenset[str] = DEFAULT_ALLOWED_COMMANDS) -> None:
        self.allowed_commands = allowed_commands

    def run(self, spec: SandboxSpec) -> SandboxResult:
        _validate(spec, self.allowed_commands)
        try:
            completed = subprocess.run(list(spec.command), cwd=spec.workspace,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       timeout=spec.timeout_seconds, check=False, env={})
        except subprocess.TimeoutExpired as exc:
            return SandboxResult(124, _decode(exc.stdout or b"", spec.max_output_bytes),
                                 _decode(exc.stderr or b"", spec.max_output_bytes), True)
        return SandboxResult(completed.returncode,
                             _decode(completed.stdout, spec.max_output_bytes),
                             _decode(completed.stderr, spec.max_output_bytes), False)
