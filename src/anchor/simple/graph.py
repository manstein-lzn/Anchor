"""A graph is a JSON file, and that is the whole definition.

No drafts, no versions, no admission checks, no bundles. You point the runner at a file and it
runs. Everything the runtime used to decide before a graph could run is now something you decide by
editing this file.

    {
      "objective": "what this graph is for, as the default task text",
      "root": ".local/graphs/academic-research",
      "agents": {
        "planner": {"model": "models.academic", "instructions": "…", "network": false}
      },
      "nodes": [{"id": "plan", "agent": "planner"}],
      "edges": [{"from": "plan", "to": "gather"}]
    }
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Agent:
    model: str
    instructions: str = ""
    tools: tuple[str, ...] = ()
    # Whether this node's commands may reach the network. A node that reads the literature needs
    # it; a node that only writes a file does not, and refusing it costs nothing.
    network: bool = False
    max_steps: int = 0
    wall_time_limit_seconds: int = 1800


@dataclass(frozen=True)
class Graph:
    nodes: dict[str, str]           # node id -> agent name
    agents: dict[str, Agent]
    inputs: dict[str, tuple[str, ...]]
    objective: str = ""
    root: str | None = None

    def order(self) -> list[str]:
        """Nodes in dependency order. A cycle is an error rather than a loop: this runner has no
        notion of revisiting a node, and inventing one silently would be worse than refusing."""
        remaining = dict(self.inputs)
        done: list[str] = []
        while remaining:
            ready = [node for node, sources in remaining.items()
                     if all(source in done for source in sources)]
            if not ready:
                raise ValueError(f"the graph has a cycle: {sorted(remaining)}")
            for node in sorted(ready):
                done.append(node)
                del remaining[node]
        return done


def load(path: str | Path) -> Graph:
    raw = json.loads(Path(path).read_text(encoding="utf-8"))
    agents = {
        name: Agent(model=spec["model"], instructions=spec.get("instructions", ""),
                    tools=tuple(spec.get("tools") or ()),
                    network=bool(spec.get("network", False)),
                    max_steps=int(spec.get("max_steps", 0)),
                    wall_time_limit_seconds=int(spec.get("wall_time_limit_seconds", 1800)))
        for name, spec in raw["agents"].items()
    }
    nodes = {item["id"]: item["agent"] for item in raw["nodes"]}
    inputs: dict[str, list[str]] = {node: [] for node in nodes}
    for edge in raw.get("edges") or ():
        source, target = edge["from"], edge["to"]
        if source not in nodes or target not in nodes:
            raise ValueError(f"edge names an unknown node: {edge}")
        inputs[target].append(source)
    missing = {agent for agent in nodes.values() if agent not in agents}
    if missing:
        raise ValueError(f"nodes name unknown agents: {sorted(missing)}")
    return Graph(nodes=nodes, agents=agents,
                 inputs={node: tuple(sources) for node, sources in inputs.items()},
                 objective=raw.get("objective", ""), root=raw.get("root"))
