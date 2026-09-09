"""Writable workspaces: every mutation leaves a revision and an audit entry.

These tests pin the contract that a caller cannot write without producing a
committable revision, cannot escape the worktree, and cannot write a frozen or
archived workspace. The source repository gains a branch and shared objects, but
its working tree is never touched.
"""

import subprocess
from pathlib import Path

import pytest

from anchor.domain.content import parse
from anchor.domain.project import Project
from anchor.domain.workspace import WorkspaceState
from anchor.runtime.workspace import WorkspaceResolver
from anchor.runtime.workspaces import WorkspaceError, WorkspaceManager


def make_repo(tmp_path):
    root = tmp_path / "repo"
    root.parent.mkdir(parents=True, exist_ok=True)
    (root / "src").mkdir(parents=True)
    (root / "readme.md").write_text("base readme\n", encoding="utf-8")
    for args in (["init", "-q"], ["config", "user.email", "t@example.com"],
                 ["config", "user.name", "Test"], ["add", "-A"], ["commit", "-qm", "init"]):
        subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True)
    sha = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"],
                         check=True, capture_output=True, text=True).stdout.strip()
    return root, sha


@pytest.fixture
def managed(tmp_path, store):
    root, sha = make_repo(tmp_path)
    store.create_project(Project(project_id="proj-1", name="Project one", root=str(root)))
    manager = WorkspaceManager(store, root=tmp_path / "worktrees")
    return store, manager, root, sha


def test_create_forks_a_writable_worktree_without_touching_the_source(managed):
    store, manager, root, sha = managed
    workspace = manager.create(project_id="proj-1", base_revision=sha, workspace_id="ws-1")
    assert workspace.state is WorkspaceState.ACTIVE
    assert workspace.base_revision == sha and workspace.current_revision == sha
    assert workspace.branch == "anchor/ws-1"
    assert (Path(workspace.path) / "readme.md").read_text(encoding="utf-8") == "base readme\n"

    # The source working tree is unchanged; only a branch and shared objects were added.
    assert subprocess.run(["git", "-C", str(root), "status", "--porcelain", "--untracked-files=no"],
                          capture_output=True, text=True, check=True).stdout.strip() == ""
    branches = subprocess.run(["git", "-C", str(root), "branch", "--list", "anchor/ws-1"],
                              capture_output=True, text=True, check=True).stdout
    assert "anchor/ws-1" in branches
    assert not (root / "worktrees").exists()


def test_write_produces_a_revision_a_ledger_entry_and_an_event(managed):
    store, manager, _, sha = managed
    manager.create(project_id="proj-1", base_revision=sha, workspace_id="ws-1")
    operation = manager.write_text("ws-1", "src/new.py", "print('new')\n", actor="agent-a")

    assert operation.after_revision and operation.after_revision != sha
    assert operation.before_revision == sha
    assert operation.content_hash and len(operation.content_hash) == 64
    workspace = store.get_workspace("ws-1")
    assert workspace.current_revision == operation.after_revision

    # The revision is readable through the content boundary.
    resolver = WorkspaceResolver(store)
    ref = parse(f"workspace://ws-1@{operation.after_revision}/src/new.py")
    assert resolver.read_text(ref) == "print('new')\n"

    ledger = store.list_workspace_operations("ws-1")
    assert [item.kind.value for item in ledger] == ["create", "write"]
    events = store.list_events("ws-1")
    assert [item["event_type"] for item in events] == ["workspace.create", "workspace.write"]
    assert events[-1]["payload"]["after_revision"] == operation.after_revision


def test_delete_commits_a_removal(managed):
    store, manager, _, sha = managed
    manager.create(project_id="proj-1", base_revision=sha, workspace_id="ws-1")
    operation = manager.delete("ws-1", "readme.md", actor="agent-a")
    assert operation.kind.value == "delete"
    with pytest.raises(Exception):
        WorkspaceResolver(store).read_text(
            parse(f"workspace://ws-1@{operation.after_revision}/readme.md"))


def test_paths_cannot_escape_the_worktree(managed):
    _, manager, _, sha = managed
    manager.create(project_id="proj-1", base_revision=sha, workspace_id="ws-1")
    for bad in ("../outside.txt", "/etc/passwd", "src/../../escape"):
        with pytest.raises(Exception):
            manager.write_text("ws-1", bad, "x", actor="agent-a")


def test_frozen_and_archived_workspaces_are_not_writable(managed):
    store, manager, _, sha = managed
    manager.create(project_id="proj-1", base_revision=sha, workspace_id="ws-1")
    manager.write_text("ws-1", "a.txt", "a\n", actor="agent-a")
    frozen = manager.freeze("ws-1", actor="operator")
    assert frozen.state is WorkspaceState.FROZEN and frozen.current_revision
    with pytest.raises(WorkspaceError, match="not writable"):
        manager.write_text("ws-1", "b.txt", "b\n", actor="agent-a")

    archived = manager.archive("ws-1", actor="operator")
    assert archived.state is WorkspaceState.ARCHIVED
    assert not Path(archived.path).exists()
    with pytest.raises(WorkspaceError, match="not writable"):
        manager.write_text("ws-1", "c.txt", "c\n", actor="agent-a")


def test_create_fails_closed_on_unknown_project_or_revision(managed):
    _, manager, _, sha = managed
    with pytest.raises(WorkspaceError, match="unknown project"):
        manager.create(project_id="nope", base_revision=sha)
    with pytest.raises(WorkspaceError):
        manager.create(project_id="proj-1", base_revision="0" * 40)
    with pytest.raises(Exception):
        manager.create(project_id="proj-1", base_revision="main")


def test_content_size_limit_is_enforced(managed, tmp_path):
    store, manager, root, sha = managed
    small = WorkspaceManager(store, root=tmp_path / "small", max_file_bytes=4)
    small.create(project_id="proj-1", base_revision=sha, workspace_id="ws-small")
    with pytest.raises(WorkspaceError, match="over the 4 limit"):
        small.write_text("ws-small", "big.txt", "12345", actor="agent-a")


def test_workspace_root_and_path_are_absolute(tmp_path, store, monkeypatch):
    """A relative root must not leak into the record or the worktree path.

    Regression: a relative path resolved per process, and `git -C <relative>`
    walked up into an unrelated repository, committing into the wrong history.
    """
    root, sha = make_repo(tmp_path)
    store.create_project(Project(project_id="p", name="P", root=str(root)))
    monkeypatch.chdir(tmp_path)
    manager = WorkspaceManager(store, root="relative/worktrees")
    workspace = manager.create(project_id="p", base_revision=sha, workspace_id="ws-rel")
    assert Path(workspace.path).is_absolute()
    assert Path(workspace.path).is_relative_to(tmp_path.resolve())

    operation = manager.write_text("ws-rel", "a.txt", "x\n", actor="t")
    assert operation.after_revision != sha
    assert (Path(workspace.path) / "a.txt").read_text(encoding="utf-8") == "x\n"


def test_worktree_verification_rejects_a_path_in_another_repository(tmp_path):
    from anchor.runtime.workspaces import GitWorktree

    repo_a, _ = make_repo(tmp_path / "a")
    repo_b, _ = make_repo(tmp_path / "b")
    nested = repo_b / "sub"
    nested.mkdir()
    with pytest.raises(WorkspaceError, match="different repository"):
        GitWorktree(str(repo_a), nested, "anchor/foreign").verify()
