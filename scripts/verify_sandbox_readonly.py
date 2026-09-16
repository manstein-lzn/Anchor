#!/usr/bin/env python3
"""Is a read-only bind actually read-only from inside the sandbox?

Everything Anchor hands a node is a pointer: another node's workspace is mounted read-only, and so is
that workspace's `.git`, so a node can read a predecessor's history and not rewrite it. Nothing is
copied. That design rests on one kernel guarantee, and this script is the check that it holds on the
machine you are about to run on.

It is a script and not a test because bubblewrap needs real namespaces, which containers commonly
forbid — and a sandbox that only *appears* to work is worse than one that refuses. So this runs the
real `BubblewrapWorkspaceSandbox` with the real flags, never a re-implementation of them: a shell
copy of the command line would drift from `sandbox.py` and then certify a sandbox nobody uses.

The documented half is settled. `mount_setattr(2)` lists `EPERM` for a `MOUNT_ATTR_RDONLY` flag that
is locked, and says the flags become locked when a mount and user namespace pair is created — naming
the attack exactly: "a calling process that is privileged in the new user namespace would, in the
absence of such locking, be able to alter sensitive mount properties (e.g., to remount a mount that
was marked read-only as read-write in the new mount namespace)". Bubblewrap creates that pair.

What the manuals do not settle is whether that also covers the binds bubblewrap makes *after* the
namespace exists, which is the case here. Hence the checks.

Usage:  .venv/bin/python scripts/verify_sandbox_readonly.py
Exit:   0 when every guarantee held, 1 otherwise, naming the one that broke.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from anchor.runtime.sandbox import BubblewrapWorkspaceSandbox, SandboxSpec  # noqa: E402

GIT_ENV = "env GIT_OPTIONAL_LOCKS=0"


class Report:
    def __init__(self) -> None:
        self.failures: list[str] = []

    def ok(self, what: str, detail: str = "") -> None:
        print(f"  \033[32mok\033[0m      {what}" + (f"  ({detail})" if detail else ""))

    def broken(self, what: str, detail: str = "") -> None:
        print(f"  \033[31mBROKEN\033[0m  {what}" + (f"  ({detail})" if detail else ""))
        self.failures.append(what)


def main() -> int:
    root = Path(tempfile.mkdtemp(prefix="anchor-readonly-"))
    try:
        return check(root)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def check(root: Path) -> int:
    # What a predecessor node left behind: files, and a repo holding its history.
    other = root / "other"
    (other / "repo").mkdir(parents=True)
    (other / "file.txt").write_text("written by its owner\n", encoding="utf-8")
    subprocess.run(["git", "init", "-q", str(other / "repo")], check=True)
    for message in ("first", "second"):
        subprocess.run(["git", "-C", str(other / "repo"), "-c", "user.name=A", "-c", "user.email=a@b",
                        "commit", "-q", "--allow-empty", "-m", message], check=True)

    workspace = root / "workspace"
    workspace.mkdir()

    try:
        sandbox = BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"sh"}))
    except RuntimeError as exc:
        # The construction probe already refuses a sandbox that cannot isolate. Saying so here rather
        # than reporting every check below as a pass is the difference between a measurement and a
        # decoration.
        print(f"this machine cannot run the sandbox, so nothing below would be a real result:\n  {exc}")
        return 1

    report = Report()
    bind = ((str(other), "/in"),)

    def run(command: str):
        return sandbox.run(SandboxSpec(workspace=workspace, command=("sh", "-c", command),
                                       readonly_binds=bind))

    # The sandbox has to come up at all. Without this, a bubblewrap that fails to build the mount
    # namespace makes every check below exit non-zero and look like a refusal — which is how the
    # first version of this script reported four passes and a failure that were all the same setup
    # error.
    alive = run("echo alive")
    if not alive.ok or alive.stdout.strip() != "alive":
        print(f"the sandbox does not come up, so nothing below would be a real result:\n"
              f"  rc={alive.returncode} out={alive.stdout!r} err={alive.stderr.strip()}")
        return 1

    print("a read-only bind, from inside the sandbox")

    result = run('echo x >> /in/file.txt')
    if not result.ok:
        report.ok("a write is refused", result.stderr.strip().splitlines()[-1] if result.stderr else "")
    else:
        report.broken("a write SUCCEEDED — the bind is not read-only")

    # The attack the kernel documents: clear the flag on the mount itself.
    result = run('mount -o remount,rw /in && echo x >> /in/file.txt')
    if not result.ok:
        report.ok("remount,rw is refused", result.stderr.strip().splitlines()[-1] if result.stderr else "")
    else:
        report.broken("remount,rw SUCCEEDED and the write landed")

    # A second bind of the same tree, made inside the sandbox without the read-only flag. This needs
    # no remount of anything, which is why it is checked separately.
    result = run('mkdir -p /tmp/x && mount --bind /in /tmp/x && echo x >> /tmp/x/file.txt')
    if not result.ok:
        report.ok("rebinding it writable is refused")
    else:
        report.broken("a second bind SUCCEEDED and the write landed")

    # The same, after a nested user namespace hands the process a fresh capability set. This is the
    # route the kernel's locking exists to close.
    result = run('unshare -Ur sh -c "mount -o remount,rw /in && echo x >> /in/file.txt"')
    if not result.ok:
        report.ok("a nested user namespace does not help")
    else:
        report.broken("a nested user namespace SUCCEEDED in remounting")

    # Nothing above should have changed the real file, whichever way the sandbox refused.
    if (other / "file.txt").read_text(encoding="utf-8") == "written by its owner\n":
        report.ok("the file behind the bind is untouched")
    else:
        report.broken("the file behind the bind WAS CHANGED — the mount did not protect it")

    print()
    print("git through a read-only bind")

    result = run(f"{GIT_ENV} git --git-dir=/in/repo/.git log --oneline")
    lines = result.stdout.strip().splitlines()
    if result.ok and len(lines) == 2:
        report.ok("git log reads history", lines[0])
    else:
        report.broken("git log failed or saw the wrong history",
                      result.stderr.strip() or repr(result.stdout))

    # Reading `git status` needs the index refreshed, which a read-only mount forbids. GIT_OPTIONAL_LOCKS=0
    # is what Anchor sets so that a read stays a read; if that is not enough, say so here rather than
    # letting a node discover it.
    result = run(f"{GIT_ENV} git --git-dir=/in/repo/.git status")
    if result.ok:
        report.ok("git status works read-only (GIT_OPTIONAL_LOCKS=0 is enough)")
    else:
        report.broken("git status cannot run read-only, so a node would hit this too",
                      result.stderr.strip().splitlines()[-1] if result.stderr else "")

    result = run(f"{GIT_ENV} git --git-dir=/in/repo/.git -c user.name=X -c user.email=x@y "
                 f"commit -q --allow-empty -m tampered")
    if not result.ok:
        report.ok("git commit is refused")
    else:
        report.broken("git commit SUCCEEDED — a node could rewrite another node's history")

    result = run(f"{GIT_ENV} git -C /in/repo checkout -q -- .")
    if not result.ok:
        report.ok("git checkout is refused")
    else:
        report.broken("git checkout SUCCEEDED against a read-only workspace")

    print()
    if report.failures:
        print(f"{len(report.failures)} guarantee(s) BROKEN — do not mount another node's workspace "
              f"read-only without addressing these")
        return 1
    print("every guarantee held: a pointer really is read-only, and git cannot write through it")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
