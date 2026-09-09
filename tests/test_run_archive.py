"""Operator management of the run list: archive, never delete.

Runs are the root of the evidence chain. Operators need to clear the active
list without losing evidence, so archiving is a reversible flag that only
applies to terminal runs; every artifact, event, decision and operation stays
queryable by run id.
"""

from uuid import uuid4

import pytest

from anchor.domain.admission import RunRequest
from anchor.domain.graph import GraphDefinition, GraphNode, GraphVersion, Trigger
from anchor.state.errors import ConcurrencyConflict
from conftest import make_store


def seeded(tmp_path, name="archive.sqlite", graph_id="archive-graph"):
    store = make_store(tmp_path, name)
    version = store.publish_graph(GraphVersion.publish(GraphDefinition(
        graph_id=graph_id, name="Archive",
        nodes=[GraphNode(id="a", type="agent", name="A", agent_ref="a")]), 1))
    trigger = store.create_trigger(Trigger(graph_version_id=version.graph_version_id,
                                           type="manual"))
    receipt = store.admit_run(RunRequest(trigger_id=trigger.id,
                                         idempotency_key=f"archive-{uuid4().hex}",
                                         objective="archive me", inputs={}))
    return store, receipt


def test_archiving_hides_a_terminal_run_and_keeps_all_evidence(tmp_path):
    store, receipt = seeded(tmp_path)
    try:
        run = store.get_run(receipt.run_id)
        # An active run must stay visible to supervision.
        with pytest.raises(ConcurrencyConflict, match="terminal run"):
            store.set_run_archived(run.id, archived=True)

        store.stop_run(run.id, reason="operator stop")
        archived = store.set_run_archived(run.id, archived=True)
        assert archived.archived_at is not None
        assert store.list_runs() == []
        assert [item.id for item in store.list_runs(include_archived=True)] == [run.id]

        # Evidence is never destroyed: the run and its event stream stay queryable.
        assert store.get_run(run.id).id == run.id
        event_types = [event["event_type"] for event in store.list_events(run.id)]
        assert "run.requested" in event_types and "run.archived" in event_types

        # Idempotent in the same direction, reversible in the other.
        again = store.set_run_archived(run.id, archived=True)
        assert again.revision == archived.revision
        restored = store.set_run_archived(run.id, archived=False)
        assert restored.archived_at is None
        assert [item.id for item in store.list_runs()] == [run.id]
    finally:
        store.close()


def test_run_list_filters_by_status_and_keeps_archived_out(tmp_path):
    store, first = seeded(tmp_path, name="filters.sqlite")
    try:
        second = store.admit_run(RunRequest(trigger_id=store.list_triggers(
            store.get_run(first.run_id).graph_version_id)[0].id,
            idempotency_key=f"archive-{uuid4().hex}", objective="second", inputs={}))
        store.stop_run(first.run_id, reason="stop first")

        assert [item.id for item in store.list_runs(statuses=["cancelled"])] == [first.run_id]
        assert [item.id for item in store.list_runs(statuses=["created"])] == [second.run_id]
        assert len(store.list_runs(statuses=["cancelled", "created"])) == 2

        store.set_run_archived(first.run_id, archived=True)
        assert [item.id for item in store.list_runs(statuses=["cancelled"])] == []
        assert [item.id for item in store.list_runs(statuses=["cancelled"],
                                                    include_archived=True)] == [first.run_id]
    finally:
        store.close()
