"""A workspace is a graph: the structure, the permissions, and every run it has had.

    <workspace>/
      graph.json          the nodes, the edges between them, and what each agent may do
      runs/
        2026-09-13T22-30-00/
          plan/  gather/  write/  review/
        2026-09-13T23-10-00/

The graph lives in the workspace rather than beside it, because the workspace is the unit: point at
one and everything a run needs is there. Runs accumulate beside each other and are never merged —
there is no state carried from one to the next, so the history is a record rather than a dependency.

An edge says where the graph may go, not when. A node with one way out follows it; a node with more
than one names its choice, and the edge is selected or rejected accordingly. Routing is a decision
the node makes, so it does not have to express it as data.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Agent:
    model: str
    instructions: str = ""
    # Whether this node's commands may reach the network. A node whose work is reading the
    # literature needs it; a node that writes a file does not, and refusing it costs nothing.
    network: bool = False
    # A bound on the model's turns, because a node that keeps deciding it is finished without
    # finishing will otherwise spend the whole wall-clock budget saying so. Sixty is generous for
    # work that is already describing itself in minutes.
    max_steps: int = 0
    # An hour, not half of one. A real literature search with a rate-limited source spends most of
    # its clock waiting, and the first version of this cut off a gathering step that was still
    # working and had sixty result files to show for it.
    wall_time_limit_seconds: int = 3600


@dataclass(frozen=True)
class Graph:
    nodes: dict[str, str]                       # node id -> agent name
    agents: dict[str, Agent]
    out_edges: dict[str, tuple[str, ...]]
    in_edges: dict[str, tuple[str, ...]]
    objective: str = ""
    # Where a run starts. Declared, because a graph with a loop has no node that nothing leads to:
    # the revision edge means every node has an incoming edge, so "the one with none" is not a rule
    # that can be inferred — it only looks like one until the first graph that loops.
    entry_node: str = ""
    # How many times a node may run in one pass. Only loops can exceed one, and without a bound a
    # graph that revises can revise forever.
    max_rounds: int = 3

    def entry(self) -> str:
        """Where a run starts.

        Declared, or inferred only when it is unambiguous. A graph with a loop has no node without an
        incoming edge, and one that does not loop usually has exactly one — so the inference is a
        convenience, not the rule.
        """
        if self.entry_node:
            return self.entry_node
        starts = [node for node, sources in self.in_edges.items() if not sources]
        if len(starts) != 1:
            raise ValueError(
                "a graph needs an explicit \"entry\" node when it has a loop or several starts; "
                f"nodes with no incoming edge: {sorted(starts)}")
        return starts[0]

    def routes(self, node_id: str) -> tuple[str, ...]:
        """The ways out of a node. More than one means the node must choose."""
        return self.out_edges.get(node_id, ())


def back_edges(graph: Graph) -> frozenset[tuple[str, str]]:
    """Edges that close a structural cycle, found by walking from the entry.

    A pending node must not wait for an edge whose source has never run: on a first arrival the cycle
    it belongs to has not started, and waiting for it deadlocks the graph at the first node after the
    entry. Once that source has run, its decision exists and gates normally — which is what makes a
    loop fire on the second pass and not the first.

    This was missing, and its absence looked like a working pipeline: `gather` had edges in from
    `gather` and `review`, neither of which could have decided anything yet, so `gather` never became
    ready and the run reported `finished` having done one node.
    """
    colour: dict[str, int] = {graph.entry(): 1}          # 1 = on the current path
    found: set[tuple[str, str]] = set()
    stack: list[tuple[str, list[str]]] = [(graph.entry(), list(graph.out_edges.get(graph.entry(), ())))]
    while stack:
        node, targets = stack[-1]
        if not targets:
            colour[node] = 2
            stack.pop()
            continue
        target = targets.pop(0)
        if colour.get(target) == 1:
            found.add((node, target))
        elif colour.get(target) is None:
            colour[target] = 1
            stack.append((target, list(graph.out_edges.get(target, ()))))
    return frozenset(found)


def load(path: str | Path) -> Graph:
    raw = json.loads(Path(path).read_text(encoding="utf-8"))
    agents = {
        name: Agent(model=spec["model"], instructions=spec.get("instructions", ""),
                    network=bool(spec.get("network", False)),
                    max_steps=int(spec.get("max_steps", 0)),
                    wall_time_limit_seconds=int(spec.get("wall_time_limit_seconds", 3600)))
        for name, spec in raw["agents"].items()
    }
    nodes = {item["id"]: item["agent"] for item in raw["nodes"]}
    missing = {agent for agent in nodes.values() if agent not in agents}
    if missing:
        raise ValueError(f"nodes name unknown agents: {sorted(missing)}")
    out_edges: dict[str, list[str]] = {node: [] for node in nodes}
    in_edges: dict[str, list[str]] = {node: [] for node in nodes}
    for edge in raw.get("edges") or ():
        source, target = edge["from"], edge["to"]
        if source not in nodes or target not in nodes:
            raise ValueError(f"edge names an unknown node: {edge}")
        if target in out_edges[source]:
            raise ValueError(f"the same edge is declared twice: {edge}")
        out_edges[source].append(target)
        in_edges[target].append(source)
    graph = Graph(nodes=nodes, agents=agents,
                  out_edges={node: tuple(targets) for node, targets in out_edges.items()},
                  in_edges={node: tuple(sources) for node, sources in in_edges.items()},
                  objective=raw.get("objective", ""), entry_node=raw.get("entry", ""),
                  max_rounds=int(raw.get("max_rounds", 3)))
    graph.entry()          # fail at load rather than at the first step of a run
    return graph
