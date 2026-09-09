"""Parallel branches: fork, merge, and fail closed on conflict.

A branch is an independent worktree forked from a shared revision. A join merges
branch revisions into its own workspace under `require_clean`; a conflict aborts
the merge and fails the node rather than silently keeping one side.
"""

import subprocess
from uuid import uuid4

import pytest

from anchor.domain.content import parse
from anchor.domain.project import Project
from anchor.runtime.join_merge import JoinMergeBehavior
from anchor.runtime.workspace import GitWorkspaceBackend
from anchor.runtime.workspaces import WorkspaceError, WorkspaceManager


def make_repo(tmp_path):
    root = tmp_path / "repo"
    root.mkdir()
    (root / "base.txt").write_text("base\n", encoding="utf-8")
    for args in (["init", "-q"], ["config", "user.email", "t@example.com"],
                 ["config", "user.name", "Test"], ["add", "-A"], ["commit", "-qm", "init"]):
        subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True)
    sha = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"],
                         check=True, capture_output=True, text=True).stdout.strip()
    return root, sha


@pytest.fixture
def branched(tmp_path, store):
    root, sha = make_repo(tmp_path)
    store.create_project(Project(project_id="proj-1", name="P", root=str(root)))
    manager = WorkspaceManager(store, root=tmp_path / "worktrees")
    manager.create(project_id="proj-1", base_revision=sha, workspace_id="ws-join")
    manager.fork("ws-join", new_workspace_id="ws-a", actor="setup")
    manager.fork("ws-join", new_workspace_id="ws-b", actor="setup")
    return store, manager, root, sha


def test_fork_creates_an_independent_worktree(branched):
    store, manager, root, sha = branched
    assert store.get_workspace("ws-a").base_revision == sha
    manager.write_text("ws-a", "a.txt", "from a\n", actor="a")
    assert not (root / "a.txt").exists()
    with pytest.raises(WorkspaceError):
        GitWorkspaceBackend(str(root)).read_text(store.get_workspace("ws-b").current_revision,
                                                 "a.txt")


def test_merge_combines_disjoint_branch_changes(branched):
    store, manager, root, sha = branched
    a = manager.write_text("ws-a", "a.txt", "from a\n", actor="a").after_revision
    b = manager.write_text("ws-b", "b.txt", "from b\n", actor="b").after_revision

    manager.merge("ws-join", a, actor="join")
    merged = manager.merge("ws-join", b, actor="join")
    assert merged.after_revision and merged.kind.value == "merge"

    backend = GitWorkspaceBackend(str(root))
    assert backend.read_text(merged.after_revision, "a.txt") == "from a\n"
    assert backend.read_text(merged.after_revision, "b.txt") == "from b\n"
    assert backend.read_text(merged.after_revision, "base.txt") == "base\n"

    kinds = [item.kind.value for item in store.list_workspace_operations("ws-join")]
    assert kinds == ["create", "merge", "merge"]


def test_conflicting_merge_fails_closed_and_leaves_the_target_unchanged(branched):
    store, manager, root, sha = branched
    manager.write_text("ws-a", "shared.txt", "from a\n", actor="a")
    a = store.get_workspace("ws-a").current_revision
    manager.write_text("ws-b", "shared.txt", "from b\n", actor="b")
    b = store.get_workspace("ws-b").current_revision

    first = manager.merge("ws-join", a, actor="join")
    before = store.get_workspace("ws-join").current_revision

    with pytest.raises(WorkspaceError, match="merge_conflict"):
        manager.merge("ws-join", b, actor="join")
    assert store.get_workspace("ws-join").current_revision == before
    assert GitWorkspaceBackend(str(root)).read_text(first.after_revision, "shared.txt") == "from a\n"


def test_join_behavior_merges_every_branch_revision(branched):
    store, manager, root, sha = branched
    manager.write_text("ws-a", "a.txt", "from a\n", actor="a")
    manager.write_text("ws-b", "b.txt", "from b\n", actor="b")
    a = store.get_workspace("ws-a").current_revision
    b = store.get_workspace("ws-b").current_revision

    behavior = JoinMergeBehavior(store, manager)
    snapshot = {"workspace": f"workspace://ws-join@{sha}", "branch_a": f"workspace://ws-a@{a}",
                "branch_b": f"workspace://ws-b@{b}"}
    result = behavior.execute_control(snapshot, store=store, artifacts=None,
                                      run_id=uuid4(), node_id="join")

    assert result["merged_sources"] == [a, b]
    final = parse(result["workspace"]).revision
    backend = GitWorkspaceBackend(str(root))
    assert backend.read_text(final, "a.txt") == "from a\n"
    assert backend.read_text(final, "b.txt") == "from b\n"


def test_join_without_branch_revisions_fails_closed(branched):
    store, manager, root, sha = branched
    behavior = JoinMergeBehavior(store, manager)
    with pytest.raises(WorkspaceError, match="no branch workspace revisions"):
        behavior.execute_control({"workspace": f"workspace://ws-join@{sha}"},
                                 store=store, artifacts=None, run_id=uuid4(), node_id="join")
