"""Following a reference, and the permission that makes following it safe.

Compression replaces old messages with a state that names its detail by reference. That promise is
only kept if something can follow the reference, and only safe if following it cannot reach content
the node was never given. Both halves are here: the refusal is as much the feature as the read.
"""

from __future__ import annotations

from uuid import uuid4

import pytest

from anchor.runtime.artifacts import LocalArtifactStore
from anchor.runtime.recall import DEFAULT_CHARS, reference_of, referenced_by_run, resolve



class Store:
    """Only the reachability listing, which is all `recall` asks the store for."""

    def __init__(self, pairs):
        self._pairs = list(pairs)

    def list_artifact_references(self):
        return self._pairs


def run_with(*refs):
    run_id = uuid4()
    return run_id, Store([(run_id, ref) for ref in refs])


def test_a_reference_the_run_holds_is_read(tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    ref = artifacts.put_text("the stored detail")
    run_id, store = run_with(ref)
    assert resolve(store, artifacts, run_id=run_id, reference=ref) == "the stored detail"


def test_a_reference_the_run_does_not_hold_is_refused(tmp_path):
    """The whole permission model. A recall that resolved anything would be a way to read content
    this node was never given — the hidden visibility the declared-input rule exists to prevent.
    """
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    hidden = artifacts.put_text("another run's business")
    run_id, store = run_with()
    answer = resolve(store, artifacts, run_id=run_id, reference=hidden)
    assert answer.startswith("REFUSED")
    assert "does not reference" in answer
    assert "another run's business" not in answer


def test_one_runs_reference_is_not_another_runs_to_read(tmp_path):
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    ref = artifacts.put_text("shared-looking content")
    owner, _ = run_with(ref)
    stranger = uuid4()
    store = Store([(owner, ref)])
    assert resolve(store, artifacts, run_id=stranger, reference=ref).startswith("REFUSED")


def test_a_permission_set_is_the_runs_own_references():
    run_id, store = run_with("artifact://sha256/" + "a" * 64)
    assert referenced_by_run(store, run_id) == {"artifact://sha256/" + "a" * 64}
    assert referenced_by_run(store, uuid4()) == set()


def test_an_unlistable_store_reads_nothing_rather_than_everything():
    """Failing closed. A store that cannot say what a run holds has not authorised anything."""
    class Broken:
        def list_artifact_references(self):
            raise RuntimeError("the database is unreachable")

    assert referenced_by_run(Broken(), uuid4()) == set()


def test_a_reference_that_resolves_but_cannot_be_read_is_distinguished_from_a_refusal(tmp_path):
    """One says the run does not hold this; the other says it does and the bytes are gone. A caller
    deciding what to do next needs to tell them apart."""
    class Vanishing:
        def get_text(self, ref):
            raise FileNotFoundError(ref)

    ref = "artifact://sha256/" + "b" * 64
    run_id, store = run_with(ref)
    answer = resolve(store, Vanishing(), run_id=run_id, reference=ref)
    assert answer.startswith("UNAVAILABLE")
    assert "cannot be read" in answer


def test_a_ledger_reference_is_refused_as_not_being_content(tmp_path):
    """Operation and verifier references are answers about the ledger. Saying so is more useful
    than a silent miss, because the caller may have meant to ask something else."""
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    run_id, store = run_with()
    answer = resolve(store, artifacts, run_id=run_id, reference="operation:abc")
    assert answer.startswith("REFUSED")
    assert "not a content reference" in answer


def test_a_long_artifact_is_bounded_and_says_so(tmp_path):
    """Bounded because recall is called from inside a loop whose growth is the thing being managed:
    an unbounded answer would put back on one call what compression just removed. Truncated rather
    than refused, because the reference did resolve."""
    artifacts = LocalArtifactStore(tmp_path / "artifacts")
    ref = artifacts.put_text("x" * (DEFAULT_CHARS + 500))
    run_id, store = run_with(ref)
    answer = resolve(store, artifacts, run_id=run_id, reference=ref)
    assert len(answer) < DEFAULT_CHARS + 400
    assert "truncated" in answer and ref in answer


def test_an_empty_reference_is_refused():
    artifacts = LocalArtifactStore.__new__(LocalArtifactStore)
    assert resolve(Store([]), artifacts, run_id=uuid4(), reference="").startswith("REFUSED")


@pytest.mark.parametrize("raw,expected", [
    ('{"ref": "artifact://sha256/a"}', "artifact://sha256/a"),
    ('{"locator": "artifact://sha256/b"}', "artifact://sha256/b"),
    ('{"reference": "artifact://sha256/c"}', "artifact://sha256/c"),
    ("artifact://sha256/d", "artifact://sha256/d"),
    ("", None),
])
def test_the_question_is_read_from_either_shape(raw, expected):
    """A model told the tool takes a reference will sometimes pass the bare string, and sometimes
    the object. Both are the same question."""
    assert reference_of(raw) == expected


def test_recall_is_offered_only_when_compression_is(tmp_path):
    """Recall exists to follow the references a state leaves behind. Without compression nothing
    writes one, so offering it would add a tool to every agent's surface in exchange for answering
    questions nobody can ask."""
    import inspect

    from anchor.runtime.agent_tools import AgentToolLoop

    signature = inspect.signature(AgentToolLoop.__init__)
    assert signature.parameters["recall"].default is False
    source = inspect.getsource(AgentToolLoop._functions)
    assert "if self.recall else []" in source
