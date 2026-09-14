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
#: Where the node's directory is mounted inside the sandbox, and what `HOME` points at.
#:
#: Not under `/tmp`. It was `/tmp/ws` with `HOME=/tmp`, which put the workspace's parent and the
#: node's home in the same place as a tmpfs that is emptied between commands — so `cd ~` left the
#: workspace, files written there vanished, and one node spent several turns working out that it was
#: looking in the wrong directory. A path of its own, with `HOME` pointing at it, removes the
#: question entirely.
SANDBOX_WORKSPACE = "/workspace"
#: The parts of the system a shell needs to exist at all. Everything not named here — other
#: projects, the operator's home, this repository's own state — is not visible to a node.
SANDBOX_SYSTEM = ("/usr", "/bin", "/lib", "/lib64", "/sbin")
SANDBOX_FILES = (
    # Name resolution, and the certificate authorities, or a node that reaches the literature
    # cannot verify what it reaches. `/etc/ssl` covers Debian-family systems and `/etc/pki` the
    # Red Hat family; both are tried, because the node should not fail on which distribution it is.
    ("/etc/resolv.conf", "/etc/resolv.conf"), ("/etc/ssl", "/etc/ssl"), ("/etc/pki", "/etc/pki"),
    ("/etc/ca-certificates.conf", "/etc/ca-certificates.conf"), ("/etc/hosts", "/etc/hosts"),
    ("/etc/passwd", "/etc/passwd"), ("/etc/group", "/etc/group"),
    ("/etc/nsswitch.conf", "/etc/nsswitch.conf"), ("/etc/localtime", "/etc/localtime"),
)


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
    # Directories added to the sandbox's PATH, for tools the node is meant to have.
    tool_dirs: tuple[str, ...] = ()
    # ("source", "destination") pairs mounted read-only, for the tools to exist at all. At their real
    # paths, because a virtual environment's interpreter and scripts carry absolute paths and moving
    # them breaks both.
    readonly_binds: tuple[tuple[str, str], ...] = ()
    # Variables handed to the node, on top of the fixed set below. This is how a command learns
    # something only the runner knows — which node it is, and where it may route to — without the
    # runner having to write a file into the node's own directory to say so.
    env: tuple[tuple[str, str], ...] = ()


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
            # Named parts of the system, not the whole thing. Binding `/` read-only looked harmless
            # and was not: a node could read the operator's home, this repository's state, and a
            # database left over from a system that no longer exists — and one did. Its plan filled
            # with `tool_operations` and `result_ref`, vocabulary it could only have learned by
            # looking, and a research step wrote a script to query that database. A node's behaviour
            # then depends on what happens to be on the disk, which is the opposite of reproducible.
            *(item for path in SANDBOX_SYSTEM if Path(path).exists()
              for item in ("--ro-bind", path, path)),
            *(item for source, destination in SANDBOX_FILES if Path(source).exists()
              for item in ("--ro-bind-try", source, destination)),
            *(item for source, destination in spec.readonly_binds
              for item in ("--ro-bind", source, destination)),
            "--tmpfs", "/tmp",
            "--dir", SANDBOX_WORKSPACE,
            # Read-write, unlike the rest of the tree, because a node that can only read cannot
            # write a paper. The isolation is unchanged: the bind is the one path it may mutate,
            # and it is the node's own workspace.
            "--bind", str(spec.workspace), SANDBOX_WORKSPACE,
            "--chdir", SANDBOX_WORKSPACE,
            "--proc", "/proc", "--dev", "/dev",
            "--setenv", "PATH", ":".join([*spec.tool_dirs, "/usr/bin:/bin"]),
            "--setenv", "HOME", SANDBOX_WORKSPACE,
            "--setenv", "TMPDIR", "/tmp",
            "--setenv", "PYTHONDONTWRITEBYTECODE", "1",
            *(item for key, value in spec.env for item in ("--setenv", key, value)),
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
