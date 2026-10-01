"""Paired parallel controls preserve a single graph and validate their complete boundary."""
from dataclasses import FrozenInstanceError
from itertools import combinations

import pytest

from anchor.simple import graph as G


def region():
    return {
        "agents": {"worker": {"model": "test", "writes": ["evidence.md"]}},
        "ops": {"split": {"fanout": {"join": "collect"}}, "join": {"join": {}},
                "command": {"run": "true"},
                "call": {"call": {"graph": "child", "mode": "wait"}}},
        "entry": "split",
        "nodes": [{"id": "split", "op": "split"}, {"id": "left", "agent": "worker"},
                  {"id": "left-tail", "op": "command"}, {"id": "right", "op": "call"},
                  {"id": "collect", "op": "join"}, {"id": "after", "agent": "worker"}],
        "edges": [{"from": source, "to": target} for source, target in [
            ("split", "left"), ("split", "right"), ("left", "left-tail"),
            ("left-tail", "collect"), ("right", "collect"), ("collect", "after")]],
    }


def add_edge(raw, source, target):
    raw["edges"].append({"from": source, "to": target})


def remove_edge(raw, source, target):
    raw["edges"].remove({"from": source, "to": target})


def test_parallel_region_contains_ordered_mixed_linear_branches():
    raw = region()
    parsed = G.parse(raw)
    expected = G.ParallelRegion("split", "collect", (("left", "left-tail"), ("right",)))
    assert parsed.parallel_regions() == {"split": expected}
    assert parsed.definition("split").fanout == {"join": "collect"}
    assert parsed.definition("collect").join == {}
    assert parsed.writes("collect") == ("join.json",)
    assert G.parse(G.to_dict(parsed)) == parsed
    with pytest.raises(FrozenInstanceError):
        expected.join = "elsewhere"
    raw["ops"]["split"]["fanout"]["join"] = "elsewhere"
    assert parsed.parallel_regions()["split"] == expected


def test_join_may_end_the_graph_and_exposes_branch_evidence_to_downstream():
    raw = region()
    raw["agents"]["summary"] = {"model": "test", "reads": ["evidence.md", "join.json"]}
    raw["nodes"][-1]["agent"] = "summary"
    parsed = G.parse(raw)
    assert {"left", "left-tail", "right", "collect"} <= G.feeders(parsed, "after")
    raw["nodes"].pop()
    remove_edge(raw, "collect", "after")
    assert G.parse(raw).routes("collect") == ()


def test_outer_choices_and_whole_region_feedback_remain_legal():
    raw = region()
    raw["nodes"].append({"id": "done", "op": "command"})
    add_edge(raw, "after", "split")
    add_edge(raw, "after", "done")
    assert G.parse(raw).routes("after") == ("split", "done")


def test_join_can_loop_directly_to_its_fanout():
    raw = region()
    remove_edge(raw, "collect", "after")
    add_edge(raw, "collect", "split")
    assert G.parse(raw).parallel_regions()["split"].join == "collect"


def module_graph():
    raw = region()
    body = {key: raw.pop(key) for key in ("entry", "nodes", "edges")}
    body["exit"] = "after"
    raw.update(graphs={"region": body}, entry="first", nodes=[
        {"id": "first", "graph": "region"}, {"id": "second", "graph": "region"}],
        edges=[{"from": "first", "to": "second"}])
    return raw


def test_reused_module_and_op_bind_pairing_per_scope_and_roundtrip():
    raw = module_graph()
    raw["ops"]["split@first"] = {"run": "true"}  # Must not overwrite an authored op.
    parsed = G.parse(raw)
    assert parsed.parallel_regions() == {
        name + "/split": G.ParallelRegion(name + "/split", name + "/collect", (
            (name + "/left", name + "/left-tail"), (name + "/right",)))
        for name in ("first", "second")}
    assert parsed.ops["split@first"].run == "true"
    assert parsed.definition("first/split").fanout == {"join": "first/collect"}
    assert parsed.definition("second/split").fanout == {"join": "second/collect"}
    snapshot = G.to_dict(parsed)
    assert G.parse(snapshot) == parsed
    assert G.parse(G.to_dict(G.parse(snapshot))) == parsed
    assert raw["ops"]["split"]["fanout"] == {"join": "collect"}


def test_nested_module_scope_is_resolved_from_containing_graph():
    raw = module_graph()
    raw["graphs"]["wrapper"] = {"entry": "inner", "exit": "inner",
        "nodes": [{"id": "inner", "graph": "region"}]}
    raw["nodes"][0]["graph"] = "wrapper"
    parsed = G.parse(raw)
    assert parsed.parallel_regions()["first/inner/split"].join == "first/inner/collect"
    assert G.parse(G.to_dict(parsed)) == parsed


def test_flat_node_slashes_do_not_create_an_implicit_scope():
    raw = region()
    raw["nodes"][0]["id"] = "path/split"
    raw["entry"] = "path/split"
    for edge in raw["edges"]:
        if edge["from"] == "split":
            edge["from"] = "path/split"
    assert G.parse(raw).parallel_regions()["path/split"].join == "collect"


def test_distinct_regions_in_sequence_and_outer_loop_are_legal():
    parsed = G.parse(module_graph())
    raw = G.to_dict(parsed)
    add_edge(raw, "second/after", "first/split")
    assert len(G.parse(raw).parallel_regions()) == 2


@pytest.mark.parametrize("control,value", [
    ("fanout", None), ("fanout", []), ("fanout", "collect"), ("fanout", {}),
    ("fanout", {"join": None}), ("fanout", {"join": []}), ("fanout", {"join": ""}),
    ("fanout", {"join": "../collect"}), ("fanout", {"join": "/collect"}),
    ("fanout", {"join": "collect", "branches": []}),
    ("join", None), ("join", []), ("join", False), ("join", {"fanout": "split"}),
])
def test_malformed_control_payloads_are_rejected(control, value):
    raw = region()
    raw["ops"]["invalid"] = {control: value}
    with pytest.raises(ValueError):
        G.parse(raw)


@pytest.mark.parametrize("operations", list(combinations(["run", "call", "fanout", "join"], 2)))
def test_exactly_one_operation_even_for_empty_control_objects(operations):
    raw = region()
    values = {"run": "true", "call": {"graph": "child", "mode": "wait"},
              "fanout": {"join": "collect"}, "join": {}}
    raw["ops"]["invalid"] = {key: values[key] for key in operations}
    with pytest.raises(ValueError, match="exactly one"):
        G.parse(raw)


def test_unknown_control_fields_are_not_silently_ignored():
    raw = region()
    raw["ops"]["split"]["parallel"] = True
    with pytest.raises(ValueError, match="unknown fields"):
        G.parse(raw)


@pytest.mark.parametrize("target", ["absent", "left", "split"])
def test_pair_must_reference_an_existing_join_node(target):
    raw = region()
    raw["ops"]["split"]["fanout"]["join"] = target
    with pytest.raises(ValueError, match="not a join node"):
        G.parse(raw)


def test_two_fanouts_cannot_share_one_join():
    raw = region()
    raw["nodes"].append({"id": "other", "op": "split"})
    with pytest.raises(ValueError, match="multiple fanout"):
        G.parse(raw)


def test_even_unreachable_orphan_join_is_rejected():
    raw = region()
    raw["nodes"].append({"id": "orphan", "op": "join"})
    with pytest.raises(ValueError, match="no paired fanout"):
        G.parse(raw)


@pytest.mark.parametrize("entry", ["left", "left-tail", "right", "collect"])
def test_entry_cannot_bypass_fanout(entry):
    raw = region()
    raw["entry"] = entry
    with pytest.raises(ValueError, match="entry cannot bypass"):
        G.parse(raw)


def test_at_least_two_branches_required():
    raw = region()
    remove_edge(raw, "split", "right")
    with pytest.raises(ValueError, match="at least two"):
        G.parse(raw)


def test_empty_branch_is_rejected():
    raw = region()
    add_edge(raw, "split", "collect")
    with pytest.raises(ValueError, match="nonempty"):
        G.parse(raw)


@pytest.mark.parametrize("source,target", [("split", "left"), ("left", "left-tail"),
                                           ("right", "collect"), ("collect", "after")])
def test_duplicate_edges_affecting_region_are_rejected(source, target):
    raw = region()
    add_edge(raw, source, target)
    with pytest.raises(ValueError, match="same edge"):
        G.parse(raw)


@pytest.mark.parametrize("source,target", [("after", "left"), ("after", "left-tail"),
                                           ("right", "left-tail")])
def test_external_and_cross_branch_ingress_are_rejected(source, target):
    raw = region()
    add_edge(raw, source, target)
    with pytest.raises(ValueError, match="incoming|exactly one outgoing"):
        G.parse(raw)


@pytest.mark.parametrize("target", ["left", "after", "right"])
def test_branch_choices_cycles_and_escape_are_rejected(target):
    raw = region()
    add_edge(raw, "left-tail", target)
    with pytest.raises(ValueError, match="exactly one outgoing|incoming"):
        G.parse(raw)


def test_branch_must_eventually_reach_the_pair():
    raw = region()
    remove_edge(raw, "left-tail", "collect")
    with pytest.raises(ValueError, match="exactly one outgoing"):
        G.parse(raw)


def test_branch_cycle_with_no_path_to_join_is_rejected():
    raw = region()
    remove_edge(raw, "left-tail", "collect")
    add_edge(raw, "left-tail", "left")
    with pytest.raises(ValueError, match="incoming|cyclic"):
        G.parse(raw)


def test_branches_cannot_merge_before_join():
    raw = region()
    remove_edge(raw, "right", "collect")
    add_edge(raw, "right", "left-tail")
    with pytest.raises(ValueError, match="incoming|overlapping"):
        G.parse(raw)


def test_join_cannot_accept_external_input_or_choose_a_route():
    raw = region()
    add_edge(raw, "after", "collect")
    with pytest.raises(ValueError, match="exactly its paired branch tails"):
        G.parse(raw)
    remove_edge(raw, "after", "collect")
    add_edge(raw, "collect", "split")
    with pytest.raises(ValueError, match="multiple outgoing"):
        G.parse(raw)


def test_nested_parallel_controls_are_rejected():
    raw = G.to_dict(G.parse(module_graph()))
    remove_edge(raw, "first/left", "first/left-tail")
    remove_edge(raw, "first/after", "second/split")
    add_edge(raw, "first/left", "second/split")
    add_edge(raw, "second/after", "first/left-tail")
    with pytest.raises(ValueError, match="nested or crossing"):
        G.parse(raw)


def test_crossing_pairings_are_rejected():
    raw = G.to_dict(G.parse(module_graph()))
    split_ops = {node["id"]: node["op"] for node in raw["nodes"] if "op" in node}
    raw["ops"][split_ops["first/split"]]["fanout"]["join"] = "second/collect"
    raw["ops"][split_ops["second/split"]]["fanout"]["join"] = "first/collect"
    with pytest.raises(ValueError, match="nested or crossing"):
        G.parse(raw)


def test_ordinary_multi_route_graph_keeps_its_choice_semantics():
    raw = region()
    raw["ops"]["split"] = {"run": "true"}
    raw["ops"]["join"] = {"run": "true"}
    parsed = G.parse(raw)
    assert parsed.parallel_regions() == {}
    assert parsed.routes("split") == ("left", "right")
    assert G.parse(G.to_dict(parsed)) == parsed


def test_parallel_graph_rejects_ancestor_descendant_workspaces():
    raw = region()
    raw['nodes'].append({'id': 'left/private', 'agent': 'worker'})
    with pytest.raises(ValueError, match='overlapping node workspaces'):
        G.parse(raw)


@pytest.mark.parametrize('name', ['left/../right', 'left//private', '/tmp/escape', 'control/left', '.views/cache'])
def test_parallel_workspace_names_cannot_alias_or_expose_control_records(name):
    raw = region()
    raw['nodes'].append({'id': name, 'agent': 'worker'})
    with pytest.raises(ValueError, match='workspace|reserved directory'):
        G.parse(raw)
