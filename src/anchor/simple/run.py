"""Run a graph: a directory per node, walked in whatever order the edges allow, and resumable.

There is no fixed order. A node is ready when every edge into it has been decided *since its own last
run* and at least one was selected, so what runs next depends on what the nodes before it chose. A
node whose edges were all rejected is skipped, and the run ends when nothing is ready.

Every execution gets its own directory, named for the node and then for which pass it is, because a
graph with a loop runs the same node more than once and those are different attempts at the same
work. A node starts from a copy of whatever the nodes that fed it left behind.

`run.json` is the whole of what a restart needs: where the run had got to, which edges it had
decided, and what each node said when it finished. The conversations are written beside their nodes
as they happen, so resuming a node means reading its messages back and stepping again — no event
history, no reconciliation, nothing to prove about what did or did not happen.
"""

from __future__ import annotations

import json
import shutil
from dataclasses import asdict, dataclass, field, replace
from datetime import datetime, timezone
from pathlib import Path

from anchor.simple import graph as graph_module
from anchor.simple.agent import build_agent

IGNORED = shutil.ignore_patterns(".git", "__pycache__")

#: Turns a node may take before it is stopped. Not a target — a ceiling, so that a node which keeps
#: announcing completion instead of achieving it is stopped in minutes rather than in half an hour.
DEFAULT_MAX_STEPS = 60


@dataclass(frozen=True)
class NodeResult:
    node_id: str
    agent: str
    tree: str
    pass_number: int
    submission: str
    files: tuple[str, ...]
    submitted: bool
    exit_status: str
    route: str | None = None


@dataclass
class RunState:
    """What survives a restart. Everything else is on disk in the directories already."""

    objective: str
    started: str
    status: str = "running"
    updated: str = ""
    # Set while a node is running and cleared when it finishes, so a restart knows both that
    # something was in flight and exactly what it was.
    cursor: dict | None = None
    passes: dict[str, int] = field(default_factory=dict)
    last_seq: dict[str, int] = field(default_factory=dict)
    decided: dict[str, list] = field(default_factory=dict)
    nodes: dict[str, dict] = field(default_factory=dict)
    executed: list[str] = field(default_factory=list)
    skipped: list[str] = field(default_factory=list)
    seq: int = 0
    error: str = ""

    def save(self, run_dir: Path) -> None:
        self.updated = _now()
        (run_dir / "run.json").write_text(
            json.dumps(asdict(self), ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    @classmethod
    def load(cls, run_dir: Path) -> "RunState":
        return cls(**json.loads((run_dir / "run.json").read_text(encoding="utf-8")))

    def result(self, node_id: str) -> NodeResult | None:
        data = self.nodes.get(node_id)
        return NodeResult(**{**data, "files": tuple(data["files"])}) if data else None


def _now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


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
                     "That is the only way this node finishes — `anchor-done` is not accepted "
                     "here, because this node chooses where the graph goes. The reason is recorded; "
                     "nothing parses it.")
    else:
        lines.append("# How this node finishes\n\nWhen the work is done, run this and nothing "
                     "after it:\n\n    anchor-done --summary \"what you did, and what you "
                     "could not do\"")
    return "\n\n".join(lines)


def _result_of(node_id: str, agent, directory: Path, number: int, outcome: dict) -> NodeResult:
    """What a node leaves behind: its words, its files, and how it got out."""
    return NodeResult(node_id=node_id, agent=agent, tree=str(directory), pass_number=number,
                      submission=str(outcome.get("submission") or ""), files=_files(directory),
                      submitted=outcome.get("exit_status") == "Submitted",
                      route=getattr(agent, "route", None),
                      exit_status=str(outcome.get("exit_status") or ""))


@dataclass(frozen=True)
class _Step:
    """The node about to run: which one, which pass, where, and what it is told."""

    node_id: str
    number: int
    directory: Path
    task: str | None            # None when continuing a conversation that already exists
    resuming: bool


def _next_step(graph: graph_module.Graph, state: RunState, decided: dict, run_dir: Path,
               order: list[str], ready) -> _Step | None:
    """The node to run now, or None when nothing can proceed.

    Two ways in: a cursor left by a process that died mid-node, which is continued; or a node whose
    inputs have arrived since it last ran, which is started. Everything else is refusing to start a
    node that has been round too many times.
    """
    cursor = state.cursor
    if cursor is not None:
        return _Step(cursor["node"], cursor["pass"], Path(cursor["dir"]), None, True)
    pending = [node for node in order if ready(node)]
    if not pending:
        return None
    node_id = pending[0]
    number = state.passes.get(node_id, 0) + 1
    if number > graph.max_rounds:
        return _Step(node_id, number, Path(), None, False)
    directory = run_dir / (node_id if number == 1 else f"{node_id}-{number}")
    incoming = [state.result(source) for source in graph.in_edges[node_id]
                if decided.get((source, node_id), (False, -1))[0]]
    _seed(directory, [Path(item.tree) for item in incoming if item])
    state.passes[node_id] = number
    state.seq += 1
    state.last_seq[node_id] = state.seq
    state.cursor = {"node": node_id, "pass": number, "dir": str(directory)}
    state.save(run_dir)             # written before the work starts, not after
    return _Step(node_id, number, directory,
                 _task(graph, node_id, state.objective, [item for item in incoming if item]), False)


def _ready(graph: graph_module.Graph, state: RunState, decided: dict,
           back: frozenset, entry: str, node_id: str) -> bool:
    """Whether this node's inputs have arrived since it last ran, and at least one is selected.

    An undecided edge holds it back — except a back-edge whose source has never run, which is a cycle
    that has not started rather than an input still to come. Without that exception the node just
    after the entry waits forever for an edge its own downstream has not had the chance to decide,
    and the run reports success having done one node.
    """
    if node_id == entry and node_id not in state.passes:
        return True
    sources = graph.in_edges[node_id]
    if not sources:
        return node_id not in state.passes
    since = state.last_seq.get(node_id, -1)
    selected = []
    for source in sources:
        key = (source, node_id)
        if key in decided:
            if decided[key][1] <= since:
                return False                        # nothing new since this node last ran
            selected.append(decided[key][0])
        elif key in back and source not in state.passes:
            continue                                # the cycle it belongs to has not started
        else:
            return False
    return any(selected)


def _record(state: RunState, graph: graph_module.Graph, run_dir: Path,
            decided: dict, result: NodeResult) -> bool:
    """Store what a node left behind and resolve its ways out.

    Returns whether the run may continue. A node that never submitted stops it: carrying on would
    select no edge, skip everything downstream and report `finished`, which is a failure wearing the
    shape of a success. That mistake is what this whole rework is about, so it is checked here rather
    than left to be noticed.
    """
    state.nodes[result.node_id] = {**asdict(result), "files": list(result.files)}
    state.executed.append(result.node_id)
    state.cursor = None
    ways = graph.routes(result.node_id)
    chosen = result.route if len(ways) > 1 else (ways[0] if ways else None)
    for target in ways:
        decided[(result.node_id, target)] = (target == chosen, state.seq)
    state.decided = {f"{source}|{target}": [value[0], value[1]]
                     for (source, target), value in decided.items()}
    if not result.submitted:
        state.status = "failed"
        state.error = (f"{result.node_id} did not submit: {result.exit_status}")
    state.save(run_dir)
    print(json.dumps({"node": result.node_id, "pass": result.pass_number, "agent": result.agent,
                      "submitted": result.submitted, "route": chosen,
                      "exit_status": result.exit_status, "files": list(result.files)},
                     ensure_ascii=False), flush=True)
    return result.submitted


def _config(config_path: str | Path) -> tuple[dict, str | None]:
    raw = json.loads(Path(config_path).read_text(encoding="utf-8"))
    return {item["ref"]: item for item in raw.get("models", [])}, raw.get("secret_file")


def _secret(secret_file: str | None, model: dict) -> str:
    from anchor.runtime.secrets import (
        ChainedSecretProvider,
        EnvironmentSecretProvider,
        JsonFileSecretProvider,
    )

    providers: list = [EnvironmentSecretProvider()]
    if secret_file:
        providers.append(JsonFileSecretProvider(secret_file))
    return ChainedSecretProvider(*providers).get(model["secret_ref"])


def _agent_for(graph, node_id: str, directory: Path, models: dict, secret_file, config_path):
    spec = graph.agents[graph.nodes[node_id]]
    model = models.get(spec.model)
    if model is None:
        raise ValueError(f"no model named {spec.model!r} in {config_path}")
    return build_agent(
        tree=directory, node_id=node_id, routes=graph.routes(node_id),
        instructions=spec.instructions,
        model_name=f"openai/{model['model']}" if model.get("base_url") else model["model"],
        model_kwargs={"api_base": model["base_url"], "api_key": _secret(secret_file, model),
                      "max_tokens": model.get("max_tokens", 8192)},
        network=spec.network, timeout_seconds=300.0,
        max_steps=spec.max_steps or DEFAULT_MAX_STEPS,
        wall_time_limit_seconds=spec.wall_time_limit_seconds,
    )


def run(workspace: str | Path, *, objective: str | None = None, config_path: str | Path,
        run_id: str | None = None, resume: str | Path | None = None) -> RunState:
    workspace = Path(workspace).resolve()
    graph = graph_module.load(workspace / "graph.json")
    models, secret_file = _config(config_path)

    if resume is not None:
        run_dir = Path(resume).resolve()
        state = RunState.load(run_dir)
        state.status = "running"
    else:
        run_dir = workspace / "runs" / (run_id or datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S"))
        run_dir.mkdir(parents=True, exist_ok=True)
        state = RunState(objective=objective or graph.objective, started=_now())
        state.save(run_dir)

    # A node that is already recorded as not having submitted means the run stopped without
    # finishing. Checked before anything runs, because afterwards the scheduler has no reason to
    # revisit it — it would find nothing ready and report `finished`, which is the failure in the
    # shape of a success that this rework exists to stop making.
    unfinished = [node for node, data in state.nodes.items() if not data.get("submitted")]
    if unfinished:
        state.status = "failed"
        state.error = f"{', '.join(sorted(unfinished))} did not submit (exit_status " \
                      f"{state.nodes[unfinished[0]].get('exit_status')})"
        state.save(run_dir)
        print(json.dumps({"run": str(run_dir), "status": state.status,
                          "stopped_at": unfinished[0], "error": state.error},
                         ensure_ascii=False), flush=True)
        return state

    decided = {tuple(key.split("|")): tuple(value) for key, value in state.decided.items()}
    entry = graph.entry()
    back = graph_module.back_edges(graph)
    order = list(graph.nodes)

    def ready(node_id: str) -> bool:
        return _ready(graph, state, decided, back, entry, node_id)

    def settle(node_id: str, chosen: str | None) -> None:
        for target in graph.routes(node_id):
            decided[(node_id, target)] = (target == chosen, state.seq)
        state.decided = {f"{source}|{target}": [value[0], value[1]]
                         for (source, target), value in decided.items()}

    try:
        while True:
            step = _next_step(graph, state, decided, run_dir, order, ready)
            if step is None:
                break
            if not step.resuming and step.number > graph.max_rounds:
                settle(step.node_id, None)
                state.save(run_dir)
                print(json.dumps({"node": step.node_id, "stopped": "max_rounds",
                                  "limit": graph.max_rounds}), flush=True)
                continue

            agent = _agent_for(graph, step.node_id, step.directory, models, secret_file, config_path)
            if step.resuming:
                trace = step.directory.parent / f"{step.directory.name}.trace.jsonl"
                outcome = agent.resume(_messages(trace))
            else:
                outcome = agent.run(task=step.task)

            result = _result_of(step.node_id, graph.nodes[step.node_id], step.directory,
                                step.number, outcome)
            result = replace(result, route=getattr(agent.env, "route", None))
            if not _record(state, graph, run_dir, decided, result):
                print(json.dumps({"run": str(run_dir), "status": state.status,
                                  "stopped_at": result.node_id}, ensure_ascii=False), flush=True)
                return state

        state.skipped = [node for node in order if node not in state.passes]
        state.status = "finished"
    except Exception as exc:  # noqa: BLE001 - recorded, so a restart can pick the run up
        state.status = "interrupted"
        state.error = f"{type(exc).__name__}: {exc}"
        state.save(run_dir)
        raise
    state.save(run_dir)
    print(json.dumps({"run": str(run_dir), "status": state.status,
                      "executed": state.executed, "skipped": state.skipped},
                     ensure_ascii=False), flush=True)
    return state


def _messages(trace: Path) -> list[dict]:
    """The conversation a previous process left, read back whole."""
    return [json.loads(line) for line in trace.read_text(encoding="utf-8").splitlines() if line]
