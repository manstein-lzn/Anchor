from uuid import uuid4

from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphVersion
from anchor.domain.models import EdgeDecisionReason, NodeRun, NodeRunStatus
from anchor.runtime.propagation import decide_outgoing_edges, plan_propagation, plan_ready_nodes


def version():
    graph = GraphDefinition(graph_id="g", name="g", nodes=[
        GraphNode(id="a", type="agent", name="a", agent_ref="a"),
        GraphNode(id="b", type="agent", name="b", agent_ref="b"),
        GraphNode(id="c", type="agent", name="c", agent_ref="c"),
    ], edges=[GraphEdge(source="a", target="b"), GraphEdge(source="a", target="c")])
    return GraphVersion.publish(graph, 1)


def test_planner_returns_only_pending_nodes_with_completed_predecessors():
    v = version(); run = uuid4()
    nodes = [NodeRun(run_id=run, node_id="a", status=NodeRunStatus.COMPLETED),
             NodeRun(run_id=run, node_id="b"), NodeRun(run_id=run, node_id="c", status=NodeRunStatus.RUNNING)]
    assert plan_ready_nodes(v, nodes) == ("b",)


def test_planner_does_not_execute_conditional_edges():
    v = version(); v.definition.edges[0].condition = "approved"
    run = uuid4()
    nodes = [NodeRun(run_id=run, node_id="a", status=NodeRunStatus.COMPLETED), NodeRun(run_id=run, node_id="b"), NodeRun(run_id=run, node_id="c")]
    assert plan_ready_nodes(v, nodes) == ("c",)


def conditional_version():
    graph = GraphDefinition(
        graph_id="conditional",
        name="conditional",
        nodes=[
            GraphNode(id="route", type="agent", name="route", agent_ref="route"),
            GraphNode(id="left", type="agent", name="left", agent_ref="left"),
            GraphNode(id="right", type="agent", name="right", agent_ref="right"),
            GraphNode(id="join", type="agent", name="join", agent_ref="join"),
        ],
        edges=[
            GraphEdge(source="route", target="left", condition="output.approved"),
            GraphEdge(source="route", target="right", condition="output.approved == `false`"),
            GraphEdge(source="left", target="join"),
            GraphEdge(source="right", target="join"),
        ],
    )
    return GraphVersion.publish(graph, 1)


def test_conditional_branch_skips_rejected_path_and_waits_at_join():
    v = conditional_version()
    run = uuid4()
    nodes = [
        NodeRun(run_id=run, node_id="route", status=NodeRunStatus.COMPLETED),
        NodeRun(run_id=run, node_id="left"),
        NodeRun(run_id=run, node_id="right"),
        NodeRun(run_id=run, node_id="join"),
    ]
    decisions = decide_outgoing_edges(
        v,
        run_id=run,
        source_node_id="route",
        evaluation_context={"output": {"approved": True}, "inputs": {}},
        evidence_ref="artifact://route",
    )
    plan = plan_propagation(v, nodes, decisions)
    assert plan.ready_node_ids == ("left",)
    assert plan.skipped_node_ids == ("right",)
    assert [(item.edge_index, item.selected, item.reason) for item in plan.inferred_decisions] == [
        (3, False, EdgeDecisionReason.UPSTREAM_SKIPPED),
    ]


def test_join_becomes_ready_after_selected_branch_completes():
    v = conditional_version()
    run = uuid4()
    nodes = [
        NodeRun(run_id=run, node_id="route", status=NodeRunStatus.COMPLETED),
        NodeRun(run_id=run, node_id="left", status=NodeRunStatus.COMPLETED),
        NodeRun(run_id=run, node_id="right", status=NodeRunStatus.SKIPPED),
        NodeRun(run_id=run, node_id="join"),
    ]
    route = decide_outgoing_edges(
        v,
        run_id=run,
        source_node_id="route",
        evaluation_context={"output": {"approved": True}, "inputs": {}},
        evidence_ref="artifact://route",
    )
    first = plan_propagation(v, nodes, route)
    left = decide_outgoing_edges(
        v,
        run_id=run,
        source_node_id="left",
        evaluation_context=None,
        evidence_ref="artifact://left",
    )
    plan = plan_propagation(v, nodes, (*route, *first.inferred_decisions, *left))
    assert plan.ready_node_ids == ("join",)
    assert plan.skipped_node_ids == ()
