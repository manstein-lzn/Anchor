"""Run a graph: a directory per node, walked in whatever order the edges allow.

There is no fixed order. A node is ready when every edge into it has been decided and at least one
was selected, so what runs next depends on what the nodes before it chose. A node whose edges were
all rejected is skipped, and the run ends when nothing is ready — which is either the graph's end or
a node that never said where to go.

Each execution gets its own directory, named for the node and then for which pass it is, because a
graph with a loop runs the same node more than once and those are different attempts at the same
work. A node starts from a copy of whatever the nodes that fed it left behind.
"""

from __future__ import annotations

import json
import shutil
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

from anchor.simple import graph as graph_module
from anchor.simple.agent import build_agent

IGNORED = shutil.ignore_patterns(".git", "__pycache__")


@dataclass(frozen=True)
class NodeResult:
    node_id: str
    agent: str
    tree: Path
    pass_number: int
    submission: str
    files: tuple[str, ...]
    submitted: bool
    exit_status: str
    route: str | None = None


@dataclass
class _Config:
    models: dict[str, dict] = field(default_factory=dict)
    secret_file: str | None = None


def _seed(target: Path, sources: list[Path]) -> None:
    """A copy of what this node's selected inputs produced, in the order the edges declare.

    A copy, so the node sees the whole deliverable so far and can revise it, and nothing it does can
    reach back and damage an earlier node's work. That is what makes "its own workspace" true without
    needing revisions to say so.
    """
    target.mkdir(parents=True, exist_ok=True)
    for source in sources:
        if not source.is_dir():
            continue
        for item in sorted(source.iterdir()):
            if item.name in {".git", "__pycache__"}:
                continue
            destination = target / item.name
            if item.is_dir():
                shutil.copytree(item, destination, dirs_exist_ok=True, ignore=IGNORED)
            else:
                shutil.copy2(item, destination)


def _files(tree: Path) -> tuple[str, ...]:
    return tuple(sorted(str(item.relative_to(tree)) for item in tree.rglob("*")
                        if item.is_file() and ".git" not in item.relative_to(tree).parts))


def _task(graph: graph_module.Graph, node_id: str, objective: str,
          sources: list[NodeResult]) -> str:
    lines = [f"# Task\n\n{objective}"]
    if sources:
        lines.append("# What the nodes before you produced")
        for result in sources:
            mark = "" if result.submitted else "  (this node did not submit)\n"
            lines.append(f"## {result.node_id}\n\n{mark}"
                         f"{result.submission.strip() or '(nothing said)'}")
            if result.files:
                lines.append("Its files are already in your directory:\n"
                             + "\n".join(f"  {name}" for name in result.files))
    routes = graph.routes(node_id)
    if len(routes) > 1:
        lines.append("# How this node finishes\n\nYou decide where it goes next. When the work is "
                     "done, run this and nothing after it:\n\n"
                     f"    anchor-route --to <{'|'.join(routes)}> --reason \"one line why\"\n\n"
                     "That is the only way this node finishes — the ordinary completion command is "
                     "not accepted here. The reason is recorded; nothing parses it.")
    else:
        lines.append("# How this node finishes\n\nWhen the work is done, run this and nothing "
                     "after it:\n\n    echo COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT")
    return "\n\n".join(lines)


def run(workspace: str | Path, *, objective: str | None = None,
        config_path: str | Path, run_id: str | None = None) -> list[NodeResult]:
    workspace = Path(workspace).resolve()
    graph = graph_module.load(workspace / "graph.json")
    raw = json.loads(Path(config_path).read_text(encoding="utf-8"))
    config = _Config(models={item["ref"]: item for item in raw.get("models", [])},
                     secret_file=raw.get("secret_file"))
    stamp = run_id or datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")
    run_dir = workspace / "runs" / stamp
    run_dir.mkdir(parents=True, exist_ok=True)

    # (source, target) -> (selected, the execution that decided it). The sequence number is what
    # makes a loop work: a node is ready when its inputs are *newer than its own last run*, not when
    # they merely exist. Deciding once and never again is a pipeline, and a graph that revises is not
    # one.
    decided: dict[tuple[str, str], tuple[bool, int]] = {}
    passes: dict[str, int] = {}
    last_seq: dict[str, int] = {}
    latest: dict[str, NodeResult] = {}
    results: list[NodeResult] = []
    order = list(graph.nodes)
    entry = graph.entry()
    seq = 0

    def ready(node_id: str) -> bool:
        """Whether this node's inputs have arrived since it last ran, and at least one is selected."""
        if node_id == entry and node_id not in passes:
            return True                      # the entry starts the run, whatever loops into it later
        sources = graph.in_edges[node_id]
        if not sources:
            return node_id not in passes
        keys = [(source, node_id) for source in sources]
        since = last_seq.get(node_id, -1)
        if not all(key in decided and decided[key][1] > since for key in keys):
            return False
        return any(decided[key][0] for key in keys)

    def settle(node_id: str, chosen: str | None) -> None:
        """Record how this execution resolved every way out it had."""
        ways = graph.routes(node_id)
        for target in ways:
            decided[(node_id, target)] = (target == chosen, seq)

    while True:
        pending = [node for node in order if ready(node)]
        if not pending:
            break
        node_id = pending[0]
        sources = graph.in_edges[node_id]
        incoming = [latest[source] for source in sources
                    if decided.get((source, node_id), (False, -1))[0] and source in latest]
        number = passes.get(node_id, 0) + 1
        passes[node_id] = number

        if number > graph.max_rounds:
            # Reached more times than the graph allows. Its ways out are refused rather than the run
            # failing: that is what a round limit means, and the nodes downstream are skipped the
            # ordinary way.
            settle(node_id, None)
            print(json.dumps({"node": node_id, "stopped": "max_rounds",
                              "limit": graph.max_rounds}), flush=True)
            continue

        directory = run_dir / (node_id if number == 1 else f"{node_id}-{number}")
        _seed(directory, [result.tree for result in incoming])
        agent_spec = graph.agents[graph.nodes[node_id]]
        model = config.models.get(agent_spec.model)
        if model is None:
            raise ValueError(f"no model named {agent_spec.model!r} in {config_path}")
        agent = build_agent(
            tree=directory, node_id=node_id, routes=graph.routes(node_id),
            instructions=agent_spec.instructions,
            model_name=f"openai/{model['model']}" if model.get("base_url") else model["model"],
            model_kwargs={"api_base": model["base_url"], "api_key": _secret(config, model),
                          "max_tokens": model.get("max_tokens", 8192)},
            network=agent_spec.network, timeout_seconds=300.0, max_steps=agent_spec.max_steps,
            wall_time_limit_seconds=agent_spec.wall_time_limit_seconds,
        )
        seq += 1
        last_seq[node_id] = seq
        outcome = agent.run(task=_task(graph, node_id, objective or graph.objective, incoming))
        route = getattr(agent.env, "route", None)
        result = NodeResult(node_id=node_id, agent=graph.nodes[node_id], tree=directory,
                            pass_number=number,
                            submission=str(outcome.get("submission") or ""), files=_files(directory),
                            submitted=outcome.get("exit_status") == "Submitted", route=route,
                            exit_status=str(outcome.get("exit_status") or ""))
        latest[node_id] = result
        results.append(result)

        ways = graph.routes(node_id)
        if not ways:
            continue
        settle(node_id, route if len(ways) > 1 else ways[0])
        chosen = route if len(ways) > 1 else ways[0]
        print(json.dumps({"node": node_id, "pass": number, "agent": result.agent,
                          "submitted": result.submitted, "route": chosen,
                          "files": list(result.files)}, ensure_ascii=False), flush=True)

    unrun = [node for node in order if node not in passes]
    print(json.dumps({"run": str(run_dir), "executed": [r.node_id for r in results],
                      "skipped": unrun}, ensure_ascii=False), flush=True)
    return results


def _secret(config: _Config, model: dict) -> str:
    from anchor.runtime.secrets import (
        ChainedSecretProvider,
        EnvironmentSecretProvider,
        JsonFileSecretProvider,
    )

    providers: list = [EnvironmentSecretProvider()]
    if config.secret_file:
        providers.append(JsonFileSecretProvider(config.secret_file))
    return ChainedSecretProvider(*providers).get(model["secret_ref"])
