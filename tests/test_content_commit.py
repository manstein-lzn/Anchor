"""Fault injection for the prepare/freeze/commit/reconcile protocol.

Each test reproduces one crash window from CONTENT_COMMIT_PROTOCOL.md and asserts
that reconciliation converges without ever guessing content from a live
workspace. The two failure modes that must never happen are: a revision being
committed twice, and an unavailable revision being treated as present.
"""

import subprocess
from uuid import uuid4

import pytest

from anchor.domain.content_commit import PreparedRevision
from anchor.domain.project import Project
from anchor.runtime.content_commit import ContentCommitter
from anchor.runtime.workspace import GitWorkspaceBackend
from anchor.runtime.workspaces import WorkspaceManager


def make_repo(tmp_path):
    root = tmp_path / "repo"
    root.mkdir()
    (root / "readme.md").write_text("base\n", encoding="utf-8")
    for args in (["init", "-q"], ["config", "user.email", "t@example.com"],
                 ["config", "user.name", "Test"], ["add", "-A"], ["commit", "-qm", "init"]):
        subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True)
    sha = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"],
                         check=True, capture_output=True, text=True).stdout.strip()
    return root, sha


@pytest.fixture
def committed(tmp_path, store):
    root, sha = make_repo(tmp_path)
    store.create_project(Project(project_id="proj-1", name="P", root=str(root)))
    manager = WorkspaceManager(store, root=tmp_path / "worktrees")
    manager.create(project_id="proj-1", base_revision=sha, workspace_id="ws-1")
    manager.write_text("ws-1", "src/new.py", "print('x')\n", actor="agent")
    run_id, node_run_id = uuid4(), uuid4()
    committer = ContentCommitter(store, manager)
    return store, manager, committer, root, run_id, node_run_id


def commit_tools(store, run_id, node_run_id):
    calls: list[str] = []

    def commit_fn(prepared):
        calls.append(prepared.revision)
        store.append_event(
            stream_id=run_id, event_type="content.committed",
            payload={"revision": prepared.revision},
            idempotency_key=f"content:{node_run_id}:{prepared.attempt}:committed")

    def is_committed(prepared):
        return store.find_event(
            run_id, f"content:{node_run_id}:{prepared.attempt}:committed") is not None

    return commit_fn, is_committed, calls


def test_prepare_freezes_and_records_a_digest(committed):
    store, _, committer, root, run_id, node_run_id = committed
    prepared = committer.prepare(run_id=run_id, node_run_id=node_run_id,
                                 attempt=0, workspace_id="ws-1")
    assert prepared.revision
    assert prepared.verifier_result is None
    assert prepared.manifest_digest == GitWorkspaceBackend(str(root)).tree_digest(prepared.revision)
    assert store.get_prepared_revision(node_run_id, 0) == prepared
    assert store.get_workspace("ws-1").state.value == "frozen"

    # Idempotent: preparing again returns the same revision, no new freeze.
    again = committer.prepare(run_id=run_id, node_run_id=node_run_id, attempt=0,
                              workspace_id="ws-1")
    assert again == prepared
    assert len([item for item in store.list_workspace_operations("ws-1")
                if item.kind.value == "freeze"]) == 1


def test_w2_prepared_without_verification_is_verified_then_committed(committed):
    store, _, committer, _, run_id, node_run_id = committed
    committer.prepare(run_id=run_id, node_run_id=node_run_id, attempt=0, workspace_id="ws-1")
    commit_fn, is_committed, calls = commit_tools(store, run_id, node_run_id)

    first = committer.reconcile(is_committed=is_committed,
                                verify_fn=lambda prepared: {"verdict": "pass"},
                                commit_fn=commit_fn)
    assert [item.action for item in first] == ["verified"]
    assert store.get_prepared_revision(node_run_id, 0).verifier_result == {"verdict": "pass"}
    assert calls == [], "a prepared revision is not committed before verification"

    revision = store.get_prepared_revision(node_run_id, 0).revision
    second = committer.reconcile(is_committed=is_committed,
                                 verify_fn=lambda prepared: {"verdict": "pass"},
                                 commit_fn=commit_fn)
    assert [item.action for item in second] == ["committed"]
    assert calls == [revision]
    assert store.get_prepared_revision(node_run_id, 0) is None


def test_w3_prepared_and_verified_commits_once(committed):
    store, _, committer, _, run_id, node_run_id = committed
    prepared = committer.prepare(run_id=run_id, node_run_id=node_run_id, attempt=0,
                                 workspace_id="ws-1")
    committer.record_verification(prepared, {"verdict": "pass"})
    commit_fn, is_committed, calls = commit_tools(store, run_id, node_run_id)

    outcomes = committer.reconcile(is_committed=is_committed,
                                   verify_fn=lambda item: {"verdict": "pass"},
                                   commit_fn=commit_fn)
    assert [item.action for item in outcomes] == ["committed"]
    assert calls == [prepared.revision]
    assert store.get_prepared_revision(node_run_id, 0) is None


def test_w4_committed_event_with_stale_marker_is_cleared_not_recommitted(committed):
    store, _, committer, _, run_id, node_run_id = committed
    prepared = committer.prepare(run_id=run_id, node_run_id=node_run_id, attempt=0,
                                 workspace_id="ws-1")
    committer.record_verification(prepared, {"verdict": "pass"})
    commit_fn, is_committed, calls = commit_tools(store, run_id, node_run_id)
    commit_fn(prepared)  # crash after the control commit, before clearing the marker

    outcomes = committer.reconcile(is_committed=is_committed,
                                   verify_fn=lambda item: {"verdict": "pass"},
                                   commit_fn=commit_fn)
    assert [item.action for item in outcomes] == ["already_committed"]
    assert calls == [prepared.revision], "the control commit must not run twice"
    assert store.get_prepared_revision(node_run_id, 0) is None


def test_w5_committed_but_unavailable_content_is_inconsistent(committed):
    store, _, committer, _, run_id, node_run_id = committed
    store.record_prepared_revision(PreparedRevision(
        node_run_id=node_run_id, attempt=0, run_id=run_id, workspace_id="ws-1",
        revision="0" * 40, manifest_digest="a" * 64, verifier_result={"verdict": "pass"}))
    commit_fn, is_committed, calls = commit_tools(store, run_id, node_run_id)

    outcomes = committer.reconcile(is_committed=is_committed,
                                   verify_fn=lambda item: {"verdict": "pass"},
                                   commit_fn=commit_fn)
    assert [item.action for item in outcomes] == ["inconsistent"]
    assert calls == [], "an unavailable revision must never be committed"
    assert store.get_prepared_revision(node_run_id, 0) is not None


def test_w1_freeze_without_prepare_leaves_no_marker(committed):
    store, manager, _, _, _, _ = committed
    # A crash before the prepared record: the revision exists but nothing points
    # at it, so it is an orphan the garbage collector may reclaim.
    manager.freeze("ws-1", actor="crashed-node")
    assert store.list_prepared_revisions() == []
    assert store.list_workspace_operations("ws-1")[-1].kind.value == "freeze"


def test_concurrent_reconcilers_commit_once(committed):
    store, _, committer, _, run_id, node_run_id = committed
    prepared = committer.prepare(run_id=run_id, node_run_id=node_run_id, attempt=0,
                                 workspace_id="ws-1")
    committer.record_verification(prepared, {"verdict": "pass"})
    commit_fn, is_committed, calls = commit_tools(store, run_id, node_run_id)

    committer.reconcile(is_committed=is_committed,
                        verify_fn=lambda item: {"verdict": "pass"}, commit_fn=commit_fn)
    committer.reconcile(is_committed=is_committed,
                        verify_fn=lambda item: {"verdict": "pass"}, commit_fn=commit_fn)
    assert calls == [prepared.revision]
