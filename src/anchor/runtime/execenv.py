"""What a node's sandbox is wired with, said without naming a framework.

`_console_script`, `_tool_binds` and the working probe used to live in `simple/agent.py`, which
imports mini-swe-agent at module level. Anything wanting the same sandbox therefore inherited that
import — and the point of a second node runner is to not have one. They are here so that **one** place
decides a node's mounts, its tool directories, and what proves the sandbox works, and two runners use
it rather than agreeing by convention.

Nothing here knows what runs a node. It builds a `SandboxSpec` and hands back what came out.
"""

from __future__ import annotations

import os
import shutil
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox, SandboxSpec

#: The commands a node's shell may be. The command arrives as one shell string, so the shell is the
#: entry point and the sandbox is the boundary. An allowlist of commands would be a second, weaker
#: boundary that the shell can step around anyway.
SHELL = ("sh",)

#: Where a node writes, and the two things that must not move under it. `/workspace` is its own;
#: `.git` is its record, which the sandbox may read and may not rewrite.
WORKSPACE_READONLY = (".git",)

#: Beside the workspace, never inside it. Inside it is a file the agent can read, and one did: it
#: found its own conversation and reasoned about that instead of its task.
PROBE = ".anchor-sandbox-probe"


def console_script(name: str) -> str | None:
    """Where a console script of *this* installation is.

    Next to the interpreter, not looked up on PATH. The runner is started as `python -m …`, which
    does not put its own environment's `bin` on PATH, so a lookup there finds nothing — and then the
    node is told about a tool that is not on its PATH and cannot be called.
    """
    # Not resolved. `sys.executable` is `<venv>/bin/python`, which is a symlink to whatever
    # interpreter the environment was built from — and following it walks out of the environment,
    # where the console scripts are not.
    candidate = Path(sys.executable).parent / name
    if candidate.is_file():
        return str(candidate)
    return shutil.which(name)


def tool_dirs(found: str | None) -> tuple[str, ...]:
    return (str(Path(found).parent),) if found else ()


def _package_root() -> Path:
    """`src/`, which is what has to be on the path for an installed console script to import Anchor.

    Resolved from this file rather than from `sys.path`, so it is the tree this code is in even when
    something else put a different copy first.
    """
    return Path(__file__).resolve().parents[2]


def tool_binds(found: str | None) -> tuple[tuple[str, str], ...]:
    """What has to be visible for the literature tool to run: its interpreter and its package.

    Both at their real paths. A virtual environment is not relocatable — the interpreter looks for
    its libraries relative to itself, and the console script's shebang names the interpreter — so a
    bind at some tidier location would produce a tool that cannot start.
    """
    if not found:
        return ()
    venv = Path(found).parents[1]
    package = _package_root()
    binds = [(str(venv), str(venv))]
    if package.is_dir():
        binds.append((str(package), str(package)))
    # And the Python the environment is built on, which is not inside it: `<venv>/bin/python` is a
    # symlink into an installed interpreter, and a console script's shebang names the symlink. There
    # are two links in that chain — the environment points at a stable alias like `cpython-3.12`,
    # which points at the versioned directory actually on disk — so both have to be present or the
    # script cannot start. Binding only the resolved one produced
    # `bad interpreter: No such file or directory`, which is what a missing alias looks like.
    #
    # The target is resolved against the link's own directory, and that is the whole point of this
    # loop. `os.readlink` may answer with a relative path — `<venv>/bin/python` points at `python3` —
    # and `Path("python3").parent` is `.`, so the bind became `--ro-bind . .`: bubblewrap's working
    # directory mounted over the sandbox root, making every mount after it fail. What a node saw was
    # `Can't mkdir /tmp: Read-only file system` on every command, and it retried for two thousand
    # turns before anything said so.
    link = Path(sys.executable)
    targets = [link]
    if link.is_symlink():
        target = Path(os.readlink(link))
        targets.append(target if target.is_absolute() else link.parent / target)
    for candidate in targets:
        prefix = candidate.parent
        prefix = prefix.parent if prefix.name == "bin" else prefix
        if not prefix.is_dir() or str(prefix) == str(venv):
            continue
        if prefix in (Path("."), Path("/")):
            # Never the sandbox root, whatever a link resolves to: a bind there takes the whole
            # filesystem away from everyone after it.
            continue
        binds.append((str(prefix), str(prefix)))
    return tuple(dict.fromkeys(binds))


@dataclass(frozen=True)
class Executed:
    """What one command did. The two runners word this differently, so it is worded once here."""

    output: str
    returncode: int
    timed_out: bool = False
    #: The command line that was dispatched. Carried on the result rather than remembered by the
    #: caller because a verdict read against a line the caller supplied is a verdict about the
    #: caller's memory — an op runtime reading exit 127 needs the name that actually ran, and this is
    #: the only place it is known for certain.
    command: str = ""
    #: Where the *whole* output went, when something would have been cut and a place was named. What a
    #: caller needs to know is whether it can get the rest, and after `output` has been cut this is the
    #: only thing that answers it.
    spilled: tuple[Path, ...] = ()
    #: The same files as the **command** could open them, when a mount was named. The host path is a
    #: private tmpfs away from the sandbox, so telling the node the host path tells it about a file it
    #: cannot read.
    visible: tuple[str, ...] = ()
    #: The output was cut and could not be kept whole. Said out loud rather than left for a caller to
    #: infer from an empty `spilled`, which is also what "nothing was cut" looks like.
    incomplete: bool = False


@dataclass
class NodeSandbox:
    """One node's sandbox: its mounts, its bounds, and the proof that it can run a command at all.

    Built once per node execution and used for every command that execution runs, so what a node may
    see and what it may not is decided in one place rather than restated per call.
    """

    tree: Path
    node_id: str
    network: bool
    timeout_seconds: float
    routes: tuple[str, ...] = ()
    # What this node was given, as (where it lives, where it is visible). Read-only, and never
    # copied: a pointer to a predecessor's workspace, which is also why its history comes with it.
    inputs: tuple[tuple[str, str], ...] = ()
    cancelled: Callable[[], bool] | None = None
    sandbox: BubblewrapWorkspaceSandbox = field(default=None)      # type: ignore[assignment]
    dirs: tuple[str, ...] = ()
    binds: tuple[tuple[str, str], ...] = ()
    #: Where to put output that would otherwise be cut, before it is cut. Settable after construction
    #: because who wants the whole output is an upper layer's business — a node that is bounding its
    #: context does, and one that is not should not be writing files for nothing.
    spill_dir: Path | None = None
    #: How many bytes may still be written, and where the spill becomes visible **inside** the sandbox.
    spill_limit_bytes: int | None = None
    spill_mount: str | None = None
    #: What the last command spilled, and whether anything was cut that could not be kept. Both are per
    #: command: read after the fact they describe the command that just ran, not the one before it.
    spilled: tuple[Path, ...] = ()
    #: The same files as the command could open them, when a mount was named.
    visible: tuple[str, ...] = ()
    incomplete: bool = False

    def __post_init__(self) -> None:
        if self.sandbox is None:
            self.sandbox = BubblewrapWorkspaceSandbox(allowed_commands=frozenset(SHELL))
        found = console_script("anchor-done")
        self.dirs = tool_dirs(found)
        self.binds = tool_binds(found)

    def readonly(self) -> tuple[tuple[str, str], ...]:
        """Everything an ordinary command may read and not write: its inputs, and the tools."""
        return (*self.binds, *self.inputs)

    def spec(self, command: str, *, timeout: float | None = None,
             readonly: tuple[str, ...] = WORKSPACE_READONLY) -> SandboxSpec:
        # Only what is there. A bind of a path that does not exist fails the whole sandbox, and a
        # caller may hand over a workspace it has not put under git yet — the real run does that
        # before a node starts, and a runner that refused a fresh directory would be refusing the
        # ordinary case to catch an unusual one.
        present = tuple(name for name in readonly if (self.tree / name).exists())
        # **The mount, not just a path in a message.** A spill the command cannot open is a spill the
        # model is told about and cannot use: the host directory is a private tmpfs away, so the read
        # fails, a pipeline's last command still exits zero, and a test that checks the host copy says
        # everything is fine. Only this one directory, read-only — mounting the record's parent would
        # put the audit trail inside the sandbox where the node could rewrite what is said about it.
        binds = self.readonly()
        if self.spill_dir is not None and self.spill_mount:
            binds = (*binds, (str(self.spill_dir), self.spill_mount))
        return SandboxSpec(
            workspace=self.tree, command=(*SHELL, "-c", command),
            timeout_seconds=float(timeout if timeout is not None else self.timeout_seconds),
            network=self.network, tool_dirs=self.dirs, readonly_binds=binds,
            workspace_readonly=present, spill_dir=self.spill_dir,
            spill_limit_bytes=self.spill_limit_bytes, spill_mount=self.spill_mount,
            cancelled=self.cancelled,
            env=(("ANCHOR_NODE", self.node_id), ("ANCHOR_ROUTES", ",".join(self.routes))))

    def run(self, command: str, *, timeout: float | None = None) -> Executed:
        result = self.sandbox.run(self.spec(command, timeout=timeout))
        # Assigned before the result is built, so a caller reading them after `run` gets this command's
        # and not the previous one's.
        self.spilled = tuple(result.spilled)
        self.visible = tuple(result.visible)
        self.incomplete = bool(result.incomplete)
        return Executed(output=result.stdout + result.stderr, returncode=result.returncode,
                        command=command, timed_out=bool(result.timed_out),
                        spilled=tuple(result.spilled),
                        visible=tuple(result.visible), incomplete=bool(result.incomplete))

    def require_working(self) -> None:
        """Run one trivial command with this node's real mounts, before anything else does.

        The sandbox is probed when it is constructed, but that probe knows nothing about the mounts
        this node will actually have. A bind whose path resolved to `.` mounted bubblewrap's working
        directory over the sandbox root, so every command after it failed with `Can't mkdir /tmp:
        Read-only file system` — and the node retried for two thousand turns before anything said so.
        One command here refuses it in the first second. The same principle as the probes in
        `sandbox.py`: a sandbox that cannot run a command is unavailable, not slow.
        """
        result = self.run(f"touch {PROBE} && rm -f {PROBE}", timeout=60.0)
        if result.returncode != 0:
            raise RuntimeError(
                "the sandbox cannot run a command in this node's workspace: "
                + (result.output.strip()[:500] or "no output"))
