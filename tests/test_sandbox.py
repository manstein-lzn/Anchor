"""The sandbox has to be able to isolate, not merely be installed.

Presence was the only check, and it passed in a container whose AppArmor profile denies namespace
creation: every command failed with `No permissions to create new namespace`, the node retried, and
the run spent its whole step budget finding out. The probe turns that into a refusal at construction.
"""

from __future__ import annotations

import pytest

from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox, SandboxSpec


def _fake_bwrap(tmp_path, *, exit_code: int, stderr: str = ""):
    """A stand-in for the real binary, so the probe can be tested without one."""
    path = tmp_path / "bwrap"
    path.write_text(f"#!/bin/sh\n[ -n '{stderr}' ] && echo '{stderr}' >&2\nexit {exit_code}\n",
                    encoding="utf-8")
    path.chmod(0o755)
    return str(path)


def test_a_binary_that_cannot_create_a_namespace_is_refused(tmp_path):
    """Measured: 60 steps and two attempts were spent learning what this probe knows immediately."""
    binary = _fake_bwrap(tmp_path, exit_code=1,
                         stderr="bwrap: No permissions to create new namespace")

    with pytest.raises(RuntimeError) as caught:
        BubblewrapWorkspaceSandbox(binary=binary)

    message = str(caught.value)
    assert "cannot create a namespace" in message
    assert "No permissions to create new namespace" in message, "the reason is passed through"


def test_a_working_binary_is_accepted(tmp_path):
    """The probe must not refuse a sandbox that works."""
    sandbox = BubblewrapWorkspaceSandbox(binary=_fake_bwrap(tmp_path, exit_code=0))

    assert sandbox.name == "bubblewrap"


def test_a_missing_binary_is_still_reported_as_missing(tmp_path):
    """ADR-034's original half, kept: absent means unavailable rather than degraded."""
    with pytest.raises(RuntimeError) as caught:
        BubblewrapWorkspaceSandbox(binary=str(tmp_path / "no-such-bwrap"))

    assert "not found" in str(caught.value)


def _fake_bwrap_py(tmp_path, script: str):
    """A stand-in written in Python, so the shell quoting of the argv inspection stays readable."""
    path = tmp_path / "bwrap"
    path.write_text(f"#!/usr/bin/env python3\n{script}", encoding="utf-8")
    path.chmod(0o755)
    return str(path)


def test_a_host_that_refuses_procfs_still_gets_a_sandbox(tmp_path):
    """A host can allow every mount but procfs, and the sandbox should not die over a convenience.

    Measured on a container that creates namespaces and tmpfs mounts happily, and answers
    `Operation not permitted` to a procfs mount inside a user namespace. Anchor's nodes run there;
    they simply cannot see /proc, which is a fresh procfs for this sandbox's own pid namespace and
    so costs functionality rather than isolation.
    """
    binary = _fake_bwrap_py(tmp_path, (
        "import sys\n"
        "if '--proc' in sys.argv:\n"
        "    print('bwrap: Can\\'t mount proc on /newroot/proc: Operation not permitted',"
        " file=sys.stderr)\n"
        "    sys.exit(1)\n"
        "sys.exit(0)\n"))

    sandbox = BubblewrapWorkspaceSandbox(binary=binary)

    assert sandbox._proc is False
    assert "--proc" not in sandbox._argv(SandboxSpec(workspace=tmp_path, command=("sh",))), \
        "the flag is left out rather than the sandbox being refused"


def test_a_host_that_allows_procfs_keeps_it(tmp_path):
    """The other direction, so the fallback is not simply always on."""
    sandbox = BubblewrapWorkspaceSandbox(binary=_fake_bwrap_py(tmp_path, "import sys\nsys.exit(0)\n"))

    assert sandbox._proc is True
