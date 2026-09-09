"""The content-reference boundary: parsing, immutability and fail-closed reads.

I2 and I9 depend on a reference meaning the same bytes forever, so the tests
below pin three properties: round-trip parsing, rejection of mutable revisions
and paths, and refusal to invent content when a reference cannot be resolved.
"""

import pytest

from anchor.domain.content import (
    ContentRef,
    ContentRefError,
    ContentKind,
    artifact_ref,
    parse,
    workspace_ref,
)
from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.content import ArtifactResolver, ContentRegistry, ContentUnavailable

DIGEST = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
GIT_SHA = "a9fe31b7c0d4e5f6a1b2c3d4e5f60718293a4b5c"


def test_artifact_reference_round_trips():
    ref = parse(f"artifact://sha256/{DIGEST}")
    assert ref.kind is ContentKind.ARTIFACT
    assert ref.is_artifact and not ref.is_workspace
    assert str(ref) == f"artifact://sha256/{DIGEST}"
    assert parse(str(ref)) == ref
    assert artifact_ref(DIGEST) == ref


def test_workspace_reference_round_trips_with_and_without_path():
    tree = parse(f"workspace://ws-1@{GIT_SHA}")
    assert tree.is_workspace and tree.path is None
    assert str(tree) == f"workspace://ws-1@{GIT_SHA}"

    file_ref = parse(f"workspace://ws-1@{GIT_SHA}/src/anchor/cli.py")
    assert file_ref.path == "src/anchor/cli.py"
    assert str(file_ref) == f"workspace://ws-1@{GIT_SHA}/src/anchor/cli.py"
    assert parse(str(file_ref)) == file_ref
    assert workspace_ref("ws-1", GIT_SHA, "src/anchor/cli.py") == file_ref


@pytest.mark.parametrize("revision", ["main", "HEAD", "latest", "refs/heads/main", "dev"])
def test_mutable_revisions_are_rejected(revision):
    with pytest.raises(ContentRefError, match="mutable|immutable"):
        parse(f"workspace://ws-1@{revision}")


@pytest.mark.parametrize("text", [
    "workspace://ws-1@abc",                 # too short to be an immutable id
    "workspace://ws-1",                     # missing revision
    "workspace://ws-1@",                    # empty revision
    "workspace://ws-1@a9fe31/../etc/passwd",  # traversal
    "workspace://ws-1@a9fe31//etc/passwd",    # empty segment
    f"artifact://sha256/{DIGEST[:-1]}",       # short digest
    f"artifact://sha256/{DIGEST.upper()}",    # not lowercase hex
    "artifact://md5/abc",
    "file:///etc/passwd",
    "",
])
def test_malformed_references_are_rejected(text):
    with pytest.raises(ContentRefError):
        parse(text)


def test_artifact_and_workspace_fields_are_mutually_exclusive():
    with pytest.raises(ContentRefError):
        ContentRef(kind=ContentKind.ARTIFACT, artifact_digest=DIGEST, workspace_id="ws-1")
    with pytest.raises(ContentRefError):
        ContentRef(kind=ContentKind.WORKSPACE, workspace_id="ws-1", revision=GIT_SHA,
                   artifact_digest=DIGEST)


def test_artifact_resolver_reads_and_reports_missing(tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    ref = artifact_ref(artifacts.put_text("hello").rsplit("/", 1)[-1])
    resolver = ArtifactResolver(artifacts)
    assert resolver.exists(ref) is True
    assert resolver.read_text(ref) == "hello"

    missing = artifact_ref("0" * 64)
    assert resolver.exists(missing) is False
    with pytest.raises(ContentUnavailable):
        resolver.read_text(missing)


def test_unregistered_kind_fails_closed_instead_of_guessing(tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    registry = ContentRegistry([ArtifactResolver(artifacts)])
    ref = workspace_ref("ws-1", GIT_SHA, "src/a.py")
    assert registry.exists(ref) is False
    with pytest.raises(ContentUnavailable, match="no workspace resolver"):
        registry.read_text(ref)


def test_require_all_fails_closed_on_the_first_missing_reference(tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    present = artifact_ref(artifacts.put_text("present").rsplit("/", 1)[-1])
    missing = artifact_ref("1" * 64)
    registry = ContentRegistry([ArtifactResolver(artifacts)])
    assert registry.require_all([present]) == {str(present): "present"}
    with pytest.raises(ContentUnavailable):
        registry.require_all([present, missing])
