from pathlib import Path

import pytest

from anchor.runtime.artifacts import LocalArtifactStore


def test_local_artifacts_are_content_addressed_and_verified(tmp_path: Path):
    store = LocalArtifactStore(tmp_path / "artifacts")
    ref = store.put_text("hello")
    assert ref.startswith("artifact://sha256/")
    assert store.get_text(ref) == "hello"
    assert store.put_text("hello") == ref


def test_local_artifacts_reject_invalid_refs(tmp_path: Path):
    store = LocalArtifactStore(tmp_path)
    with pytest.raises(ValueError):
        store.get_text("artifact://sha256/nope")

