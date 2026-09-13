"""Run a graph: one directory per node, seeded from the nodes it depends on, each walked by
mini-swe-agent until it submits.

Nothing here retries a model call, counts tokens, parses a reply or manages a conversation. Those
belong to the agent loop, and it is theirs. What is left is the part that is actually ours: which
nodes run, in what order, what each one starts with, and where its commands are allowed to run.
"""

from __future__ import annotations

import json
import shutil
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from uuid import uuid4

from anchor.simple import graph as graph_module
from anchor.simple.agent import build_agent

IGNORED = shutil.ignore_patterns(".git", "__pycache__")


@dataclass(frozen=True)
class NodeResult:
    node_id: str
    agent: str
    tree: Path
    submission: str
    files: tuple[str, ...]
    submitted: bool
    exit_status: str
    turns: int = 0


@dataclass
class _Config:
    models: dict[str, dict] = field(default_factory=dict)
    secret_file: str | None = None


def _seed(target: Path, sources: list[Path]) -> None:
    """Give the node everything its inputs produced, in the order they were declared.

    A copy, so the node sees the whole deliverable so far and can revise it, and nothing it does can
    reach back and damage an earlier node's work. That is what makes "its own workspace" true
    without needing revisions to say so.
    """
    target.mkdir(parents=True, exist_ok=True)
    for source in sources:
        if not source.is_dir():
            continue
        for item in sorted(source.iterdir()):
            if item.name in {".git", "__pycache__", "trace.jsonl"}:
                continue
            destination = target / item.name
            if item.is_dir():
                shutil.copytree(item, destination, dirs_exist_ok=True, ignore=IGNORED)
            else:
                shutil.copy2(item, destination)


def _files(tree: Path) -> tuple[str, ...]:
    return tuple(sorted(str(item.relative_to(tree)) for item in tree.rglob("*")
                        if item.is_file() and ".git" not in item.relative_to(tree).parts
                        and item.name != "trace.jsonl"))


def _task(graph: graph_module.Graph, node_id: str, objective: str, done: dict) -> str:
    lines = [f"# Task\n\n{objective}"]
    sources = graph.inputs[node_id]
    if sources:
        lines.append("# What the nodes before you produced")
        for source in sources:
            result = done[source]
            mark = "" if result.submitted else "  (this node did not submit)\n"
            lines.append(f"## {source}\n\n{mark}{result.submission.strip() or '(nothing said)'}")
            if result.files:
                lines.append("Its files are already in your directory:\n"
                             + "\n".join(f"  {name}" for name in result.files))
    return "\n\n".join(lines)


def _trace(tree: Path, messages: list) -> None:
    """The conversation, one line per message, written where it happened.

    This is the debugging surface: without it, what an agent did has to be inferred by re-running
    it, which is slow and produces guesses. With it, "it searched 47 times and adapted around a
    source that kept refusing" is a thing you read.
    """
    with (tree / "trace.jsonl").open("w", encoding="utf-8") as handle:
        for index, message in enumerate(messages):
            role = message.get("role")
            parts = message.get("content")
            if isinstance(parts, list):
                parts = " | ".join(str(part.get("text", part))[:400] for part in parts)
            handle.write(json.dumps({"i": index, "role": role,
                                     "text": str(parts or "")[:4000],
                                     "extra": message.get("extra") or {}},
                                    ensure_ascii=False) + "\n")


def run(path: str | Path, objective: str | None = None, *, work: str | Path,
        config_path: str | Path, task_override: str | None = None) -> list[NodeResult]:
    graph = graph_module.load(path)
    # Only what a run needs from a runtime config: model profiles, and where the secret lives. The
    # rest of that file belongs to the system this runner does not use.
    raw = json.loads(Path(config_path).read_text(encoding="utf-8"))
    config = _Config(models={item["ref"]: item for item in raw.get("models", [])},
                     secret_file=raw.get("secret_file"))
    work = Path(work)
    work.mkdir(parents=True, exist_ok=True)
    results: list[NodeResult] = []
    done: dict[str, NodeResult] = {}

    for node_id in graph.order():
        agent_spec = graph.agents[graph.nodes[node_id]]
        tree = work / node_id
        _seed(tree, [done[source].tree for source in graph.inputs[node_id]])
        model = config.models.get(agent_spec.model)
        if model is None:
            raise ValueError(f"no model named {agent_spec.model!r} in {config_path}")
        agent = build_agent(
            tree=tree,
            instructions=agent_spec.instructions,
            model_name=f"openai/{model['model']}" if model.get("base_url") else model["model"],
            model_kwargs={"api_base": model["base_url"], "api_key": _secret(config, model),
                          "max_tokens": model.get("max_tokens", 8192)},
            network=agent_spec.network,
            timeout_seconds=300.0,
            max_steps=agent_spec.max_steps,
            wall_time_limit_seconds=agent_spec.wall_time_limit_seconds,
        )
        # Nothing can narrate its way out: a turn without a command is retried by the loop, and the
        # run ends only when a command asks for submission.
        outcome = agent.run(task=task_override or _task(graph, node_id,
                                                        objective or graph.objective, done))
        _trace(tree, agent.messages)
        result = NodeResult(node_id=node_id, agent=graph.nodes[node_id], tree=tree,
                            submission=str(outcome.get("submission") or ""),
                            files=_files(tree),
                            submitted=outcome.get("exit_status") == "Submitted",
                            exit_status=str(outcome.get("exit_status") or ""),
                            turns=len(agent.messages))
        done[node_id] = result
        results.append(result)
        print(json.dumps({"node": node_id, "agent": result.agent, "submitted": result.submitted,
                          "exit_status": result.exit_status, "files": list(result.files)},
                         ensure_ascii=False), flush=True)
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


def new_work_dir(root: str | Path) -> Path:
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")
    return Path(root) / f"run-{stamp}-{uuid4().hex[:6]}"
