"""Run a graph: a workspace per node, walked in whatever order the edges allow, and resumable.

There is no fixed order. A node is ready when every edge into it has been decided *since its own last
run* and at least one was selected, so what runs next depends on what the nodes before it chose. A
node whose edges were all rejected is skipped, and the run ends when nothing is ready.

A node has one workspace, kept across the passes of a loop: a node revising its own work needs to see
what it wrote last time, and that is simply the directory it is already standing in. Nothing is copied
between nodes. What an edge carries is a pointer — the predecessor's workspace, mounted read-only in
the sandbox at `/in/<node>`, its git history included — so a node reads what it was given and cannot
write to it. Its own output is exactly what is in its own directory, which is why a node's files can be
attributed to it without keeping a list of what it was handed.

Each pass is frozen as a commit in the node's own repository, made here rather than in the sandbox:
the history is the record of the node's work, and a record the recorded thing can edit is not one. The
commit is what makes one pass readable after a later pass has written over it.

`run.json` is the whole of what a restart needs: where the run had got to, which edges it had decided,
what each node said when it finished, and which commit that was. The conversations are written beside
their workspaces as they happen, one per pass, so resuming a node means reading its messages back and
stepping again — no event history, no reconciliation, nothing to prove about what did not happen.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
from dataclasses import asdict, dataclass, field, replace
from datetime import datetime, timezone
from pathlib import Path

from anchor.simple import graph as graph_module
from anchor.simple.agent import build_agent

#: Turns a node may take before it is stopped. Not a target — a ceiling, so that a node which keeps
#: announcing completion instead of achieving it is stopped in minutes rather than in half an hour.
DEFAULT_MAX_STEPS = 60

#: Ways a node can run out of budget rather than get the work wrong. Running out of clock is not a
#: failed attempt: the conversation is good and continuing it is exactly the right response, so these
#: leave the run resumable instead of ending it.
BUDGET_EXITS = frozenset({"TimeExceeded", "LimitsExceeded"})

#: How many times one pass of one node may be started, counting resumes. Each is given a fresh
#: budget, so without a ceiling a node that never finishes would be resumed forever.
MAX_ATTEMPTS = 4

#: Who a node's history belongs to. Given rather than inherited, for the reason the repository's own
#: commit says: a record has an author, and it is not whoever happens to be logged in on this machine.
COMMIT_AUTHOR = ("Anchor", "anchor@localhost")

#: Read-only git, and no configuration of the node's own. GIT_OPTIONAL_LOCKS is what keeps a read a
#: read: without it `git status` refreshes the index, and on a read-only `.git` that turns reading
#: into a failure. The two CONFIG variables keep git from writing a `.gitconfig` — its home is the
#: node's workspace, so anything it writes there becomes part of what the next node is handed.
GIT_READ_ENV = ("GIT_OPTIONAL_LOCKS=0", "GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_GLOBAL=/dev/null")


def _git(workspace: Path, *arguments: str, check: bool = True) -> subprocess.CompletedProcess:
    """Run git on a node's workspace, as Anchor. Never from inside the sandbox."""
    author = ["-c", f"user.name={COMMIT_AUTHOR[0]}", "-c", f"user.email={COMMIT_AUTHOR[1]}"]
    return subprocess.run(["git", "-C", str(workspace), *author, *arguments],
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=check, text=True)


def _init_history(workspace: Path) -> None:
    """Start a node's history, and make it one the node can read from its first pass.
    The empty commit is what makes `git log` answer on a node that has not finished anything yet. An
    agent told to look at its own history and shown `does not have any commits yet` learns the tool is
    broken rather than that it is new.
    """
    if (workspace / ".git").exists():
        return
    _git(workspace, "init", "-q")
    _git(workspace, "commit", "-q", "--allow-empty", "-m", "start")


def _freeze(workspace: Path, message: str) -> str:
    """Commit this pass and return it. The commit is the record, so failing to make one is not a
    detail to swallow: a run whose passes point at nothing is a run nobody can read afterwards."""
    summary = " ".join((message or "(this node said nothing)").split()) or "(this node said nothing)"
    _git(workspace, "add", "-A")
    _git(workspace, "commit", "-q", "--allow-empty", "-m", summary[:2000])
    return _git(workspace, "rev-parse", "HEAD").stdout.strip()


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
    # This pass, frozen. The workspace is reused across passes, so the commit is what makes one pass
    # readable after a later one has written over it — and what a downstream node reads the history
    # through.
    commit: str = ""


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
    # keyed "{node}|{pass}" — how many times that pass has been started, resumes included.
    attempts: dict[str, int] = field(default_factory=dict)
    error: str = ""
    # Why a run that stopped did not simply finish. A run a node's round ceiling cut short did not
    # carry out the graph's intent, and calling that `finished` is the silent stop this rework exists
    # to stop making. Empty means the run ended for the ordinary reason: nothing was ready.
    reason: str = ""
    # One "{node}@{ceiling}" entry per pass the ceiling turned away, so a reader can see which loop
    # was cut off and not merely that something was.
    ceased: list[str] = field(default_factory=list)

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


def _files(tree: Path) -> tuple[str, ...]:
    return tuple(sorted(str(item.relative_to(tree)) for item in tree.rglob("*")
                        if item.is_file() and ".git" not in item.relative_to(tree).parts))


def _task(graph: graph_module.Graph, node_id: str, objective: str,
          sources: list[NodeResult], inputs: tuple[tuple[str, str], ...]) -> str:
    lines = [f"# Task\n\n{objective}"]
    if inputs:
        lines.append(
            "# What you were given\n\n"
            "Nothing was copied to you. These are the workspaces of the nodes before you, mounted "
            "read-only, one per node:\n\n"
            + "\n".join(f"    {mount}   ({node})" for (_, mount), node in
                        zip(inputs, (result.node_id for result in sources)))
            + "\n\nRead them with `cat`, `grep`, `find` or `git`. You cannot write to them — if you "
              "want to change something in one, copy it into your own workspace first.")
        for (_, mount), result in zip(inputs, sources):
            mark = "" if result.submitted else "  (this node did not submit)\n"
            lines.append(f"## {result.node_id}\n\n{mark}"
                         f"{result.submission.strip() or '(nothing said)'}")
            if result.files:
                lines.append("What it produced:\n" + "\n".join(f"  {name}" for name in result.files))
            lines.append(f"Its whole history is there too, one commit per pass:\n\n"
                         f"    git --git-dir={mount}/.git log --oneline\n"
                         f"    git --git-dir={mount}/.git show <commit>")
    lines.append(
        "# Your own workspace\n\n"
        "You work in `/workspace`, which is yours alone. It is kept between your passes, so if you "
        "have run before, what you left is still in it — look before you start over. Whatever is in "
        "it when you finish is the only thing the nodes after you will see, so the deliverable has to "
        "be a file rather than only something you say here.\n\n"
        "Your own history is in it as well, one commit per pass:\n\n"
        "    git -C /workspace log --oneline\n"
        "    git -C /workspace diff HEAD~1")
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
    # This pass's conversation, beside a workspace that is reused across passes.
    trace: Path = Path()
    # (where it lives, where it is visible) for everything this node was given, read-only.
    inputs: tuple[tuple[str, str], ...] = ()


def _mount_for(node_id: str) -> str:
    """Where a predecessor's workspace is visible inside this node's sandbox.
    Named for the node, so a module's nodes keep their scope: `/in/write/draft` is the draft node of
    the `write` module and not a node called `draft` somewhere else.
    """
    return f"/in/{node_id}"


def _incoming(graph: graph_module.Graph, state: RunState, decided: dict,
              node_id: str) -> list[NodeResult]:
    """What this node is given: the workspaces of the nodes whose edge into it was selected."""
    results = []
    for source in graph.in_edges[node_id]:
        if not decided.get((source, node_id), (False, -1))[0]:
            continue
        result = state.result(source)
        if result is not None:
            results.append(result)
    return results


def _trace_path(run_dir: Path, node_id: str, number: int) -> Path:
    """One conversation per pass. The workspace is reused; the conversation is not."""
    return run_dir / (f"{node_id}.trace.jsonl" if number == 1
                      else f"{node_id}-{number}.trace.jsonl")


def _next_step(graph: graph_module.Graph, state: RunState, decided: dict, run_dir: Path,
               order: list[str], ready) -> _Step | None:
    """The node to run now, or None when nothing can proceed.

    Two ways in: a cursor left by a process that died mid-node, which is continued; or a node whose
    inputs have arrived since it last ran, which is started. Everything else is refusing to start a
    node that has been round too many times.
    """
    cursor = state.cursor
    if cursor is not None:
        key = f"{cursor['node']}|{cursor['pass']}"
        state.attempts[key] = state.attempts.get(key, 0) + 1
        if state.attempts[key] > MAX_ATTEMPTS:
            # Continued as far as it is worth continuing. Each attempt had a fresh budget, so this
            # is a node that cannot finish rather than one that needs longer, and saying so beats
            # resuming it until somebody notices.
            state.status = "failed"
            state.error = (f"{cursor['node']} pass {cursor['pass']} was started "
                           f"{state.attempts[key] - 1} times without finishing")
            state.cursor = None
            state.save(run_dir)
            return None
        state.save(run_dir)
        node_id = cursor["node"]
        return _Step(node_id, cursor["pass"], Path(cursor["dir"]), None, True,
                     trace=_trace_path(run_dir, node_id, cursor["pass"]),
                     inputs=tuple((item.tree, _mount_for(item.node_id))
                                  for item in _incoming(graph, state, decided, node_id)))
    pending = [node for node in order if ready(node)]
    if not pending:
        return None
    node_id = pending[0]
    number = state.passes.get(node_id, 0) + 1
    if number > graph.ceiling(node_id):
        return _Step(node_id, number, Path(), None, False)
    # One directory per node, kept across its passes. A node revising its own work needs to see what
    # it wrote last time, and that is simply the directory it is already standing in.
    directory = run_dir / node_id
    directory.mkdir(parents=True, exist_ok=True)
    _init_history(directory)
    state.attempts[f"{node_id}|{number}"] = state.attempts.get(f"{node_id}|{number}", 0) + 1
    handed = _incoming(graph, state, decided, node_id)
    inputs = tuple((item.tree, _mount_for(item.node_id)) for item in handed)
    state.passes[node_id] = number
    state.seq += 1
    state.last_seq[node_id] = state.seq
    state.cursor = {"node": node_id, "pass": number, "dir": str(directory)}
    state.save(run_dir)             # written before the work starts, not after
    return _Step(node_id, number, directory, _task(graph, node_id, state.objective, handed, inputs),
                 False, trace=_trace_path(run_dir, node_id, number), inputs=inputs)


def _ready(graph: graph_module.Graph, state: RunState, decided: dict,
           back: frozenset, entry: str, node_id: str) -> bool:
    """Whether this node should run now.
    Two separate questions, and conflating them cost a loop that silently did not happen:
      is anything still to come?  Every incoming edge must be decided, except a back-edge whose
        source has never run — that is a cycle that has not started rather than an input pending.
      is there anything new?      At least one incoming edge must be selected by an execution newer
        than this node's own last run.
    The second question cannot be asked of every edge the way the first can. Once a node has run, its
    other inputs are necessarily older than it is, so requiring them all to be newer means a node can
    never run twice. And a node that routes to itself has an incoming edge written by its own
    execution, which is why a decision is stamped strictly after the execution that made it rather
    than at the same moment.
    """
    if node_id == entry and node_id not in state.passes:
        return True
    sources = graph.in_edges[node_id]
    if not sources:
        return node_id not in state.passes
    since = state.last_seq.get(node_id, -1)
    fresh = False
    for source in sources:
        key = (source, node_id)
        if key not in decided:
            if key in back and source not in state.passes:
                continue                            # the cycle it belongs to has not started
            return False                            # an input still to come
        selected, when = decided[key]
        if selected and when > since:
            fresh = True
    return fresh


def _record(state: RunState, graph: graph_module.Graph, run_dir: Path,
            decided: dict, result: NodeResult, settle) -> bool:
    """Store what a node left behind and resolve its ways out.
    Returns whether the run may continue. A node that never submitted stops it: carrying on would
    select no edge, skip everything downstream and report `finished`, which is a failure wearing the
    shape of a success. That mistake is what this whole rework is about, so it is checked here rather
    than left to be noticed.
    """
    if not result.submitted and result.exit_status in BUDGET_EXITS:
        # Out of clock, not out of ideas. The node keeps its cursor so a resume re-enters the same
        # pass and continues the same conversation, and the edges stay undecided so nothing
        # downstream moves on the strength of work that did not happen.
        state.executed.append(result.node_id)
        state.error = f"{result.node_id} ran out of budget: {result.exit_status}"
        state.save(run_dir)
        print(json.dumps({"node": result.node_id, "pass": result.pass_number,
                          "agent": result.agent, "ran_out": result.exit_status,
                          "resumable": True}, ensure_ascii=False), flush=True)
        return True
    # Frozen before it is recorded, so what `run.json` points at exists by the time it says so. A
    # budget exit above returns before this: what it left is partial work in progress, and committing
    # it would file it as a result — the next attempt continues from the same directory instead.
    result = replace(result, commit=_freeze(Path(result.tree), result.submission))
    state.nodes[result.node_id] = {**asdict(result), "files": list(result.files)}
    state.executed.append(result.node_id)
    state.cursor = None
    ways = graph.routes(result.node_id)
    chosen = result.route if len(ways) > 1 else (ways[0] if ways else None)
    # Through `settle`, not by stamping `state.seq` here. That was a second copy of the same rule and
    # it stamped with the sequence the execution *started* at, so a node routing to itself wrote an
    # edge that looked older than itself: the loop was dropped and the run said `finished`. The
    # ceiling path had the fix and this one did not.
    settle(result.node_id, chosen)
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


def _agent_for(graph, node_id: str, directory: Path, models: dict, secret_file, config_path,
               inputs: tuple[tuple[str, str], ...] = (), trace: Path | None = None):
    node = graph.nodes[node_id]
    spec = graph.agents[node.agent]
    model = models.get(spec.model)
    if model is None:
        raise ValueError(f"no model named {spec.model!r} in {config_path}")
    # What this use adds to what the role already says. A role is declared once so it can be used
    # more than once, and two uses that differ only in their framing differ here.
    instructions = (f"{spec.instructions}\n\n{node.with_}" if spec.instructions and node.with_
                    else spec.instructions or node.with_)
    return build_agent(
        tree=directory, node_id=node_id, routes=graph.routes(node_id),
        instructions=instructions,
        model_name=f"openai/{model['model']}" if model.get("base_url") else model["model"],
        model_kwargs={"api_base": model["base_url"], "api_key": _secret(secret_file, model),
                      "max_tokens": model.get("max_tokens", 8192)},
        # Ten minutes for one command, not five. A batch of literature searches is legitimately
        # slow — a dozen queries at twenty seconds each is already four minutes — and a batch that
        # is killed at five throws away everything it had done.
        network=spec.network, timeout_seconds=600.0,
        max_steps=spec.max_steps or DEFAULT_MAX_STEPS,
        wall_time_limit_seconds=spec.wall_time_limit_seconds,
        inputs=inputs, trace=trace,
    )


def run(workspace: str | Path, *, objective: str | None = None, config_path: str | Path,
        run_id: str | None = None, resume: str | Path | None = None) -> RunState:
    workspace = Path(workspace).resolve()
    graph_path = workspace / "graph.json"
    graph = graph_module.load(graph_path)
    # Which graph this run is actually reading, as a digest. A workspace owns its own copy, so
    # editing the one in the repository changes nothing about a workspace that already has one —
    # which cost a long run and a wrong conclusion about the model before anyone thought to look.
    digest = hashlib.sha256(graph_path.read_bytes()).hexdigest()[:12]
    print(json.dumps({"graph": str(graph_path), "digest": digest,
                      "entry": graph.entry(), "nodes": len(graph.nodes)}), flush=True)
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
    # The graph as this run read it — every module already inlined, every node naming its agent.
    # Written rather than referenced, so a run can be read without the workspace still holding the
    # file it came from, and so which module a node belongs to is answerable from the record alone.
    (run_dir / "graph.json").write_text(
        json.dumps(graph_module.to_dict(graph), ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
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
        # Stamped after this execution, not with it. A node that routes to itself writes an edge into
        # itself, and stamping it with the same number as the run that produced it makes it look like
        # an input older than the node — which is how a review that asked for another round got
        # skipped instead, and the run reported success having quietly dropped the loop.
        state.seq += 1
        for target in graph.routes(node_id):
            decided[(node_id, target)] = (target == chosen, state.seq)
        state.decided = {f"{source}|{target}": [value[0], value[1]]
                         for (source, target), value in decided.items()}
    try:
        while True:
            step = _next_step(graph, state, decided, run_dir, order, ready)
            if step is None:
                break
            if not step.resuming and step.number > graph.ceiling(step.node_id):
                settle(step.node_id, None)
                state.ceased.append(f"{step.node_id}@{graph.ceiling(step.node_id)}")
                state.save(run_dir)
                print(json.dumps({"node": step.node_id, "stopped": "max_rounds",
                                  "limit": graph.ceiling(step.node_id)}), flush=True)
                continue
            agent = _agent_for(graph, step.node_id, step.directory, models, secret_file, config_path,
                               inputs=step.inputs, trace=step.trace)
            if step.resuming:
                outcome = agent.resume(_messages(step.trace))
            else:
                outcome = agent.run(task=step.task)
            result = _result_of(step.node_id, graph.nodes[step.node_id].agent, step.directory,
                                step.number, outcome)
            result = replace(result, route=getattr(agent.env, "route", None))
            if not _record(state, graph, run_dir, decided, result, settle):
                print(json.dumps({"run": str(run_dir), "status": state.status,
                                  "stopped_at": result.node_id}, ensure_ascii=False), flush=True)
                return state
        state.skipped = [node for node in order if node not in state.passes]
        if state.status == "running":
            # Only if nothing else has already decided otherwise: a node that used up its attempts
            # sets `failed`, and the end of the loop is not the place to disagree with it.
            if state.ceased:
                # A ceiling that turned a pass away means the graph's intent was not carried out, so
                # this is not `finished`. The distinction is the whole point: the earlier halves of
                # this runtime stopped runs and said nothing about it.
                state.status = "stopped"
                state.reason = "max_rounds"
            else:
                state.status = "finished"
    except Exception as exc:  # noqa: BLE001 - recorded, so a restart can pick the run up
        state.status = "interrupted"
        state.error = f"{type(exc).__name__}: {exc}"
        state.save(run_dir)
        raise
    state.save(run_dir)
    print(json.dumps({"run": str(run_dir), "status": state.status,
                      "reason": state.reason,
                      "executed": state.executed, "skipped": state.skipped},
                     ensure_ascii=False), flush=True)
    return state


def _messages(trace: Path) -> list[dict]:
    """The conversation a previous process left, read back whole."""
    return [json.loads(line) for line in trace.read_text(encoding="utf-8").splitlines() if line]
