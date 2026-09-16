#!/usr/bin/env bash
# Is a read-only bind actually read-only from inside the sandbox?
#
# Everything Anchor hands a node is a pointer: another node's workspace is mounted read-only, and so
# is that workspace's `.git`, so a node can read history and not rewrite it. That design rests on one
# kernel guarantee, and this script is the check that the guarantee holds on the machine you are
# about to run on. It is not a unit test: bubblewrap needs real namespaces, which containers commonly
# forbid, and a sandbox that only appears to work is worse than one that refuses.
#
# The documented position (mount_setattr(2), EPERM) is that MOUNT_ATTR_RDONLY becomes *locked* when a
# mount and user namespace pair is created, precisely so that a process privileged in the new user
# namespace cannot remount read-only as read-write. Bubblewrap creates that pair. What the manuals do
# not settle is whether that also covers the binds bubblewrap makes *after* the namespace exists,
# which is what Anchor uses — hence test 2 and test 3.
#
# Usage:  scripts/verify_sandbox_readonly.sh
# Exit:   0 when every guarantee held, 1 otherwise (each failure says which one broke).

set -u

BWRAP=${BWRAP:-bwrap}
failures=0

pass() { printf '  \033[32mok\033[0m    %s\n' "$1"; }
fail() { printf '  \033[31mBROKEN\033[0m %s\n' "$1"; failures=$((failures + 1)); }

if ! command -v "$BWRAP" >/dev/null 2>&1; then
  echo "bwrap is not installed ($BWRAP); nothing to check" >&2
  exit 1
fi

# The probe has to succeed before any of the rest means anything: a bubblewrap that cannot create a
# namespace fails every test below for the wrong reason and would look like a pass.
if ! "$BWRAP" --unshare-all --ro-bind / / -- true 2>/dev/null; then
  echo "$BWRAP cannot create a namespace here, so nothing below would be a real result:" >&2
  "$BWRAP" --unshare-all --ro-bind / / -- true
  exit 1
fi

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
readonly_dir="$root/other"
mkdir -p "$readonly_dir"
echo "written by its owner" >"$readonly_dir/file.txt"

# A repo, to check that history can be read through a read-only mount and not written.
git init -q "$readonly_dir/repo" 2>/dev/null || {
  echo "git is required for the history checks" >&2
  exit 1
}
git -C "$readonly_dir/repo" -c user.name=A -c user.email=a@b commit -q --allow-empty -m "first"
git -C "$readonly_dir/repo" -c user.name=A -c user.email=a@b commit -q --allow-empty -m "second"

echo "read-only bind, from inside the sandbox"

# 1. An ordinary write through the bind. This is the case git's file writes hit.
out=$("$BWRAP" --unshare-all --ro-bind / / --ro-bind "$readonly_dir" /in -- \
        sh -c 'echo x >> /in/file.txt' 2>&1)
if [ $? -ne 0 ]; then pass "a write is refused ($(echo "$out" | tail -1))"
else fail "a write SUCCEEDED — the bind is not read-only"; fi

# 2. Remounting the bind read-write. The documented attack; the kernel locks the flag for mounts
#    that existed when the namespace was created.
out=$("$BWRAP" --unshare-all --ro-bind / / --ro-bind "$readonly_dir" /in -- \
        mount -o remount,rw /in 2>&1)
if [ $? -ne 0 ]; then pass "remount,rw is refused ($(echo "$out" | tail -1))"
else
  # Refusing the remount is not enough on its own: if it succeeded, the next write is the proof.
  if "$BWRAP" --unshare-all --ro-bind / / --ro-bind "$readonly_dir" /in -- \
       sh -c 'mount -o remount,rw /in 2>/dev/null && echo x >> /in/file.txt' 2>/dev/null; then
    fail "remount,rw SUCCEEDED and the write landed"
  else
    fail "remount,rw reported success (write did not land, but the mount changed)"
  fi
fi

# 3. A second bind of the same tree, made inside the sandbox, without the read-only flag. This is the
#    one that would matter most here: it needs no remount of anything.
out=$("$BWRAP" --unshare-all --ro-bind / / --ro-bind "$readonly_dir" /in -- \
        sh -c 'mkdir -p /tmp/x && mount --bind /in /tmp/x && echo x >> /tmp/x/file.txt' 2>&1)
if [ $? -ne 0 ]; then pass "rebinding it writable is refused ($(echo "$out" | tail -1))"
else fail "a second bind SUCCEEDED and the write landed"; fi

# 4. The same, after gaining a fresh set of capabilities in a nested user namespace — the route a
#    determined agent would take, and the one the kernel's locking exists to stop.
out=$("$BWRAP" --unshare-all --ro-bind / / --ro-bind "$readonly_dir" /in -- \
        sh -c 'unshare -Ur sh -c "mount -o remount,rw /in && echo x >> /in/file.txt"' 2>&1)
if [ $? -ne 0 ]; then pass "a nested user namespace does not help ($(echo "$out" | tail -1))"
else fail "a nested user namespace SUCCEEDED in remounting"; fi

echo
echo "git through a read-only bind"

# 5. Reading history. GIT_OPTIONAL_LOCKS=0 is what Anchor sets: without it git tries to refresh the
#    index, and on a read-only mount that turns a read into a failure.
out=$("$BWRAP" --unshare-all --ro-bind / / --ro-bind "$readonly_dir" /in -- \
        env GIT_OPTIONAL_LOCKS=0 git --git-dir=/in/repo/.git log --oneline 2>&1)
if [ $? -eq 0 ] && [ "$(printf '%s\n' "$out" | wc -l)" -eq 2 ]; then
  pass "git log reads history ($(printf '%s\n' "$out" | head -1))"
else fail "git log failed or saw the wrong history: $(echo "$out" | tail -1)"; fi

# 6. Writing history. The whole point of mounting `.git`: read it, do not rewrite it.
out=$("$BWRAP" --unshare-all --ro-bind / / --ro-bind "$readonly_dir" /in -- \
        env GIT_OPTIONAL_LOCKS=0 git --git-dir=/in/repo/.git \
        -c user.name=X -c user.email=x@y commit -q --allow-empty -m tampered 2>&1)
if [ $? -ne 0 ]; then pass "git commit is refused ($(echo "$out" | tail -1))"
else fail "git commit SUCCEEDED — a node could rewrite another node's history"; fi

# 7. And the worktree half of the same thing: a node may not edit an upstream's files via git.
out=$("$BWRAP" --unshare-all --ro-bind / / --ro-bind "$readonly_dir" /in -- \
        env GIT_OPTIONAL_LOCKS=0 git -C /in/repo checkout -q -- . 2>&1)
if [ $? -ne 0 ]; then pass "git checkout is refused"
else fail "git checkout SUCCEEDED against a read-only workspace"; fi

echo
if [ "$failures" -eq 0 ]; then
  echo "every guarantee held: a pointer really is read-only, and git cannot write through it"
else
  echo "$failures guarantee(s) BROKEN — do not mount another node's workspace with --ro-bind alone"
fi
exit $(( failures > 0 ))
