"""A graph can contain graphs, and what a run reads is the expansion.

These are the rules that make one file cover a whole network: where a module's edges attach, what a
module may and may not declare, and what is refused so that two different things cannot end up with
the same name.
"""

from __future__ import annotations

import pytest

from anchor.simple import graph as G


def _file(**overrides) -> dict:
    raw = {
        "objective": "a survey",
        "agents": {"w": {"model": "m", "instructions": "role"}},
        "graphs": {
            "mod": {
                "entry": "a",
                "exit": "b",
                "nodes": [{"id": "a", "agent": "w"}, {"id": "b", "agent": "w"}],
                "edges": [{"from": "a", "to": "b"}],
            }
        },
        "entry": "in",
        "nodes": [{"id": "in", "agent": "w"},
                  {"id": "use", "graph": "mod"},
                  {"id": "out", "agent": "w"}],
        "edges": [{"from": "in", "to": "use"}, {"from": "use", "to": "out"}],
    }
    raw.update(overrides)
    return raw


# -- what expansion produces ---------------------------------------------------------------------


def test_a_module_is_inlined_under_its_own_name():
    graph = G.parse(_file())

    assert list(graph.nodes) == ["in", "use/a", "use/b", "out"]


def test_edges_attach_to_the_modules_entry_and_its_exit():
    """The parent never sees the module, so its edges have to arrive somewhere inside it.

    An edge into the module lands on the module's `entry`; an edge out of it leaves from the module's
    `exit`. Getting either backwards produces a graph that still runs — it just runs the wrong thing,
    which is why this is asserted rather than assumed.
    """
    graph = G.parse(_file())

    assert G.edges(graph) == [("in", "use/a"), ("use/a", "use/b"), ("use/b", "out")]


def test_the_same_module_twice_gives_two_scopes():
    raw = _file(nodes=[{"id": "in", "agent": "w"},
                       {"id": "first", "graph": "mod"},
                       {"id": "second", "graph": "mod"}],
                edges=[{"from": "in", "to": "first"}, {"from": "first", "to": "second"}])

    graph = G.parse(raw)

    assert list(graph.nodes) == ["in", "first/a", "first/b", "second/a", "second/b"]
    assert G.edges(graph) == [("in", "first/a"), ("first/a", "first/b"),
                              ("first/b", "second/a"), ("second/a", "second/b")]


def test_a_module_may_omit_its_entry_because_it_can_be_inferred():
    raw = _file()
    del raw["graphs"]["mod"]["entry"]

    assert "use/a" in G.parse(raw).nodes


def test_each_scope_keeps_its_own_ceiling():
    """The parent's bound is the parent's; a module's bound belongs to the module."""
    raw = _file(max_rounds=2)
    raw["graphs"]["mod"]["max_rounds"] = 8

    graph = G.parse(raw)

    assert graph.ceiling("in") == 2
    assert graph.ceiling("out") == 2
    assert graph.ceiling("use/a") == 8
    assert graph.ceiling("use/b") == 8


def test_a_nodes_own_ceiling_wins_over_its_scope():
    raw = _file(max_rounds=2)
    raw["nodes"][0]["max_rounds"] = 9

    assert G.parse(raw).ceiling("in") == 9


def test_with_is_carried_on_the_node_and_not_on_the_role():
    """Two nodes may share a role and differ in what this use adds to it."""
    raw = _file()
    raw["graphs"]["mod"]["nodes"][0]["with"] = "Focus on the prose."

    graph = G.parse(raw)

    assert graph.nodes["use/a"].agent == "w"
    assert graph.nodes["use/a"].with_ == "Focus on the prose."
    assert graph.nodes["use/b"].with_ == ""


def test_the_expanded_graph_is_itself_a_graph_file():
    """What a run writes into its own directory has to be a graph, or the record is a dead end."""
    graph = G.parse(_file())

    again = G.parse(G.to_dict(graph))

    assert list(again.nodes) == list(graph.nodes)
    assert G.edges(again) == G.edges(graph)
    assert again.ceiling("use/a") == graph.ceiling("use/a")
    assert again.nodes["use/a"] == graph.nodes["use/a"]


# -- what is refused -----------------------------------------------------------------------------


@pytest.mark.parametrize("key", ["agents", "objective", "graphs"])
def test_a_module_may_not_declare_what_the_file_declares(key):
    """One agent pool per file is what makes a role worth declaring separately from its nodes."""
    raw = _file()
    raw["graphs"]["mod"][key] = {"x": {}} if key != "objective" else "text"

    with pytest.raises(ValueError) as caught:
        G.parse(raw)

    assert key in str(caught.value)


def test_a_module_must_say_where_its_result_comes_from():
    raw = _file()
    del raw["graphs"]["mod"]["exit"]

    with pytest.raises(ValueError) as caught:
        G.parse(raw)

    assert "exit" in str(caught.value)


def test_a_graph_that_contains_itself_is_refused():
    """No finite expansion, and no finite identity: its content would include its own content."""
    raw = _file()
    raw["graphs"]["mod"]["nodes"].append({"id": "again", "graph": "mod"})

    with pytest.raises(ValueError) as caught:
        G.parse(raw)

    assert "cycle" in str(caught.value)


def test_two_graphs_containing_each_other_are_refused():
    raw = _file()
    raw["graphs"]["other"] = {"entry": "p", "exit": "q",
                              "nodes": [{"id": "p", "agent": "w"}, {"id": "q", "graph": "mod"}],
                              "edges": [{"from": "p", "to": "q"}]}
    raw["graphs"]["mod"]["nodes"][0] = {"id": "a", "graph": "other"}

    with pytest.raises(ValueError) as caught:
        G.parse(raw)

    assert "cycle" in str(caught.value)


def test_a_reference_to_a_graph_that_is_not_here_is_refused():
    raw = _file()
    raw["nodes"][1]["graph"] = "elsewhere"

    with pytest.raises(ValueError) as caught:
        G.parse(raw)

    assert "elsewhere" in str(caught.value)


def test_a_node_needs_exactly_one_of_agent_and_graph():
    for item in ({"id": "x", "agent": "w", "graph": "mod"}, {"id": "x"}):
        raw = _file()
        raw["nodes"] = [item]

        with pytest.raises(ValueError) as caught:
            G.parse(raw)

        assert "agent" in str(caught.value) and "graph" in str(caught.value)


def test_the_separator_is_reserved_when_the_file_expands():
    """`a/b` written by hand and "node b of module a" would otherwise be one string."""
    raw = _file()
    raw["nodes"][0] = {"id": "a/b", "agent": "w"}

    with pytest.raises(ValueError) as caught:
        G.parse(raw)

    assert "/" in str(caught.value)


def test_a_module_name_may_contain_the_separator():
    """The scope prefix comes from the node's id, not from the graph's name — so only ids are
    restricted. Naming a module `in/out` is odd, and harmless."""
    raw = _file()
    raw["graphs"]["in/out"] = raw["graphs"].pop("mod")
    raw["nodes"][1]["graph"] = "in/out"

    assert list(G.parse(raw).nodes) == ["in", "use/a", "use/b", "out"]


def test_a_flat_file_may_use_the_separator():
    """Nothing expands, so there is nothing for a name to collide with — and this is the shape an
    already-expanded graph has, which is what lets a run write out the graph it read."""
    flat = {"objective": "o", "agents": {"w": {"model": "m"}}, "entry": "a/b",
            "nodes": [{"id": "a/b", "agent": "w"}], "edges": []}

    assert list(G.parse(flat).nodes) == ["a/b"]


@pytest.mark.parametrize("key,value", [("with", "text"), ("max_rounds", 5)])
def test_a_module_node_may_not_carry_what_would_be_ignored(key, value):
    """A field nothing reads is worse than a field that is refused, so it is refused."""
    raw = _file()
    raw["nodes"][1][key] = value

    with pytest.raises(ValueError) as caught:
        G.parse(raw)

    assert key in str(caught.value)
