"""Read-only workspace resolution: a project is readable, never mutable.

Every test asserts the fail-closed rule from I2: an unresolvable workspace
reference raises instead of falling back to a working tree, and registration
refuses a root that is not a usable read-only git repository.
"""

import subprocess

import pytest

from anchor.domain.content import ContentRefError, parse
from anchor.domain.project import Project
from anchor.runtime.content import ContentUnavailable
from anchor.runtime.workspace import (
    GitWorkspaceBackend,
    WorkspaceError,
    WorkspaceResolver,
    validate_project_root,
)


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
def workspace(tmp_path, store):
    root, sha = make_repo(tmp_path)
    store.create_project(Project(project_id="proj-1", name="Project one", root=str(root)))
    return store, WorkspaceResolver(store), root, sha


def test_project_registration_validates_the_root(tmp_path):
    root, _ = make_repo(tmp_path)
    validate_project_root(str(root))  # does not raise

    not_a_repo = tmp_path / "plain"
    not_a_repo.mkdir()
    with pytest.raises(WorkspaceError, match="not a git repository"):
        validate_project_root(str(not_a_repo))
    with pytest.raises(WorkspaceError, match="not a directory"):
        validate_project_root(str(tmp_path / "missing"))
    with pytest.raises(WorkspaceError, match="unsupported project backend"):
        validate_project_root(str(root), backend="svn")


def test_workspace_resolver_reads_a_file_at_an_immutable_revision(workspace):
    _, resolver, _, sha = workspace
    ref = parse(f"workspace://proj-1@{sha}/readme.md")
    assert resolver.exists(ref) is True
    assert resolver.read_text(ref) == "hello workspace\n"
    nested = parse(f"workspace://proj-1@{sha}/src/app.py")
    assert resolver.read_text(nested) == "print('hi')\n"


def test_backend_lists_paths_at_a_revision(workspace):
    _, _, root, sha = workspace
    backend = GitWorkspaceBackend(str(root))
    paths = backend.list_paths(sha)
    assert "readme.md" in paths and "src/app.py" in paths
    assert backend.list_paths(sha, prefix="src/") == ["src/app.py"]


@pytest.mark.parametrize("text, reason", [
    ("workspace://missing@{sha}/readme.md", "unknown workspace"),
    ("workspace://proj-1@{sha}/nope.md", "not present"),
    ("workspace://proj-1@{sha}/src", "not a file"),
])
def test_unresolvable_references_fail_closed(workspace, text, reason):
    _, resolver, _, sha = workspace
    with pytest.raises(ContentUnavailable, match=reason):
        resolver.read_text(parse(text.format(sha=sha)))


def test_unknown_revision_fails_closed(workspace):
    _, resolver, _, _ = workspace
    unknown = "0" * 40
    with pytest.raises(ContentUnavailable):
        resolver.read_text(parse(f"workspace://proj-1@{unknown}/readme.md"))


def test_mutable_revision_is_refused_before_any_read(workspace):
    _, _, _, _ = workspace
    with pytest.raises(ContentRefError):
        parse("workspace://proj-1@main/readme.md")


def test_reading_a_tree_without_a_path_fails_closed(workspace):
    _, resolver, _, sha = workspace
    with pytest.raises(ContentUnavailable, match="name a path"):
        resolver.read_text(parse(f"workspace://proj-1@{sha}"))


def test_oversized_file_fails_closed(workspace):
    store, _, root, sha = workspace
    resolver = WorkspaceResolver(store, max_bytes=4)
    with pytest.raises(ContentUnavailable, match="over the 4 limit"):
        resolver.read_text(parse(f"workspace://proj-1@{sha}/readme.md"))
