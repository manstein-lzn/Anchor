"""Workspace-confined execution, with no network.

A sandbox runs an allowlisted command with its workspace bound read-write and everything else bound
read-only. The confinement is the point: no network, no path out of the bound tree, no privileged
operation. Commands are an explicit allowlist, never a shell string.

Whether the bound tree is ephemeral or a node's own workspace is the caller's decision, and the two
mean different things. ``execute_in_workspace`` materializes a pinned revision into a temporary
directory, so a command there is still an observation — it cannot mutate a revision and the tree is
gone afterwards. Binding a node's live worktree makes writes persist, which is what ADR-054 gives a
node so it can produce a paper incrementally instead of in one response.
"""

from __future__ import annotations

import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

# Reading, inspecting and running a script. With no network and everything outside the bound
# workspace read-only, an interpreter can compute over the tree it is given and reach nothing else.
# A caller may narrow this further.
DEFAULT_ALLOWED_COMMANDS = frozenset({
    "cat", "cut", "file", "find", "git", "grep", "head", "ls", "python3",
    "sort", "tail", "uniq", "wc",
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
    # A node that must reach the literature needs the network; a node that only writes does not, and
    # refusing it costs nothing. Per node, because the two kinds of work are not the same kind of
    # risk and a single global answer would be the wrong one for half of them.
    network: bool = False
    # Directories added to the sandbox's PATH, for tools the node is meant to have. The whole
    # filesystem is already bound read-only, so a directory here is reachable either way; what it
    # changes is whether a command can be found by name, which is the only way a shell can use it.
    tool_dirs: tuple[str, ...] = ()


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
    """Unprivileged isolation: no network, and writes confined to the workspace.

    The workspace is bound read-write and everything else read-only, so a node may do anything to
    its own tree and nothing to anything else. That is the whole boundary: no network, no path out
    of the workspace, no privileged operation. ADR-054.
    """

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
            # `--unshare-all` takes the network away, and `--share-net` gives it back for the nodes
            # whose work is reaching the literature. Nothing else is shared either way.
            *(["--share-net"] if spec.network else []),
            "--ro-bind", "/", "/",
            "--tmpfs", "/tmp",
            "--dir", SANDBOX_WORKSPACE,
            # Read-write, unlike the rest of the tree, because a node that can only read cannot
            # write a paper. The isolation is unchanged: the bind is the one path it may mutate,
            # and it is the node's own workspace.
            "--bind", str(spec.workspace), SANDBOX_WORKSPACE,
            "--chdir", SANDBOX_WORKSPACE,
            "--proc", "/proc", "--dev", "/dev",
            "--setenv", "PATH", ":".join([*spec.tool_dirs, "/usr/bin:/bin"]),
            "--setenv", "HOME", "/tmp",
            "--setenv", "TMPDIR", "/tmp",
            "--setenv", "PYTHONDONTWRITEBYTECODE", "1",
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
    """TEST ONLY: cwd and timeout, NO isolation.

    It exists so the sandbox *contract* is testable where bubblewrap is not
    available. Production code never selects it: `worker_service` disables
    workspace.exec instead of degrading to an unisolated subprocess.
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
