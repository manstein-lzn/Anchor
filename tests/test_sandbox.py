"""Read-only workspace execution: an observation, not a mutation.

The sandbox runs an allowlisted command against a materialized immutable
revision. These tests pin the guarantees: the command sees the pinned bytes, it
cannot write to the workspace, a disallowed command is refused before execution,
and the timeout and output cap hold.
"""

import shutil
import subprocess
from pathlib import Path

import pytest

from anchor.domain.content import parse
from anchor.domain.project import Project
from anchor.runtime.sandbox import (
    BubblewrapWorkspaceSandbox,
    SandboxDenied,
    SandboxSpec,
    SubprocessWorkspaceSandbox,
)
from anchor.runtime.workspace import (
    GitWorkspaceBackend,
    execute_in_workspace,
)

bwrap_only = pytest.mark.skipif(shutil.which("bwrap") is None, reason="bwrap not installed")


def make_repo(tmp_path):
    root = tmp_path / "repo"
    (root / "src").mkdir(parents=True)
    (root / "readme.md").write_text("hello workspace\n", encoding="utf-8")
    (root / "src" / "app.py").write_text("print('hi')\n", encoding="utf-8")
    for args in (["init", "-q"], ["config", "user.email", "t@example.com"],
                 ["config", "user.name", "Test"], ["add", "-A"], ["commit", "-qm", "init"]):
        subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True)
    sha = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"],
                         check=True, capture_output=True, text=True).stdout.strip()
    return root, sha


@pytest.fixture
def project(tmp_path, store):
    root, sha = make_repo(tmp_path)
    store.create_project(Project(project_id="proj-1", name="Project one", root=str(root)))
    return store, root, sha


def test_materialize_extracts_the_pinned_tree_without_touching_the_repo(project, tmp_path):
    _, root, sha = project
    before = subprocess.run(["git", "-C", str(root), "status", "--porcelain"],
                            capture_output=True, text=True, check=True).stdout
    destination = tmp_path / "tree"
    GitWorkspaceBackend(str(root)).materialize(sha, destination)
    assert (destination / "readme.md").read_text(encoding="utf-8") == "hello workspace\n"
    assert (destination / "src" / "app.py").is_file()
    after = subprocess.run(["git", "-C", str(root), "status", "--porcelain"],
                           capture_output=True, text=True, check=True).stdout
    assert before == after, "materializing a revision must not dirty the source repository"


@bwrap_only
def test_bubblewrap_reads_the_pinned_revision(project):
    store, _, sha = project
    sandbox = BubblewrapWorkspaceSandbox()
    result = execute_in_workspace(store, parse(f"workspace://proj-1@{sha}/readme.md"),
                                  ["cat", "readme.md"], sandbox=sandbox)
    assert result.ok and result.stdout == "hello workspace\n"

    listing = execute_in_workspace(store, parse(f"workspace://proj-1@{sha}/readme.md"),
                                   ["ls", "src"], sandbox=sandbox)
    assert listing.ok and "app.py" in listing.stdout


@bwrap_only
def test_bubblewrap_workspace_is_read_only_and_has_no_network(project):
    store, _, sha = project
    sandbox = BubblewrapWorkspaceSandbox(allowed_commands=frozenset({"ls", "cat", "sh"}))
    write_attempt = execute_in_workspace(
        store, parse(f"workspace://proj-1@{sha}/readme.md"),
        ["sh", "-c", "echo x > new.txt"], sandbox=sandbox)
    assert not write_attempt.ok
    assert "read-only" in write_attempt.stderr.lower()


def test_disallowed_command_is_refused_before_execution(project, tmp_path):
    _, root, sha = project
    sandbox = SubprocessWorkspaceSandbox(allowed_commands=frozenset({"cat"}))
    spec = SandboxSpec(workspace=Path(root), command=("rm", "-rf", "."))
    with pytest.raises(SandboxDenied, match="command_not_allowed"):
        sandbox.run(spec)


def test_allowlist_accepts_absolute_paths_by_basename(project):
    store, _, sha = project
    sandbox = SubprocessWorkspaceSandbox(allowed_commands=frozenset({"cat"}))
    result = execute_in_workspace(store, parse(f"workspace://proj-1@{sha}/readme.md"),
                                  ["/bin/cat", "readme.md"], sandbox=sandbox)
    assert result.ok and result.stdout == "hello workspace\n"


def test_timeout_and_output_cap_hold(project):
    store, _, sha = project
    sandbox = SubprocessWorkspaceSandbox(allowed_commands=frozenset({"sh", "cat"}))
    timed_out = execute_in_workspace(store, parse(f"workspace://proj-1@{sha}/readme.md"),
                                     ["sh", "-c", "sleep 5"], sandbox=sandbox,
                                     timeout_seconds=0.2)
    assert timed_out.timed_out and timed_out.returncode == 124

    capped = execute_in_workspace(store, parse(f"workspace://proj-1@{sha}/readme.md"),
                                  ["cat", "readme.md"], sandbox=sandbox, max_output_bytes=4)
    assert capped.stdout.startswith("hell") and "truncated" in capped.stdout


def test_empty_command_is_refused(project):
    store, _, sha = project
    sandbox = SubprocessWorkspaceSandbox()
    with pytest.raises(SandboxDenied, match="empty_command"):
        execute_in_workspace(store, parse(f"workspace://proj-1@{sha}/readme.md"),
                             [], sandbox=sandbox)
