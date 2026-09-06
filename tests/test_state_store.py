import asyncio
from uuid import uuid4

import pytest

from anchor.domain import GraphDefinition, GraphNode, GraphVersion, NodeType
from anchor.domain.models import Run, Task
from anchor.state import ConcurrencyConflict, DuplicateEvent
from anchor.runtime import DeterministicHarness, InProcessWorkflowService
from anchor.domain.models import RunStatus


def test_event_sequence_is_monotonic_and_duplicate_delivery_is_idempotent(store):
    stream_id = uuid4()

    first = store.append_event(
        stream_id=stream_id,
        event_type="task.created",
        payload={"objective": "test"},
        idempotency_key="evt-1",
    )
    duplicate = store.append_event(
        stream_id=stream_id,
        event_type="task.created",
        payload={"objective": "test"},
        idempotency_key="evt-1",
    )
    second = store.append_event(
        stream_id=stream_id,
        event_type="task.started",
        payload={},
        idempotency_key="evt-2",
    )

    assert (first, duplicate, second) == (1, 1, 2)
    assert [event["sequence"] for event in store.list_events(stream_id)] == [1, 2]


def test_reusing_key_for_different_event_fails_closed(store):
    stream_id = uuid4()
    store.append_event(stream_id=stream_id, event_type="a", payload={"x": 1}, idempotency_key="same")

    with pytest.raises(DuplicateEvent):
        store.append_event(stream_id=stream_id, event_type="b", payload={"x": 2}, idempotency_key="same")


def test_task_and_run_round_trip(store):
    task = store.create_task(Task(objective="round trip"))
    graph = GraphVersion.publish(
        GraphDefinition(
            graph_id="round-trip",
            name="Round trip",
            nodes=[GraphNode(id="agent", type=NodeType.AGENT, name="agent", agent_ref="test")],
        ),
        version=1,
    )
    store.publish_graph(graph)
    run = store.create_run(Run(task_id=task.id, graph_version_id=graph.graph_version_id))

    assert store.get_task(task.id) == task
    assert store.get_run(run.id) == run


def test_local_vertical_slice_verifies_and_commits(store):
    task = store.create_task(Task(objective="complete locally"))
    graph = GraphVersion.publish(
        GraphDefinition(
            graph_id="local",
            name="Local",
            nodes=[GraphNode(id="agent", type=NodeType.AGENT, name="agent", agent_ref="test")],
        ),
        version=1,
    )
    store.publish_graph(graph)
    service = InProcessWorkflowService(store, DeterministicHarness())

    run_id = asyncio.run(service.start(task.id, graph.graph_version_id))
    run = store.get_run(run_id)

    assert run is not None
    assert run.status is RunStatus.COMPLETED
    assert [event["event_type"] for event in store.list_events(run_id)] == [
        "run.running",
        "run.completed",
    ]


def test_stale_run_transition_fails_without_appending_event(store):
    task = store.create_task(Task(objective="concurrency"))
    graph = GraphVersion.publish(
        GraphDefinition(
            graph_id="concurrency",
            name="Concurrency",
            nodes=[GraphNode(id="agent", type=NodeType.AGENT, name="agent", agent_ref="test")],
        ),
        version=1,
    )
    store.publish_graph(graph)
    run = store.create_run(Run(task_id=task.id, graph_version_id=graph.graph_version_id))

    store.transition_run(
        run_id=run.id,
        expected_revision=0,
        status=RunStatus.RUNNING,
        phase="execute",
        payload={},
        idempotency_key="run-start",
    )

    with pytest.raises(ConcurrencyConflict):
        store.transition_run(
            run_id=run.id,
            expected_revision=0,
            status=RunStatus.COMPLETED,
            phase="complete",
            payload={},
            idempotency_key="run-complete-stale",
        )

    assert len(store.list_events(run.id)) == 1
