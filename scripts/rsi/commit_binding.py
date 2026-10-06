"""Verify legacy and Rust-native RSI artifact references at the validator boundary."""

from __future__ import annotations

import hashlib
import json
import re
from pathlib import Path
import subprocess


_FS_ID = re.compile(r"fs[12]-[0-9a-f]{64}\Z")
_OID = re.compile(r"[0-9a-f]{40}\Z")
_SHA256 = re.compile(r"[0-9a-f]{64}\Z")


def _git(directory: Path, *args: str) -> bytes:
    return subprocess.check_output(
        ["git", f"--git-dir={directory / '.git'}", *args],
        stderr=subprocess.STDOUT,
    )


def _verify_projection(directory: Path, expected: dict[str, object]) -> None:  # noqa: C901
    """Check the immutable Git projection mounted for a Rust artifact.

    The projection is derived from the authoritative fs2 snapshot.  The
    commit message binds the artifact id and manifest digest; the tree check
    binds the mounted bytes, and fsck rejects unreachable or malformed Git
    objects.  The host creates this directory read-only for graph inputs.
    """
    if not directory.is_dir() or not (directory / ".git").is_dir():
        raise ValueError("native artifact input is missing its Git projection")
    head = _git(directory, "rev-parse", "HEAD").decode().strip()
    if not _OID.fullmatch(head):
        raise ValueError("native artifact Git HEAD is not a SHA-1 object")
    commit = _git(directory, "cat-file", "commit", "HEAD")
    artifact_id = expected["id"]
    expected_message = (
        f"Anchor Artifact {artifact_id}\n"
        f"Artifact-Node: {expected['node_id']}\n"
        f"Artifact-Invocation: {expected['invocation']}\n"
    )
    marker = expected_message + "Manifest-SHA256: "
    if not commit.startswith(b"tree ") or b"\n\n" not in commit:
        raise ValueError("native artifact Git commit is malformed")
    message = commit.split(b"\n\n", 1)[1].decode("utf-8")
    if not message.startswith(marker) or not message.endswith("\n"):
        raise ValueError("native artifact Git commit is not bound to its artifact")
    manifest_hash = message[len(marker) : -1]
    if not _SHA256.fullmatch(manifest_hash):
        raise ValueError("native artifact manifest binding is not a SHA-256 digest")

    if message != marker + manifest_hash + "\n":
        raise ValueError("native artifact Git commit identity does not match its CommitRef")

    # A host-side projection file, when available to an offline validator, is
    # checked as an additional consistency witness. Runtime mounts need not
    # expose this file because the commit message carries the same identity.
    projection_path = next(
        (candidate for candidate in (directory / "projection.json", directory.parent / "projection.json")
         if candidate.is_file()),
        None,
    )
    if projection_path is not None:
        try:
            projection = json.loads(projection_path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            raise ValueError("native artifact Git projection identity is unreadable") from exc
        if (projection.get("format") != 1 or projection.get("artifact") != expected
                or projection.get("head") != head or projection.get("manifest_sha256") != manifest_hash):
            raise ValueError("native artifact Git projection identity does not match its commit")
        git_files = projection.get("git_files")
        if not isinstance(git_files, dict):
            raise ValueError("native artifact Git projection has no file inventory")
        observed_git_files = {}
        for path in sorted(git_files):
            candidate = directory / ".git" / path
            if not candidate.is_file() or candidate.is_symlink():
                raise ValueError("native artifact Git metadata is incomplete")
            observed_git_files[path] = hashlib.sha256(candidate.read_bytes()).hexdigest()
        if observed_git_files != git_files:
            raise ValueError("native artifact Git metadata differs from its projection")

    listing = _git(directory, "ls-tree", "-rz", "HEAD")
    for entry in listing.split(b"\0"):
        if not entry:
            continue
        metadata, raw_path = entry.split(b"\t", 1)
        mode, object_type, oid = metadata.split(b" ", 2)
        path = raw_path.decode("utf-8")
        if mode != b"100644" or object_type != b"blob" or not _OID.fullmatch(oid.decode()):
            raise ValueError("native artifact Git tree contains an unsafe entry")
        if (not path or path.startswith("/") or "\\" in path
                or any(part in ("", ".", "..") for part in path.split("/"))
                or path == ".git" or path.startswith(".git/") or ".." in Path(path).parts):
            raise ValueError("native artifact Git tree contains an unsafe path")
        source = directory / path
        if not source.is_file() or source.is_symlink():
            raise ValueError("native artifact Git tree does not match mounted files")
        payload = source.read_bytes()
        digest = hashlib.sha1(b"blob " + str(len(payload)).encode() + b"\0" + payload).hexdigest()
        if digest != oid.decode():
            raise ValueError("native artifact mounted bytes differ from Git projection")
    if _git(directory, "fsck", "--full", "--strict", "--no-reflogs").strip():
        raise ValueError("native artifact Git projection contains invalid objects")


def verify_commit(directory: Path, expected, *, node: str) -> None:
    """Verify a branch commit from a legacy string or Rust CommitRef object."""
    if isinstance(expected, str):
        actual = _git(directory, "rev-parse", "HEAD").decode().strip()
        if expected != actual:
            raise ValueError("joined commit does not match the branch Git HEAD")
        return
    if not isinstance(expected, dict) or set(expected) != {"id", "node_id", "invocation"}:
        raise ValueError("native joined commit must be an exact CommitRef object")
    artifact_id = expected["id"]
    if not isinstance(artifact_id, str) or not _FS_ID.fullmatch(artifact_id):
        raise ValueError("native joined commit has an invalid artifact id")
    if expected["node_id"] != node:
        raise ValueError("native joined commit node identity differs from its branch")
    if isinstance(expected["invocation"], bool) or not isinstance(expected["invocation"], int) or expected["invocation"] <= 0:
        raise ValueError("native joined commit has an invalid invocation")
    _verify_projection(directory, expected)
