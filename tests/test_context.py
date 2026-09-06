import pytest

from anchor.domain.graph import GraphEdge
from anchor.runtime.context import build_input_snapshot, canonical_json, input_hash


def test_snapshot_mapping_is_canonical_and_hashed():
    edges = [GraphEdge(source="a", target="b", input_mapping={"query": "inputs.query", "result": "outputs.a"})]
    first = build_input_snapshot(run_inputs={"query": "hello"}, edges=edges, predecessor_outputs={"a": "report"})
    second = build_input_snapshot(run_inputs={"query": "hello"}, edges=edges, predecessor_outputs={"a": "report"})
    assert first == {"query": "hello", "result": "report"}
    assert input_hash(first) == input_hash(second)
    assert canonical_json({"b": 1, "a": 2}) == '{"a":2,"b":1}'


def test_missing_mapping_path_fails_closed():
    edges = [GraphEdge(source="a", target="b", input_mapping={"query": "inputs.missing"})]
    with pytest.raises(KeyError, match="missing"):
        build_input_snapshot(run_inputs={}, edges=edges)


def test_multiple_predecessor_edges_merge_deterministically():
    edges = [
        GraphEdge(source="b", target="join", input_mapping={"second": "outputs.b"}),
        GraphEdge(source="a", target="join", input_mapping={"first": "outputs.a"}),
    ]
    snapshot = build_input_snapshot(run_inputs={}, edges=edges,
                                    predecessor_outputs={"a": "alpha", "b": "beta"})
    assert snapshot == {"first": "alpha", "second": "beta"}


def test_conflicting_mapping_targets_fail_closed():
    edges = [
        GraphEdge(source="a", target="join", input_mapping={"value": "outputs.a"}),
        GraphEdge(source="b", target="join", input_mapping={"value": "outputs.b"}),
    ]
    with pytest.raises(ValueError, match="conflicting input mapping"):
        build_input_snapshot(run_inputs={}, edges=edges,
                             predecessor_outputs={"a": "alpha", "b": "beta"})


def test_selected_conditional_edge_contributes_mapping():
    edges = [GraphEdge(source="a", target="b", condition="approved",
                       input_mapping={"result": "outputs.a"})]
    assert build_input_snapshot(run_inputs={"query": "hi"}, edges=edges,
                                predecessor_outputs={"a": "report"}) == {"result": "report"}


def test_edges_without_mappings_fall_back_to_inputs():
    edges = [GraphEdge(source="a", target="b")]
    assert build_input_snapshot(run_inputs={"query": "hi"}, edges=edges,
                                predecessor_outputs={"a": "report"}) == {"inputs": {"query": "hi"}}
