import pytest

from anchor.domain.conditions import (
    ConditionError,
    build_condition_context,
    evaluate_condition,
    validate_condition,
)
from anchor.domain.graph import GraphDefinition, GraphEdge, GraphNode, GraphValidator


def test_condition_context_parses_json_and_evaluates_strict_boolean():
    context = build_condition_context('{"approved": true, "score": 9}', {"minimum": 7})
    assert context == {
        "output": {"approved": True, "score": 9},
        "inputs": {"minimum": 7},
    }
    assert evaluate_condition("output.approved && output.score >= inputs.minimum", context)


def test_condition_context_preserves_plain_text_output():
    context = build_condition_context("approved")
    assert context == {"output": "approved", "inputs": {}}
    assert evaluate_condition("output == 'approved'", context)


def test_condition_rejects_blank_invalid_and_non_boolean_results():
    with pytest.raises(ConditionError, match="blank"):
        validate_condition("  ")
    with pytest.raises(ConditionError, match="invalid JMESPath"):
        validate_condition("output[")
    with pytest.raises(ConditionError, match="must evaluate to a boolean"):
        evaluate_condition("output.score", {"output": {"score": 9}})


def test_graph_validation_rejects_invalid_condition_syntax():
    graph = GraphDefinition(
        graph_id="condition-validation",
        name="Condition validation",
        nodes=[
            GraphNode(id="a", type="agent", name="A", agent_ref="a"),
            GraphNode(id="b", type="agent", name="B", agent_ref="b"),
        ],
        edges=[GraphEdge(source="a", target="b", condition="output[")],
    )
    result = GraphValidator().validate(graph)
    assert not result.valid
    assert [(issue.code, issue.edge_index) for issue in result.issues] == [
        ("invalid_edge_condition", 0),
    ]
