"""Workspace-confined execution.

A sandbox runs one command with the node's directory bound read-write and the named parts of the
system bound read-only. That is the whole boundary: no network unless the node's agent asked for it,
no path out of the bound tree, no privileged operation.

The command is what the loop runs through a shell — `sh -c "<string>"` — so the allowlist gates the
*entry point*, not the programs a node may name inside it. That is deliberate: an allowlist in front
of a shell is a second, weaker boundary that the shell steps around, and the confinement that holds
is the bound tree and the namespace.

Whether the sandbox can isolate is checked when it is constructed, not assumed from the binary being
installed: a `bwrap` that cannot create a namespace is refused rather than trusted, because otherwise
every command fails and the node spends its whole budget finding out. ADR-034.
"""

from __future__ import annotations

import hashlib

import shutil
import subprocess
import tempfile
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
    #: Where to put the full output before `max_output_bytes` cuts it. **The bytes are in hand at the
    #: moment the cut happens and are gone immediately after**, so this is the only place a caller can
    #: ask for them. A path rather than a return value because a command may print a great deal, and
    #: holding it all in memory to hand it back would be the same problem one layer up.
    spill_dir: Path | None = None
    #: How many bytes the caller is still willing to have written. A spill that ignores this is a bound
    #: that does not bound: the caller's own accounting said one thing and the disk did another.
    spill_limit_bytes: int | None = None
    #: Where to make the spill visible to the command, as a path **inside** the sandbox. The host path
    #: is not reachable from in there — `/tmp` is a private tmpfs — so a model told the host path is
    #: told about a file it cannot open, and a test that only checks the host copy passes falsely.
    spill_mount: str | None = None
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
    # Paths inside the workspace bound read-only *after* it is bound read-write, so a node can read
    # them and not rewrite them. Its own repository is the one that matters: the history of its work
    # is the record of it, and a record the recorded thing can edit is not one.
    workspace_readonly: tuple[str, ...] = ()
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
    #: Where the *untruncated* output was put, when the spec asked for it. Empty otherwise, and empty
    #: when there was nothing to spill or nowhere to put it — a caller that needs the whole of a large
    #: output has to check rather than assume.
    spilled: tuple[Path, ...] = ()
    #: The same files as the command can reach them, when a mount was named.
    visible: tuple[str, ...] = ()
    #: True when the output was cut and **could not be kept whole** — the store's bound was reached,
    #: or there was nowhere to put it. Said out loud because the alternative is a caller that believes
    #: it holds a complete output, which is the failure this whole path exists to avoid.
    incomplete: bool = False

    @property
    def complete(self) -> bool:
        """Whether `stdout`/`stderr` are everything the command produced.

        True when nothing was cut, and also true when what was cut is somewhere else. The distinction
        that matters to a caller is "can I get the rest", not "was it cut".
        """
        return bool(self.spilled) or "[truncated:" not in self.stdout + self.stderr

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


def _decode(data: bytes, limit: int, visible: str | None = None) -> str:
    """The output as far as the model may see it, and **where the rest is when there is a rest**.

    A truncation notice that does not say where the rest went is a dead end: the model is told its
    output was cut and given no way to read the rest, which is worse than not cutting it. And the path
    it is given is the one **it** can open — the sandbox's view, not the host's, which is a private
    tmpfs away and reads as a missing file.
    """
    text = data[:limit].decode("utf-8", errors="replace")
    if len(data) > limit:
        where = f" the whole output is at {visible}" if visible else " and was not kept"
        text += f"\n[truncated:{len(data) - limit}-bytes;{where}]"
    return text


def _spill(spec: SandboxSpec, stdout: bytes, stderr: bytes) -> tuple[tuple[Path, ...], int]:
    """Put the whole of a command's output where a caller can read it, before it is cut.

    Returns what was written **and how much of what was cut could not be written**. The second number
    is the one that matters: a caller that is told "the rest is at <path>" when the rest is partly
    missing has been told something false, and it will act on it.

    Only when the spec asked and only when something would actually be lost — a file per command with
    nothing in it is a directory that fills up for no reason. The name is a digest of the content, so
    the same output twice is one file and a caller can tell whether two commands produced the same
    thing.
    """
    if spec.spill_dir is None:
        return (), 0
    written: list[Path] = []
    lost = 0
    # **One budget for both streams.** Each stream used to be checked against the caller's full
    # remaining allowance, so two streams of 1,100,000 bytes against a 50,000-byte bound wrote two
    # 50,000-byte files — a hundred thousand bytes under a bound of fifty.
    remaining = spec.spill_limit_bytes
    for stream, data in (("stdout", stdout), ("stderr", stderr)):
        if len(data) <= spec.max_output_bytes:
            continue
        if remaining is not None:
            kept_bytes = min(len(data), max(remaining, 0))
            lost += len(data) - kept_bytes
            remaining -= kept_bytes
            data = data[:kept_bytes]
            if not data:
                continue
        digest = hashlib.sha256(data).hexdigest()[:16]
        path = Path(spec.spill_dir) / f"{stream}-{digest}.txt"
        if not path.exists():
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        written.append(path)
    return tuple(written), lost


def _probe(binary: str, timeout_seconds: float = 10.0) -> None:
    """Confirm the sandbox can create a namespace, not merely that its binary is installed.

    Presence is not capability. Under a container whose AppArmor profile denies namespace creation,
    `bwrap` is on PATH with the right version and every command still fails with `No permissions to
    create new namespace`. Checking only for the binary let that through, and the first thing to
    notice was a node: it retried, produced a directory of nothing, and spent its whole step budget
    and real money finding out — 60 steps and two attempts in one measured run.

    Refusing here keeps ADR-034's promise: a sandbox that cannot isolate is unavailable, and
    unavailable is refused rather than degraded to something unisolated.
    """
    argv = [binary, "--unshare-all", "--ro-bind", "/", "/", "--", "true"]
    try:
        completed = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   timeout=timeout_seconds, check=False, env={})
    except (OSError, subprocess.SubprocessError) as exc:
        raise RuntimeError(f"sandbox binary {binary} could not be run: {exc}") from exc
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", errors="replace").strip() or "no stderr"
        raise RuntimeError(f"sandbox binary {binary} cannot create a namespace: {detail}")


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
        _probe(binary)
        # Whether this host will mount a fresh procfs inside the sandbox. Measured rather than
        # assumed, because a host can deny it while allowing everything else: a user namespace may
        # create a mount, and the kernel still refuses a procfs mount there — and then the whole
        # sandbox fails to start over a mount that only adds convenience. That procfs would belong to
        # this sandbox's own pid namespace, showing the node its own processes and nothing else, so
        # leaving it out costs functionality and no isolation at all.
        self._proc = self._can_mount_proc()

    def _argv(self, spec: SandboxSpec) -> list[str]:
        """The bubblewrap command line for one execution.

        One place, so the probe below cannot certify a sandbox that differs from the one that runs.
        """
        return [
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
            *(("--proc", "/proc") if self._proc else ()),
            "--dir", SANDBOX_WORKSPACE,
            # Read-write, unlike the rest of the tree, because a node that can only read cannot
            # write a paper. The isolation is unchanged: the bind is the one path it may mutate,
            # and it is the node's own workspace.
            "--bind", str(spec.workspace), SANDBOX_WORKSPACE,
            # After the workspace, so these win. The other order would have the read-write bind
            # covering them again and the boundary would be a comment.
            *(item for relative in spec.workspace_readonly
              for item in ("--ro-bind", str(spec.workspace / relative),
                           f"{SANDBOX_WORKSPACE}/{relative}")),
            "--chdir", SANDBOX_WORKSPACE,
            "--dev", "/dev",
            "--setenv", "PATH", ":".join([*spec.tool_dirs, "/usr/bin:/bin"]),
            "--setenv", "HOME", SANDBOX_WORKSPACE,
            "--setenv", "TMPDIR", "/tmp",
            "--setenv", "PYTHONDONTWRITEBYTECODE", "1",
            *(item for key, value in spec.env for item in ("--setenv", key, value)),
            "--", *spec.command,
        ]

    def _can_mount_proc(self) -> bool:
        """Run the real command line once with `/proc`, and see whether this host allows it."""
        workspace = Path(tempfile.mkdtemp(prefix="anchor-proc-probe-"))
        self._proc = True
        try:
            argv = self._argv(SandboxSpec(workspace=workspace, command=("true",)))
            try:
                completed = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                           timeout=10.0, check=False, env={})
            except (OSError, subprocess.SubprocessError) as exc:
                raise RuntimeError(f"sandbox binary {self.binary} could not be run: {exc}") from exc
        finally:
            shutil.rmtree(workspace, ignore_errors=True)
        if completed.returncode == 0:
            return True
        detail = completed.stderr.decode("utf-8", errors="replace").strip()
        if "proc" in detail.lower():
            return False
        raise RuntimeError(f"sandbox cannot start: {detail or 'no stderr'}")

    def run(self, spec: SandboxSpec) -> SandboxResult:
        _validate(spec, self.allowed_commands)
        try:
            completed = subprocess.run(self._argv(spec), stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, timeout=spec.timeout_seconds,
                                       check=False, env={})
        except subprocess.TimeoutExpired as exc:
            return self._result(124, exc.stdout or b"", exc.stderr or b"", spec, True)
        return self._result(completed.returncode, completed.stdout, completed.stderr, spec, False)

    @staticmethod
    def _result(returncode: int, stdout: bytes, stderr: bytes, spec: SandboxSpec,
                timed_out: bool) -> SandboxResult:
        """One place that decides what a caller is told, so the two streams cannot be confused.

        Each stream cites **its own** file. Citing the first one for both is how a model is sent to
        stdout's copy to look for a stderr message that is not in it.
        """
        spilled, lost = _spill(spec, stdout, stderr)
        # What the command can actually open, per stream. The host path is a private tmpfs away from
        # the sandbox, so a model told the host path is told about a file it cannot read — and a test
        # that checks the host copy passes while the node never saw it.
        seen: dict[str, str] = {}
        for path in spilled:
            stream = path.name.split("-", 1)[0]
            seen.setdefault(stream, f"{spec.spill_mount}/{path.name}" if spec.spill_mount
                            else str(path))
        cut = len(stdout) > spec.max_output_bytes or len(stderr) > spec.max_output_bytes
        return SandboxResult(
            returncode,
            _decode(stdout, spec.max_output_bytes, seen.get("stdout")),
            _decode(stderr, spec.max_output_bytes, seen.get("stderr")),
            timed_out, spilled,
            tuple(seen[name] for name in ("stdout", "stderr") if name in seen),
            # Cut, and either nothing was kept or part of it was not. A caller that is told "the rest
            # is at <path>" when part of the rest is missing has been told something false.
            incomplete=bool(cut and (lost or not spilled)))


class SubprocessWorkspaceSandbox:
    """TEST ONLY: cwd and timeout, NO isolation.

    It exists so the sandbox *contract* is testable where bubblewrap is not available. Nothing
    selects it in a real run: a sandbox that cannot isolate is refused at construction rather than
    degraded to an unisolated subprocess. ADR-034.
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
