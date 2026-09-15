"""The sandbox has to be able to isolate, not merely be installed.

Presence was the only check, and it passed in a container whose AppArmor profile denies namespace
creation: every command failed with `No permissions to create new namespace`, the node retried, and
the run spent its whole step budget finding out. The probe turns that into a refusal at construction.
"""

from __future__ import annotations

import pytest

from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox


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
