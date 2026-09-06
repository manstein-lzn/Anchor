import pytest

from anchor.domain import (
    GraphDefinition,
    GraphEdge,
    GraphNode,
    GraphValidator,
    GraphVersion,
    NodeType,
    Trigger,
    TriggerType,
)
from anchor.state import GraphVersionConflict


def node(node_id: str, node_type: NodeType, **kwargs) -> GraphNode:
    return GraphNode(id=node_id, type=node_type, name=node_id, **kwargs)


def test_valid_graph_can_be_published_with_stable_hash():
    definition = GraphDefinition(
        graph_id="review",
        name="Review",
        nodes=[
            node("research", NodeType.AGENT, agent_ref="research-v1"),
            node("verify", NodeType.VERIFIER, verifier_ref="citations-v1"),
        ],
        edges=[GraphEdge(source="research", target="verify")],
    )

    result = GraphValidator().validate(definition)
    published = GraphVersion.publish(definition, version=1)

    assert result.valid
    assert published.graph_id == "review"
    assert len(published.content_hash) == 64
    assert published.content_hash == GraphVersion.publish(definition, version=1).content_hash
    assert definition.resolved_entry_node_id() == "research"


def test_entry_resolution_refuses_ambiguous_drafts():
    definition = GraphDefinition(
        graph_id="ambiguous", name="Ambiguous",
        nodes=[node("one", NodeType.ARTIFACT), node("two", NodeType.ARTIFACT)],
    )
    with pytest.raises(ValueError, match="exactly one"):
        definition.resolved_entry_node_id()


def test_validator_rejects_unreachable_nodes_and_cycles():
    definition = GraphDefinition(
        graph_id="invalid",
        name="Invalid",
        nodes=[
            node("a", NodeType.AGENT, agent_ref="a"),
            node("b", NodeType.AGENT, agent_ref="b"),
            node("orphan", NodeType.TOOL, tool_ref="orphan"),
        ],
        edges=[
            GraphEdge(source="a", target="b"),
            GraphEdge(source="b", target="a"),
        ],
    )

    result = GraphValidator().validate(definition)
    codes = {issue.code for issue in result.issues}

    assert not result.valid
    assert {"cycle", "unreachable_node"}.issubset(codes)
    with pytest.raises(ValueError, match="cannot be published"):
        GraphVersion.publish(definition, version=1)


def test_explicit_loop_is_allowed_without_numeric_limits():
    definition = GraphDefinition(
        graph_id="loop",
        name="Loop",
        nodes=[
            node("work", NodeType.AGENT, agent_ref="worker"),
            node("loop", NodeType.LOOP, exit_condition="state.done == true", progress_signal="new artifact or changed state"),
            node("done", NodeType.ARTIFACT),
        ],
        edges=[
            GraphEdge(source="work", target="loop"),
            GraphEdge(source="loop", target="work", condition="state.done == false"),
            GraphEdge(source="loop", target="done", condition="state.done == true"),
        ],
        entry_node_id="work",
    )

    assert GraphValidator().validate(definition).valid
    assert GraphVersion.publish(definition, version=1).definition.nodes[1].exit_condition


def test_trigger_requires_type_specific_configuration():
    with pytest.raises(ValueError, match="event_type"):
        Trigger(graph_version_id="00000000-0000-0000-0000-000000000001", type=TriggerType.WEBHOOK)


def test_loop_needs_no_numeric_limit_or_custom_progress_signal():
    loop = node("repeat", NodeType.LOOP, exit_condition="verified")
    assert loop.progress_signal is None


def test_upstream_loop_does_not_authorize_an_uncontrolled_inner_cycle():
    definition = GraphDefinition(
        graph_id="inner-cycle", name="Inner cycle", entry_node_id="a_loop",
        nodes=[node("a_loop", NodeType.LOOP, exit_condition="verified"),
               node("b", NodeType.AGENT, agent_ref="worker"),
               node("c", NodeType.AGENT, agent_ref="reviewer"),
               node("done", NodeType.ARTIFACT)],
        edges=[GraphEdge(source="a_loop", target="b"), GraphEdge(source="b", target="c"),
               GraphEdge(source="c", target="b"), GraphEdge(source="a_loop", target="done")],
    )
    assert "cycle" in {issue.code for issue in GraphValidator().validate(definition).issues}


def test_loop_without_exit_path_is_rejected_even_if_other_branch_finishes():
    definition = GraphDefinition(
        graph_id="no-exit", name="No exit",
        nodes=[node("start", NodeType.AGENT, agent_ref="worker"),
               node("loop", NodeType.LOOP, exit_condition="verified"),
               node("done", NodeType.ARTIFACT)],
        edges=[GraphEdge(source="start", target="loop"), GraphEdge(source="loop", target="loop"),
               GraphEdge(source="start", target="done")],
    )
    assert "no_exit_path" in {issue.code for issue in GraphValidator().validate(definition).issues}


def test_published_graph_and_trigger_round_trip(store):
    definition = GraphDefinition(
        graph_id="persisted",
        name="Persisted",
        nodes=[node("agent", NodeType.AGENT, agent_ref="agent-v1")],
    )
    version = store.publish_graph(GraphVersion.publish(definition, version=1))
    trigger = store.create_trigger(
        Trigger(graph_version_id=version.graph_version_id, type=TriggerType.MANUAL)
    )

    loaded = store.get_graph_version(version.graph_version_id)
    assert loaded is not None
    assert loaded.content_hash == version.content_hash
    assert store.list_triggers(version.graph_version_id) == [trigger]


def test_same_graph_version_cannot_be_reused_for_different_content(store):
    first = GraphVersion.publish(
        GraphDefinition(
            graph_id="conflict",
            name="Conflict",
            nodes=[node("agent", NodeType.AGENT, agent_ref="one")],
        ),
        version=1,
    )
    store.publish_graph(first)
    second = GraphVersion.publish(
        GraphDefinition(
            graph_id="conflict",
            name="Conflict",
            nodes=[node("agent", NodeType.AGENT, agent_ref="two")],
        ),
        version=1,
    )

    with pytest.raises(GraphVersionConflict):
        store.publish_graph(second)
