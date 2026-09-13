"""Run a graph: one directory per node, seeded from the nodes it depends on.

A node's workspace is a plain directory. It starts as a copy of everything its inputs produced, so
a node sees the whole deliverable so far and can revise it, and nothing it does can reach back and
damage an earlier node's work — that is what makes "its own sandbox" true without needing revisions
to say so.

The result is the same directory, plus whatever the node said. A downstream node gets the text; the
files are already in its tree.
"""

from __future__ import annotations

import json
import shutil
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from uuid import uuid4

from anchor.runtime.model_gateway import PydanticAIModelGateway
from anchor.runtime.secrets import ChainedSecretProvider, EnvironmentSecretProvider, JsonFileSecretProvider
from anchor.simple import graph as graph_module
from anchor.simple import tools as tool_module

IGNORED = shutil.ignore_patterns(".git", "__pycache__")


@dataclass(frozen=True)
class NodeResult:
    node_id: str
    agent: str
    tree: Path
    text: str
    files: tuple[str, ...]
    completed: bool = False


def _seed(target: Path, sources: list[Path]) -> None:
    """Give the node everything its inputs produced, in the order they were declared."""
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


def _prompt(graph: graph_module.Graph, node_id: str, objective: str, done: dict) -> str:
    lines = [f"# Task\n\n{objective}"]
    sources = graph.inputs[node_id]
    if sources:
        lines.append("# What the nodes before you produced")
        for source in sources:
            result = done[source]
            mark = "" if result.completed else "  (this node did not mark its goal complete)\n"
            lines.append(f"## {source}\n\n{mark}{result.text.strip() or '(no text)'}")
            if result.files:
                lines.append("Its files are already in your workspace:\n"
                             + "\n".join(f"  {name}" for name in result.files))
    lines.append("# Your workspace\n\nIt is the only place you may write. What is in it when you "
                 "finish is what the nodes after you receive.\n\n"
                 "You are finished only when you call `goal_complete`. Until you call it you will be "
                 "asked to continue, so a reply in words does not end your turn.")
    return "\n\n".join(lines)


DEFAULT_MAX_TURNS = 12
"""How many times a node may be asked to continue before the runner gives up on it.

A node ends when it marks its goal complete. Until then it keeps being asked, because a reply in
words is not work — the model says "I'll start by reading the plan" and stops, and every harness
that runs unattended has to answer that with another turn rather than with a result. The bound
exists so a node that never gets there is reported as unfinished instead of running forever.
"""


def _continue_prompt(prompt: str, node_id: str, tree: Path, turn: int) -> str:
    """Ask the node to carry on. It sees its own workspace, and the conversation is continuous, so
    it also sees everything it has already done."""
    present = _files(tree)
    listing = "\n".join(f"  {name}" for name in present) if present else "  (empty)"
    return (
        f"Your workspace contains:\n\n{listing}\n\n"
        f"You have not called `goal_complete`, so this is not finished — round {turn + 1}. "
        f"Reply with a tool call, or call `goal_complete` with a summary if the goal is met.")


def _trace(path: Path, **line) -> None:
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(line, ensure_ascii=False) + "\n")


def _describe(message: dict) -> str:
    """One line a human can read, so the trace does not need jq to be useful."""
    pieces = []
    for part in message.get("parts") or []:
        if not isinstance(part, dict):
            continue
        kind = str(part.get("part_kind", ""))
        if "tool" in kind:
            arrow = "->" if "return" in kind else "<-"
            body = part.get("content") or part.get("args")
            pieces.append(f"{arrow} {part.get('tool_name', '?')} {str(body)[:200]}")
        elif part.get("content"):
            pieces.append(str(part["content"])[:240])
    return " | ".join(pieces)


async def _work(gateway, agent, node_id: str, tree: Path, prompt: str):
    """Run the node until it marks its goal complete, or until the bound.

    One conversation, appended to. A node asked to continue must see what it already did — its own
    tool calls and their results — or it re-decides from nothing every round. The trace is that same
    conversation, written as it happens, which is the difference between reading what an agent did
    and re-running experiments to infer it.
    """
    trace = tree / "trace.jsonl"
    completion = tool_module.Completion()
    catalogue = tool_module.catalog(
        agent.tools, tree=tree, sandbox=tool_module.new_sandbox(),
        timeout_seconds=agent.timeout_seconds, completion=completion)
    history: tuple[dict, ...] = ()
    turn = 0
    while True:
        ask = prompt if turn == 0 else _continue_prompt(prompt, node_id, tree, turn)
        _trace(trace, turn=turn, kind="asked", text=ask)
        response = await gateway.generate_with_tools(
            prompt=ask, system_prompt=agent.instructions, tools=catalogue,
            request_limit=agent.request_limit, message_history=history or None)
        for message in response.messages[len(history):]:
            _trace(trace, turn=turn, kind=message.get("kind", "message"),
                   summary=_describe(message), message=message)
        history = response.messages
        if completion.done:
            _trace(trace, turn=turn, kind="completed", summary=completion.summary)
            break
        turn += 1
        if turn > (agent.max_turns or DEFAULT_MAX_TURNS):
            _trace(trace, turn=turn, kind="gave_up",
                   summary=f"the goal was never marked complete after {turn} rounds")
            break
    return response, completion


async def run(path: str | Path, objective: str | None = None, *, work: str | Path,
              config_path: str | Path, secrets: object | None = None) -> list[NodeResult]:
    graph = graph_module.load(path)
    work = Path(work)
    work.mkdir(parents=True, exist_ok=True)
    secrets = secrets or _secrets(config_path)
    done: dict[str, NodeResult] = {}
    results: list[NodeResult] = []

    for node_id in graph.order():
        agent = graph.agents[graph.nodes[node_id]]
        tree = work / node_id
        _seed(tree, [done[source].tree for source in graph.inputs[node_id]])
        gateway = PydanticAIModelGateway(_profile(config_path, agent.model), secrets)
        try:
            response, completion = await _work(
                gateway, agent, node_id, tree,
                _prompt(graph, node_id, objective or graph.objective, done))
        finally:
            await gateway.close()
        result = NodeResult(node_id=node_id, agent=graph.nodes[node_id], tree=tree,
                            text=completion.summary or response.text,
                            files=_files(tree) + ("trace.jsonl",),
                            completed=completion.done)
        done[node_id] = result
        results.append(result)
        print(json.dumps({"node": node_id, "agent": result.agent, "files": list(result.files),
                          "completed": result.completed,
                          "input_tokens": response.input_tokens,
                          "output_tokens": response.output_tokens,
                          "reply": result.text.strip()[:300]}, ensure_ascii=False), flush=True)
    return results


def _config(config_path: str | Path) -> dict:
    return json.loads(Path(config_path).read_text(encoding="utf-8"))


def _profile(config_path: str | Path, model_ref: str):
    from anchor.runtime.capabilities import ModelProfile

    for item in _config(config_path)["models"]:
        if item["ref"] == model_ref:
            return ModelProfile.model_validate(item)
    raise ValueError(f"no model named {model_ref!r} in {config_path}")


def _secrets(config_path: str | Path) -> object:
    config = _config(config_path)
    providers = [EnvironmentSecretProvider()]
    if config.get("secret_file"):
        providers.append(JsonFileSecretProvider(config["secret_file"]))
    return ChainedSecretProvider(*providers)


def new_work_dir(root: str | Path) -> Path:
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")
    return Path(root) / f"run-{stamp}-{uuid4().hex[:6]}"
