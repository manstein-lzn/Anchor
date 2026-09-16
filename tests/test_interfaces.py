"""What a node says it reads has to be something it can actually be handed.

This is the class of failure the runtime cannot report, because nothing about it looks wrong while it
is happening: a node whose input is wired to nothing reads nothing, does the work anyway, and submits.
The `revise-loop` example was exactly that — `done` was told to copy `draft.md`, its only edge came
from `review`, and the reviewer wrote only `review.md`. The graph loaded, the run finished, and the
loop inside it had never read anything.

Refused here instead, where the author is, and where the message can say what would have worked.
"""

from __future__ import annotations

import pytest

from anchor.simple import graph as G


def _file(**overrides) -> dict:
    raw = {
        "entry": "a",
        "objective": "test",
        "agents": {
            "writer": {"model": "m", "writes": ["plan.md"]},
            "reader": {"model": "m", "reads": ["plan.md"]},
        },
        "nodes": [{"id": "a", "agent": "writer"}, {"id": "b", "agent": "reader"}],
        "edges": [{"from": "a", "to": "b"}],
    }
    raw.update(overrides)
    return raw


def test_a_node_may_read_what_its_input_writes():
    parsed = G.parse(_file())

    assert parsed.reads("b") == ("plan.md",)
    assert parsed.writes("a") == ("plan.md",)


def test_a_node_may_read_what_an_input_was_built_from():
    """The whole point of the reaching rule: `c` has an edge from `b` only, and still reaches `a`.

    Under copying this came free. Under pointers `b` would have to carry `a`'s file forward, by hand
    and by memory — and forgetting to is silent.
    """
    raw = _file(agents={
        "writer": {"model": "m", "writes": ["plan.md"]},
        "middle": {"model": "m", "writes": ["notes.md"]},
        "last": {"model": "m", "reads": ["plan.md", "notes.md"]}})
    raw["nodes"] = [{"id": "a", "agent": "writer"}, {"id": "b", "agent": "middle"},
                    {"id": "c", "agent": "last"}]
    raw["edges"] = [{"from": "a", "to": "b"}, {"from": "b", "to": "c"}]

    G.parse(raw)          # reaching back to `a` through `b` is what makes this load


def test_a_node_may_read_what_it_wrote_itself():
    """Its workspace is kept between passes, so a node revising its own work has it in front of it."""
    raw = _file(agents={"writer": {"model": "m", "reads": ["draft.md"], "writes": ["draft.md"]}})
    raw["nodes"] = [{"id": "a", "agent": "writer"}]
    raw["edges"] = []

    G.parse(raw)


def test_a_node_may_not_read_what_nothing_writes():
    raw = _file(agents={"writer": {"model": "m", "writes": ["plan.md"]},
                        "reader": {"model": "m", "reads": ["sources.md"]}})

    with pytest.raises(ValueError) as caught:
        G.parse(raw)

    message = str(caught.value)
    assert "sources.md" in message and "'b'" in message
    assert "It can be handed" in message, "the message has to say what would have worked"
    assert "a writes plan.md" in message


def test_a_node_may_not_read_what_only_a_sibling_writes():
    """Two ways out of one node are alternatives, so neither branch can be handed the other's work."""
    raw = _file(
        agents={"writer": {"model": "m", "writes": ["plan.md"]},
                "left": {"model": "m", "writes": ["left.md"]},
                "right": {"model": "m", "reads": ["left.md"]}},
        nodes=[{"id": "a", "agent": "writer"}, {"id": "b", "agent": "left"},
               {"id": "c", "agent": "right"}],
        edges=[{"from": "a", "to": "b"}, {"from": "a", "to": "c"}])

    with pytest.raises(ValueError, match="left.md"):
        G.parse(raw)


def test_a_loop_may_not_read_across_its_own_back_edge():
    """A back edge carries the loop's current state, so the round before it is not reachable.

    `c` reads a file `b` writes, `b` is behind a back edge from `c` — the only way `b`'s file could
    arrive is by the loop coming round, and what the loop carries is `c`'s own state, not `b`'s.
    """
    raw = _file(
        agents={"writer": {"model": "m", "writes": ["seed.md"]},
                "loop": {"model": "m", "reads": ["seed.md"], "writes": ["grown.md"]},
                "tail": {"model": "m", "reads": ["grown.md"]}},
        nodes=[{"id": "a", "agent": "writer"}, {"id": "b", "agent": "loop"},
               {"id": "c", "agent": "tail"}],
        # c -> b is the back edge; b can reach a, and a can reach nothing
        edges=[{"from": "a", "to": "b"}, {"from": "b", "to": "c"}, {"from": "c", "to": "b"}])

    G.parse(raw)          # b's reads are satisfied by a, which is a forward input


def test_the_interface_has_to_be_inside_the_workspace():
    """A declaration that could name anything would check nothing."""
    for bad in ("/etc/passwd", "../outside.md", ""):
        raw = _file(agents={"writer": {"model": "m", "writes": ["plan.md"]},
                            "reader": {"model": "m", "reads": [bad]}})
        with pytest.raises(ValueError, match="not a path inside the workspace"):
            G.parse(raw)


def test_an_op_declares_the_same_interface():
    """One interface, two kinds of node — which is what makes the check apply to both."""
    raw = {
        "entry": "make", "objective": "test",
        "ops": {"make": {"run": "printf 'x\\n' > plan.md", "writes": ["plan.md"]},
                "read-plan": {"run": "test -s plan.md", "reads": ["plan.md"]}},
        "nodes": [{"id": "make", "op": "make"}, {"id": "check", "op": "read-plan"}],
        "edges": [{"from": "make", "to": "check"}],
    }

    G.parse(raw)

    raw["ops"]["read-plan"]["reads"] = ["sources.md"]
    with pytest.raises(ValueError, match="sources.md"):
        G.parse(raw)


def test_an_op_is_one_of_the_three_things_a_node_can_be():
    raw = {
        "entry": "a", "objective": "test",
        "ops": {"one": {"run": "true"}},
        "nodes": [{"id": "a", "op": "one"}],
        "edges": [],
    }
    G.parse(raw)

    raw["nodes"] = [{"id": "a", "op": "one", "agent": "writer"}]
    with pytest.raises(ValueError, match="one of them"):
        G.parse(raw)

    # Naming the other kind is not "one of them" but "not declared": an op is not an agent, and the
    # message says which name is unknown rather than leaving it to the run to find out.
    raw["nodes"] = [{"id": "a", "agent": "one"}]
    with pytest.raises(ValueError, match="not declared"):
        G.parse(raw)


def test_an_op_has_no_instructions_to_add_to():
    """`with` appends to what a role says. An op is a command, and its parameters are in the command,
    so a `with` here would be read by nothing — which is refused rather than accepted quietly."""
    raw = {
        "entry": "a", "objective": "test",
        "ops": {"one": {"run": "true"}},
        "nodes": [{"id": "a", "op": "one", "with": "and also"}],
        "edges": [],
    }

    with pytest.raises(ValueError, match="nothing to apply to"):
        G.parse(raw)


def test_an_op_needs_a_command():
    """A node that runs a command without a command has nothing to do, and would finish having
    done it."""
    raw = {"entry": "a", "objective": "test", "ops": {"one": {}},
           "nodes": [{"id": "a", "op": "one"}], "edges": []}

    with pytest.raises(ValueError, match="needs a \"run\""):
        G.parse(raw)
