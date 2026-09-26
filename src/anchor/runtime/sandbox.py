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
import os
import signal

import shutil
import subprocess
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Protocol

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
    cancelled: Callable[[], bool] | None = None


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


def _digest(path: Path) -> str:
    """The content's name, in chunks.

    The first version of this used the `hashlib` object itself in the filename, having called `update`
    and never `hexdigest`: the files were called `stdout-<sha256 _hashlib.HASH object @ 0x…>.txt`. Not
    content-addressed, not a legal name to hand a shell, and two different outputs could collide on a
    reused address.
    """
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()[:16]


def _head(path: Path | None, size: int, limit: int) -> bytes:
    if path is None or size == 0:
        return b""
    with path.open("rb") as handle:
        return handle.read(min(size, limit))


def _capture(spec: SandboxSpec, argv: list[str], staging: Path) -> tuple[
        dict[str, Path], dict[str, int], int, int, bool]:
    """Run the command, keeping at most `max_output_bytes` per stream and counting the rest.

    The reader does the bounding. A thread per stream takes chunks from the pipe, writes the part that
    is allowed to be kept, and adds the rest to a counter — so neither memory nor disk holds what the
    caller did not ask for, and the total size is still known exactly.
    """
    limit = spec.max_output_bytes
    # **Three numbers, and conflating two of them is what broke this.** `max_output_bytes` is how much of
    # a stream the *model* is shown; the caller's budget is how much may be *kept* in total. The preview
    # is **per stream** — each of stdout and stderr may show `limit` bytes — and only what is kept
    # *beyond* the previews comes out of the shared budget, so two large streams cannot each take the
    # whole allowance.
    #
    # Sharing the preview was the bug: with stdout and stderr each printing 8000 bytes against a 10000
    # limit and no store, one stream got 2000, the other 8000, `incomplete` was False, and 6000 bytes
    # vanished with nothing said. Both streams were under the limit; the limit was being spent twice.
    budget: int = spec.spill_limit_bytes if spec.spill_limit_bytes is not None else 0
    if spec.spill_dir is None:
        budget = 0
    staged: dict[str, Path] = {}
    sizes: dict[str, int] = {}
    written: dict[str, int] = {}
    # **Two threads share the budget, so it needs a lock.** Each stream's reader took `sum(written)` and
    # added to it without one: both could read the same total and both spend it, which is how the same
    # test passed three times and failed once. The dictionaries are small and the critical section is
    # three arithmetic operations, so the lock costs nothing worth measuring.
    lock = threading.Lock()
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               env={}, start_new_session=True)
    assert process.stdout is not None and process.stderr is not None

    def drain(name: str, pipe: Any) -> None:
        path = staging / f"{name}.kept"
        with path.open("wb") as handle:
            while True:
                chunk = pipe.read(1 << 16)
                if not chunk:
                    break
                with lock:
                    sizes[name] = sizes.get(name, 0) + len(chunk)
                    # Each stream's own preview first, then whatever the shared budget has left for
                    # keeping beyond the previews.
                    preview_room = max(limit - written.get(name, 0), 0)
                    kept_beyond = sum(max(0, count - limit) for count in written.values())
                    spill_room = max(budget - kept_beyond, 0)
                    room = preview_room + spill_room
                    piece = chunk[:room] if room > 0 else b""
                    if piece:
                        written[name] = written.get(name, 0) + len(piece)
                # Written outside the lock: the file belongs to this thread alone, and holding the lock
                # across a disk write would serialise the two streams for no reason.
                if piece:
                    handle.write(piece)
        staged[name] = path

    threads = [threading.Thread(target=drain, args=(name, pipe), daemon=True)
               for name, pipe in (("stdout", process.stdout), ("stderr", process.stderr))]
    for thread in threads:
        thread.start()
    timed_out = False
    deadline = time.monotonic() + spec.timeout_seconds
    while True:
        if spec.cancelled is not None and spec.cancelled() and process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            returncode = 130 if process.returncode == -signal.SIGKILL else process.returncode
            break
        try:
            returncode = process.wait(timeout=min(0.1, max(0, deadline - time.monotonic())))
            break
        except subprocess.TimeoutExpired:
            if time.monotonic() >= deadline:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
                returncode, timed_out = 124, True
                break
    # **Waited for, without closing anything and without a short timeout.** A reader ends when its pipe
    # reaches end of file, which happens when the process is gone; closing the pipe instead discards
    # whatever is still buffered in it — and that was measured, as a result whose stdout was empty, a
    # digest that differed between identical runs, and a spill that never happened. All three were the
    # same mistake: the data was thrown away on the way out.
    for thread in threads:
        thread.join()
    lost = sum(sizes.values()) - sum(written.values())
    return staged, {name: sizes.get(name, 0) for name in ("stdout", "stderr")}, lost, returncode, \
        timed_out


def _decode(data: bytes, size: int, limit: int, visible: str | None = None, *,
            complete: bool = True) -> str:
    """The output as far as the model may see it, and the truth about the rest.

    `size` is the whole length, which the file's own `stat` answered without reading it. The notice
    distinguishes three cases that used to be one: nothing was cut; the rest is somewhere the model can
    open; and **the rest was cut and could not be kept whole** — where saying "the whole output is at
    <path>" sends the model to a file missing part of what it is looking for.
    """
    text = data.decode("utf-8", errors="replace")
    if size <= limit:
        return text
    gone = size - limit
    if not complete:
        rest = "the rest was cut and could NOT be kept whole — this is all there is"
    elif visible:
        rest = f"the whole output is at {visible}"
    else:
        rest = "the rest was not kept"
    return f"{text}\n[truncated:{gone}-bytes; {rest}]"


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
            "--tmpfs", "/tmp",
            *(item for source, destination in spec.readonly_binds
              for item in ("--ro-bind", source, destination)),
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
        """Run one command, keeping at most the bytes the caller allowed — on disk and in memory.

        **Bounded as it is produced, not afterwards.** The first version wrote the command's whole
        output to a file and truncated it once the process had finished: memory was fine, but the disk
        was not, because a command that printed ten gigabytes filled ten gigabytes before anything
        looked at the number. A reader per stream keeps the head it was asked for and counts the rest
        without storing it, so the bound holds during the command and not only after it.

        Nothing is left behind either. The staged files live in a directory of their own, and only what
        is kept is moved out under its content's name — so a stream that fit is not left lying in the
        mounted directory unaccounted for, to be overwritten by the next command.
        """
        _validate(spec, self.allowed_commands)
        staging = Path(tempfile.mkdtemp(prefix="anchor-out-"))
        try:
            staged, sizes, lost, returncode, timed_out = _capture(spec, self._argv(spec), staging)
            return self._finish(spec, staged, sizes, lost, returncode, timed_out)
        finally:
            shutil.rmtree(staging, ignore_errors=True)

    @staticmethod
    def _finish(spec: SandboxSpec, staged: dict[str, Path], sizes: dict[str, int], lost: int,
                returncode: int, timed_out: bool) -> SandboxResult:
        """Move what is kept into place and describe it truthfully."""
        # **Read before moving.** The staged file is renamed into place below, and reading the head
        # afterwards looked for it where it no longer was.
        heads = {name: _head(staged.get(name), sizes[name], spec.max_output_bytes)
                 for name in ("stdout", "stderr")}
        seen: dict[str, str] = {}
        spilled: list[Path] = []
        if spec.spill_dir is not None:
            target = Path(spec.spill_dir)
            target.mkdir(parents=True, exist_ok=True)
            for stream, source in staged.items():
                # **Only what was cut is worth keeping.** A stream that fits is already shown to the
                # caller in full, so a file for it is a file nothing reads — and the staging is removed
                # either way, which is what stops raw leftovers sitting in the mounted directory.
                if sizes[stream] <= spec.max_output_bytes:
                    continue
                # **And the file is cut to the caller's budget**, which is a different number from what
                # is shown: the staging holds the head the model is shown, and the store holds as much
                # of the rest as the caller said it would pay for. Keeping the staging's length here is
                # what wrote a megabyte against a fifty-thousand-byte bound.
                keep_bytes = min(sizes[stream], spec.spill_limit_bytes or 0)
                if keep_bytes < source.stat().st_size:
                    with source.open("r+b") as handle:
                        handle.truncate(keep_bytes)
                if source.stat().st_size == 0:
                    continue
                name = f"{stream}-{_digest(source)}.txt"
                destination = target / name
                if not destination.exists():
                    source.replace(destination)
                spilled.append(destination)
                seen[stream] = (f"{spec.spill_mount}/{name}" if spec.spill_mount
                                else str(destination))
        cut = {name: sizes[name] > spec.max_output_bytes for name in sizes}
        return SandboxResult(
            returncode,
            _decode(heads["stdout"], sizes["stdout"], spec.max_output_bytes, seen.get("stdout"),
                    complete=not cut["stdout"] or ("stdout" in seen and not lost)),
            _decode(heads["stderr"], sizes["stderr"], spec.max_output_bytes, seen.get("stderr"),
                    complete=not cut["stderr"] or ("stderr" in seen and not lost)),
            timed_out, tuple(spilled),
            tuple(seen[name] for name in ("stdout", "stderr") if name in seen),
            incomplete=bool(any(cut.values()) and (lost or not seen)))


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
        """The same contract as the real one, through the same capture and the same wording.

        It is the contract's test double, so it has to answer the same questions — including what it
        says about output that was cut and could not be kept whole.
        """
        _validate(spec, self.allowed_commands)
        staging = Path(tempfile.mkdtemp(prefix="anchor-out-"))
        try:
            staged, sizes, lost, code, timed_out = _capture(
                spec, list(spec.command), staging)
            return BubblewrapWorkspaceSandbox._finish(spec, staged, sizes, lost, code, timed_out)
        finally:
            shutil.rmtree(staging, ignore_errors=True)
