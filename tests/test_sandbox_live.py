"""The sandbox against a real bubblewrap, with the mounts a node actually gets.

`tests/test_sandbox.py` checks the refusals with a stand-in binary, which is fast and runs anywhere.
This file checks the thing those refusals are about, and it needs a kernel that will create a
namespace and mount inside it. It skips where that is unavailable rather than reporting a pass.

Why it exists: a bind whose path resolved to `.` — `os.readlink` on `<venv>/bin/python` answers
`python3`, and `Path("python3").parent` is `.` — mounted bubblewrap's working directory over the
sandbox root. Every command then failed with `Can't mkdir /tmp: Read-only file system`, which the
agent retried for two thousand turns. Nothing in the suite noticed, because everything that used a
sandbox used a stand-in or a spec built without the tool binds.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox, SandboxSpec
from anchor.runtime.execenv import console_script, tool_binds


@pytest.fixture(scope="module")
def sandbox():
    try:
        return BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    except RuntimeError as exc:                     # a container that forbids namespaces
        pytest.skip(f"no usable sandbox on this machine: {exc}")


@pytest.fixture
def workspace(tmp_path):
    (tmp_path / ".git").mkdir()
    (tmp_path / "mine.md").write_text("written by its owner\n", encoding="utf-8")
    return tmp_path


def test_no_bind_is_a_relative_path():
    """A relative bind is bubblewrap's working directory mounted over the sandbox root.

    That is not a broken mount, which is what makes it dangerous: it succeeds, and then everything
    after it fails.
    """
    binds = tool_binds(console_script("anchor-scholarly"))

    for source, destination in binds:
        assert Path(source).is_absolute(), f"{source!r} would be resolved against bubblewrap's cwd"
        assert Path(destination).is_absolute(), destination
        assert Path(source) not in (Path("."), Path("/")), source


def test_a_node_can_run_a_command_in_its_workspace(sandbox, workspace):
    """The one command that would have caught the above in the first second instead of the 2000th."""
    result = sandbox.run(SandboxSpec(workspace=workspace, command=("sh", "-c", "echo alive"),
                                     readonly_binds=tool_binds(console_script("anchor-scholarly")),
                                     workspace_readonly=(".git",)))

    assert result.ok, result.stderr
    assert result.stdout.strip() == "alive"


def test_the_workspace_is_writable_and_git_is_not(sandbox, workspace):
    """The workspace is the node's own; its history is readable and not rewritable."""
    result = sandbox.run(SandboxSpec(
        workspace=workspace,
        command=("sh", "-c",
                 "echo added >> mine.md && echo writable; "
                 "echo tampered >> .git/config 2>/dev/null && echo REWRITABLE || echo history-safe"),
        workspace_readonly=(".git",)))

    assert result.ok, result.stderr
    assert "writable" in result.stdout
    assert "history-safe" in result.stdout, "a node could rewrite its own record"
    assert "REWRITABLE" not in result.stdout


def test_an_input_is_readable_and_not_writable(sandbox, workspace, tmp_path):
    """What an edge carries: a pointer a node can read, including the history behind it."""
    upstream = tmp_path / "upstream"
    upstream.mkdir()
    (upstream / "theirs.md").write_text("from the node before\n", encoding="utf-8")
    subprocess.run(["git", "init", "-q", str(upstream)], check=True)
    subprocess.run(["git", "-C", str(upstream), "-c", "user.name=A", "-c", "user.email=a@b",
                    "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(upstream), "-c", "user.name=A", "-c", "user.email=a@b",
                    "commit", "-q", "-m", "their work"], check=True)

    result = sandbox.run(SandboxSpec(
        workspace=workspace,
        command=("sh", "-c",
                 "cat /in/gather/theirs.md; "
                 "git --git-dir=/in/gather/.git log --format=%s; "
                 "echo tampered >> /in/gather/theirs.md 2>/dev/null && echo WROTE || echo refused"),
        readonly_binds=((str(upstream), "/in/gather"),)))

    assert result.ok, result.stderr
    assert "from the node before" in result.stdout, "the file was not readable through the pointer"
    assert "their work" in result.stdout, "its history did not come with it"
    assert "refused" in result.stdout and "WROTE" not in result.stdout
    assert (upstream / "theirs.md").read_text(encoding="utf-8") == "from the node before\n"


def test_the_interpreter_the_tool_needs_is_visible(sandbox, workspace):
    """The reason those binds exist at all: a console script's shebang names a path, not a program."""
    found = console_script("anchor-scholarly")
    if not found:
        pytest.skip("anchor-scholarly is not installed here")

    result = sandbox.run(SandboxSpec(
        workspace=workspace,
        command=("sh", "-c", f"{found} sources >/dev/null 2>&1; echo rc=$?"),
        timeout_seconds=120.0,
        tool_dirs=(str(Path(found).parent),),
        readonly_binds=tool_binds(found)))

    assert result.ok, result.stderr
    assert "rc=0" in result.stdout or "rc=1" in result.stdout, \
        f"the tool could not start at all: {result.stdout.strip()}"
    assert "bad interpreter" not in result.stdout
    assert sys.version_info[:2] == (3, 12), "the bind chain above is written for this interpreter"
