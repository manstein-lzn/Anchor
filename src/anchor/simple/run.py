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
import io
import json
import shutil
import subprocess
import tarfile
from collections.abc import Callable
from typing import Any
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
    # What this pass was handed, as (node, commit). The pointers are pinned, so naming them here is
    # what makes "what did this node read" answerable rather than reconstructable.
    inputs: tuple[tuple[str, str], ...] = ()


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
    # Rounds within the current entry into this node's scope, which is what the ceiling compares
    # against: counting is per level, so re-entering a module starts its nodes' rounds again.
    passes: dict[str, int] = field(default_factory=dict)
    # Every run of every node, never reset. Names this pass's conversation, because a number that
    # restarts would name the same trace file twice and the second would overwrite the first.
    runs: dict[str, int] = field(default_factory=dict)
    # How many times each module has been entered, keyed by scope. A module's `max_rounds` is how
    # many times its parent may enter it, and at that level the module is just a node.
    activations: dict[str, int] = field(default_factory=dict)
    last_seq: dict[str, int] = field(default_factory=dict)
    decided: dict[str, list] = field(default_factory=dict)
    # The latest pass of each node, which is what an edge resolves to.
    nodes: dict[str, dict] = field(default_factory=dict)
    # Every pass, keyed "{node}|{commit}". `nodes` overwrites, so without this a pointer to an
    # earlier pass could name a commit and nothing could say what that commit was handed — which is
    # exactly what following a line of work needs.
    history: dict[str, dict] = field(default_factory=dict)
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

    @staticmethod
    def _as_result(data: dict) -> NodeResult:
        return NodeResult(**{**data, "files": tuple(data["files"]),
                             "inputs": tuple(tuple(item) for item in data.get("inputs", ()))})

    def result(self, node_id: str) -> NodeResult | None:
        data = self.nodes.get(node_id)
        return self._as_result(data) if data else None

    def pass_result(self, node_id: str, commit: str) -> NodeResult:
        """One named pass, or a refusal. A pointer that cannot be resolved is not a missing file:
        it means the record and the pointer disagree, and carrying on would mount nothing and say
        nothing."""
        data = self.history.get(f"{node_id}|{commit}")
        if data is None:
            raise RuntimeError(f"no record of {node_id} at commit {commit[:12]}, so the pointer to "
                               f"it cannot be resolved")
        return self._as_result(data)


def _now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _files(tree: Path) -> tuple[str, ...]:
    return tuple(sorted(str(item.relative_to(tree)) for item in tree.rglob("*")
                        if item.is_file() and ".git" not in item.relative_to(tree).parts))


def _task(graph: graph_module.Graph, node_id: str, objective: str,
          sources: list[NodeResult], inputs: tuple[_Given, ...]) -> str:
    # Empty for an agent node. Its presence is what makes the two kinds one function: everything
    # above this line — the task, what was given, what can be reached — is the same for both.
    node = graph.nodes[node_id]
    node_op = graph.ops[node.op].run if node.op else ""
    lines = [f"# Task\n\n{objective}"]
    reached = [item for item in inputs if not item.direct]
    inputs = tuple(item for item in inputs if item.direct)
    if inputs:
        lines.append(
            "# What you were given\n\n"
            "Nothing was copied to you. Each of these is another node's work at one exact commit, "
            "mounted read-only:\n\n"
            + "\n".join(f"    {item.mount}   ({item.node_id} at {item.commit[:12]})"
                        for item in inputs)
            + "\n\nRead them with `cat`, `grep` or `find`. You cannot write to them — if you want to "
              "change something in one, copy it into your own workspace first.")
        for result, item in zip(sources, inputs):
            mark = "" if result.submitted else "  (this node did not submit)\n"
            lines.append(f"## {result.node_id}\n\n{mark}"
                         f"{result.submission.strip() or '(nothing said)'}")
            if result.files:
                lines.append("What it produced:\n" + "\n".join(f"  {name}" for name in result.files))
            lines.append(f"Its whole history is there too, one commit per pass — what you were "
                         f"given is {item.commit[:12]}:\n\n"
                         f"    git --git-dir={item.mount}/.git log --oneline\n"
                         f"    git --git-dir={item.mount}/.git show <commit>")
    if reached:
        lines.append(
            "# What you can reach\n\n"
            "The work those were built from is mounted read-only as well. **You are not expected to "
            "read it** — it is there so that you can look when the task calls for it:\n\n"
            + "\n".join(f"    {item.mount}   ({item.node_id} at {item.commit[:12]})"
                        for item in reached)
            + "\n\nEach is at the commit it had when it fed this line of work, not its latest. "
              "`git --git-dir=<mount>/.git log` reads the rest of that node's history.")
    if node_op:
        lines.append(
            "# What this node is\n\n"
            "An **op**, not an agent: it runs one command and nothing else, and there is no model "
            "in this pass. The command is\n\n"
            f"    {node_op}\n\n"
            "It runs in `/workspace`. **Its exit code is the verdict**: 0 finishes this pass and its "
            "output is what the pass says it did, and anything else fails the pass outright — there "
            "is nothing to correct and no turns to spend. What it was given is mounted read-only as "
            "described above, so it reads its inputs where they are rather than looking for them in "
            "its own directory.")
        if len(graph.routes(node_id)) > 1:
            lines.append(
                "This node chooses where the graph goes, so the command has to name it: finish by "
                f"running `anchor-route --to <{'|'.join(graph.routes(node_id))}> --reason \"…\"`, "
                "or by printing that line itself.")
        return "\n\n".join(lines)
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


def _result_of(node_id: str, agent, directory: Path, number: int, outcome: dict,
               given: tuple[_Given, ...] = ()) -> NodeResult:
    """What a node leaves behind: its words, its files, and how it got out."""
    return NodeResult(node_id=node_id, agent=agent, tree=str(directory), pass_number=number,
                      inputs=tuple((item.node_id, item.commit) for item in given),
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
    # What this node was given: predecessors' work, each pinned to the commit it was frozen at.
    inputs: tuple[_Given, ...] = ()
    # Set when a ceiling turned this pass away. The step carries it rather than the loop re-deriving
    # it: there are two ceilings now — the node's own, and the one its parent put on entering the
    # module it belongs to — and a sentinel the loop had to recognise twice was one it got wrong once.
    refused: str = ""


@dataclass(frozen=True)
class _Given:
    """One predecessor's work, pinned to the commit it was frozen at.

    A pointer to a commit rather than to a directory. A directory is a live thing that its owner will
    write to again — on its next pass, or, once nodes may run at the same time, while this one is
    reading it. A commit cannot move, so what a node read stays answerable afterwards and the same
    input gives the same run.
    """

    node_id: str
    commit: str
    mount: str
    tree: Path
    # Given by an edge this node selected, rather than reached by following the work back. The
    # distinction is only about what the prompt pushes: both are mounted and both are readable.
    direct: bool = True

    def binds(self) -> tuple[tuple[str, str], ...]:
        """The tree at that commit, and the repository behind it so the history stays readable."""
        return ((str(self.tree), self.mount), (str(self.tree / ".git"), f"{self.mount}/.git"))


def _materialize(repo: Path, commit: str, into: Path) -> Path:
    """Write one commit's tree out, so what gets mounted is that commit and not the live directory.

    From the commit rather than the working tree, which is also the honest reading of `.gitignore`: a
    file a node chose not to commit is a file it chose not to hand on.
    """
    if into.is_dir():
        return into
    # Built beside the name it will take, and moved into place only once it is whole. A half-written
    # view left under the real name is returned by the guard above on the next call and mounted as if
    # it were that commit, which is the one way this can go wrong without saying so.
    staging = into.with_name(into.name + ".building")
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True)

    def failed(completed: subprocess.CompletedProcess) -> str:
        shutil.rmtree(staging, ignore_errors=True)
        detail = completed.stderr.decode("utf-8", errors="replace").strip() or "no stderr"
        return f"cannot read commit {commit[:12]} of {repo}: {detail}"

    listing = subprocess.run(["git", "-C", str(repo), "ls-tree", "-r", "--name-only", commit],
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if listing.returncode != 0:
        raise RuntimeError(failed(listing))
    # An empty tree is not an empty tar. `git archive` answers one that Python's `tarfile` refuses
    # outright (`end of file header`), so the archive is asked for only when there is something in it.
    # A node that wrote nothing is an ordinary node — one that only routed — and it still gets a view.
    if listing.stdout.strip():
        archive = subprocess.run(["git", "-C", str(repo), "archive", "--format=tar", commit],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
        if archive.returncode != 0:
            raise RuntimeError(failed(archive))
        with tarfile.open(fileobj=io.BytesIO(archive.stdout)) as tar:
            # `tar` and not `data`: the stricter filter refuses a symlink to an absolute path
            # outright, so one node leaving `ln -s /usr/bin/python3 .` behind would make the next
            # node fail to start, reported as a tar error about a link. A symlink is part of the
            # snapshot and is kept; the archive-level guards (`..`, absolute member paths) stay.
            tar.extractall(staging, filter="tar")
    # Made here, not by the sandbox: a mount point cannot be created inside a read-only bind, and the
    # history is mounted over this.
    (staging / ".git").mkdir(exist_ok=True)
    staging.rename(into)
    return into


def _given(run_dir: Path, result: NodeResult, *, direct: bool = True) -> _Given:
    """Where a pointer lands, and what it points at."""
    if not result.commit:
        raise RuntimeError(f"{result.node_id} has no commit, so there is nothing to point at")
    name = f"{result.node_id.replace('/', '_')}-{result.commit[:12]}"
    tree = _materialize(Path(result.tree), result.commit, run_dir / ".views" / name)
    # The mount is named for the node, so a module's nodes keep their scope: `/in/write/draft` is the
    # draft node of the `write` module, not a node called `draft` somewhere else.
    return _Given(node_id=result.node_id, commit=result.commit,
                  mount=f"/in/{result.node_id}", tree=tree, direct=direct)


def _handed(run_dir: Path, graph: graph_module.Graph, state: RunState, node_id: str,
            direct: list[NodeResult]) -> tuple[_Given, ...]:
    """Everything a node is handed: its inputs, and what those inputs were themselves built from.

    A node downstream of a pipeline needs the plan as well as the draft, and under pointers nothing
    carries it along the chain — the node in the middle would have to copy it forward, which is the
    pass-through this design removed, done by hand and by memory.

    The following stops at a **back edge**. A back edge says the loop came round again, so what it
    carries is the loop's *current* state, and that state has already superseded the round it came
    from. Following one pulls in every earlier round: measured on a three-node loop, what a node is
    handed was 3 commits in round one, 9 by round three and 30 by round ten — unbounded in how long
    the run has been going, which is not a property anyone wants the prompt to have. Stopping at the
    back edge makes it constant, and what remains is bounded by the shape of the graph instead.
    """
    back = graph_module.back_edges(graph)
    position = {key: index for index, key in enumerate(state.history)}

    # One mount per node, and the newest pass wins. A later pass supersedes an earlier one, and two
    # bindings at the same path leave whichever bubblewrap applied last: the first run of this had
    # `work/check` handed `work/draft@2` and also reaching `work/draft@1` through the critique it
    # wrote the round before, so it read v1, asked for another pass, and the loop never converged.
    out: dict[str, _Given] = {}
    stack: list[NodeResult] = []
    for result in direct:
        out[result.node_id] = _given(run_dir, result, direct=True)
        stack.append(result)
    while stack:
        current = stack.pop()
        for source, commit in current.inputs:
            if source == node_id:
                # Its own earlier pass. The node is standing in that workspace, and the passes before
                # this one are in its own history, so a pointer to itself would be a second, older
                # copy of the thing it is already working on.
                continue
            if (source, current.node_id) in back:
                continue                    # the loop's current state, not a line of work to re-read
            existing = out.get(source)
            if existing is not None and (
                    existing.direct
                    or position.get(f"{source}|{existing.commit}", -1) >= position.get(f"{source}|{commit}", -1)):
                continue
            upstream = state.pass_result(source, commit)
            out[source] = _given(run_dir, upstream, direct=False)
            stack.append(upstream)
    return tuple(out.values())


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


def _trace_path(run_dir: Path, node_id: str, run_number: int) -> Path:
    """One conversation per run. The workspace is reused; the conversation is not.

    Keyed by how many times the node has run, not by its round within the current entry: a round
    number restarts when a module is re-entered, and the second run would have overwritten the
    first's conversation under the same name.
    """
    return run_dir / (f"{node_id}.trace.jsonl" if run_number == 1
                      else f"{node_id}-{run_number}.trace.jsonl")


def _enters_scope(graph: graph_module.Graph, state: RunState, decided: dict, node_id: str) -> bool:
    """Whether this run is an entry into the node's module from outside it.

    An edge from a sibling is the same visit; a crossing edge is a new one, and a new entry is what
    starts the module's counting again. It has to be a crossing edge *newer than this node's last
    run*: an edge is selected once and stays selected, so the edge that first led in here is still
    selected on every later pass, and counting it every time would make every pass a new visit.

    The root scope is entered once and nothing can cross into it, which is what makes its ceiling
    bound the whole run.
    """
    scope = graph_module.scope_of(node_id)
    inside = scope + graph_module.SEP
    since = state.last_seq.get(node_id, -1)
    for source in graph.in_edges[node_id]:
        selected, when = decided.get((source, node_id), (False, -1))
        if selected and when > since and not source.startswith(inside):
            return True
    return False


def _restart_scope(state: RunState, scope: str) -> None:
    """Start this module's counting again, and every module inside it.

    A module entered twice is two visits, and the inner ones are new visits too — otherwise the outer
    loop spends the inner loop's budget, which is the shape a nested loop fails in.
    """
    inside = scope + graph_module.SEP
    for node_id in state.passes:
        if node_id.startswith(inside):
            state.passes[node_id] = 0
    for nested in state.activations:
        if nested.startswith(inside):
            state.activations[nested] = 0


def _next_step(graph: graph_module.Graph, state: RunState, decided: dict, run_dir: Path,
               order: list[str], ready) -> _Step | None:
    """The node to run now, or None when nothing can proceed.

    Two ways in: a cursor left by a process that died mid-node, which is continued; or a node whose
    inputs have arrived since it last ran, which is started. Everything else is refusing to start a
    node that has been round too many times.
    """
    cursor = state.cursor
    if cursor is not None:
        key = f"{cursor['node']}|{cursor['run']}"
        state.attempts[key] = state.attempts.get(key, 0) + 1
        if state.attempts[key] > MAX_ATTEMPTS:
            # Continued as far as it is worth continuing. Each attempt had a fresh budget, so this
            # is a node that cannot finish rather than one that needs longer, and saying so beats
            # resuming it until somebody notices.
            state.status = "failed"
            state.error = (f"{cursor['node']} run {cursor['run']} was started "
                           f"{state.attempts[key] - 1} times without finishing")
            state.cursor = None
            state.save(run_dir)
            return None
        state.save(run_dir)
        node_id = cursor["node"]
        return _Step(node_id, cursor["pass"], Path(cursor["dir"]), None, True,
                     trace=_trace_path(run_dir, node_id, cursor["run"]),
                     inputs=_handed(run_dir, graph, state, node_id,
                                    _incoming(graph, state, decided, node_id)))
    # A node the ceiling already turned away stays turned away. Settling its out-edges is not enough
    # on its own: in a cycle of two or more, the node is still "fresh" through the edge coming back
    # into it, so it is chosen again, turned away again, and the run spins printing `stopped` forever.
    # A self-loop happens to escape this because settling its edge is what makes it unready.
    turned_away = set(state.ceased)
    pending = [node for node in order
               if ready(node)
               and f"{node}@{graph.ceiling(node)}" not in turned_away
               and f"{graph_module.scope_of(node)}@{graph.module_rounds.get(graph_module.scope_of(node), -1)}"
               not in turned_away]
    if not pending:
        return None
    node_id = pending[0]
    if _enters_scope(graph, state, decided, node_id):
        scope = graph_module.scope_of(node_id)
        entry = state.activations.get(scope, 0) + 1
        allowed = graph.module_rounds.get(scope)
        if allowed is not None and entry > allowed:
            # The parent has entered this module as often as it said it might. Refused the way any
            # other ceiling refuses: the entry node is turned away, so nothing inside runs and the
            # module produces no exit for whatever comes after it.
            state.cursor = None
            return _Step(node_id, state.passes.get(node_id, 0) + 1, Path(), None, False,
                         refused=f"{scope}@{allowed}")
        # Counted only once it is allowed, so the number is visits that happened rather than visits
        # that were asked for.
        state.activations[scope] = entry
        _restart_scope(state, scope)
    number = state.passes.get(node_id, 0) + 1
    if number > graph.ceiling(node_id):
        return _Step(node_id, number, Path(), None, False,
                     refused=f"{node_id}@{graph.ceiling(node_id)}")
    # One directory per node, kept across its passes. A node revising its own work needs to see what
    # it wrote last time, and that is simply the directory it is already standing in.
    directory = run_dir / node_id
    directory.mkdir(parents=True, exist_ok=True)
    _init_history(directory)
    run_number = state.runs.get(node_id, 0) + 1
    state.attempts[f"{node_id}|{run_number}"] = \
        state.attempts.get(f"{node_id}|{run_number}", 0) + 1
    # `handed` is what the edges gave, which is what the prompt pushes; `inputs` is everything
    # mounted, which is what it can reach.
    handed = _incoming(graph, state, decided, node_id)
    inputs = _handed(run_dir, graph, state, node_id, handed)
    state.passes[node_id] = number
    state.runs[node_id] = run_number
    state.seq += 1
    state.last_seq[node_id] = state.seq
    state.cursor = {"node": node_id, "pass": number, "run": run_number, "dir": str(directory)}
    state.save(run_dir)             # written before the work starts, not after
    return _Step(node_id, number, directory, _task(graph, node_id, state.objective, handed, inputs),
                 False, trace=_trace_path(run_dir, node_id, run_number), inputs=inputs)


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
    state.history[f"{result.node_id}|{result.commit}"] = state.nodes[result.node_id]
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
               inputs: tuple[_Given, ...] = (), trace: Path | None = None,
               script: list[str] | None = None):
    node = graph.nodes[node_id]
    if node.op:
        # An op is a command. No model profile, no secret, one turn, and its exit code is the
        # verdict — everything else about the node is what it is for an agent.
        op = graph.ops[node.op]
        return build_agent(
            tree=directory, node_id=node_id, routes=graph.routes(node_id), instructions="",
            network=op.network, timeout_seconds=600.0, max_steps=1,
            wall_time_limit_seconds=op.wall_time_limit_seconds,
            inputs=tuple(bind for item in inputs for bind in item.binds()), trace=trace,
            op=op.run)
    spec = graph.agents[node.agent]
    # A scripted node needs no model profile and no secret: nothing is being called.
    model_name, model_kwargs = "", {}
    if script is None:
        model = models.get(spec.model)
        if model is None:
            raise ValueError(f"no model named {spec.model!r} in {config_path}")
        model_name = f"openai/{model['model']}" if model.get("base_url") else model["model"]
        model_kwargs = {"api_base": model["base_url"], "api_key": _secret(secret_file, model),
                        "max_tokens": model.get("max_tokens", 8192)}
    # What this use adds to what the role already says. A role is declared once so it can be used
    # more than once, and two uses that differ only in their framing differ here.
    instructions = (f"{spec.instructions}\n\n{node.with_}" if spec.instructions and node.with_
                    else spec.instructions or node.with_)
    return build_agent(
        tree=directory, node_id=node_id, routes=graph.routes(node_id),
        instructions=instructions,
        model_name=model_name, model_kwargs=model_kwargs,
        # Ten minutes for one command, not five. A batch of literature searches is legitimately
        # slow — a dozen queries at twenty seconds each is already four minutes — and a batch that
        # is killed at five throws away everything it had done.
        network=spec.network, timeout_seconds=600.0,
        max_steps=spec.max_steps or DEFAULT_MAX_STEPS,
        wall_time_limit_seconds=spec.wall_time_limit_seconds,
        inputs=tuple(bind for item in inputs for bind in item.binds()), trace=trace, script=script,
    )


def _cease(state: RunState, step: _Step, settle: Any, run_dir: Path) -> None:
    """Record that a ceiling turned this pass away, and say so.

    A graph whose intent was not carried out is not `finished`; the distinction is the whole reason the
    earlier halves of this runtime used to stop runs without saying anything about it.
    """
    settle(step.node_id, None)
    state.ceased.append(str(step.refused))
    state.save(run_dir)
    print(json.dumps({"node": step.node_id, "stopped": "max_rounds",
                      "what": step.refused}), flush=True)


def _asked_to_stop(asked: Callable[[], str | None] | None, state: RunState,
                  run_dir: Path) -> RunState | None:
    """The state to return when a caller has asked the run to stop, or `None` to carry on.

    Left where it is, with the edges it has decided and the nodes it has run, so a continue picks up from
    exactly here rather than starting over.
    """
    if asked is None:
        return None
    status = asked()
    if status is None:
        return None
    state.status = status
    state.reason = "asked"
    state.save(run_dir)
    print(json.dumps({"run": str(run_dir), "status": status, "reason": "asked"},
                     ensure_ascii=False), flush=True)
    return state


def _settled_already(asked: Callable[[str], tuple[str, str | None] | None] | None,
                     step: _Step) -> NodeResult | None:
    """The pass to record **without running the node**, when its record says it already submitted.

    Split out so `run` stays readable: one decision, one place, and the branch it replaces was four lines
    of result-building in the middle of the scheduler.
    """
    if asked is None:
        return None
    settled = asked(step.node_id)
    if settled is None:
        return None
    submission, route = settled
    result = NodeResult(node_id=step.node_id, agent="", tree=str(step.directory),
                        pass_number=step.number,
                        inputs=tuple((item.node_id, item.commit) for item in step.inputs),
                        submission=submission, files=_files(step.directory), submitted=True,
                        route=route, exit_status="Submitted")
    return result


# The branch count is the scheduler's shape: stop requests, refusals, settled nodes, resuming, and the
# loop's own exits. Three of those were extracted to helpers while adding the settled-node seam and the
# count did not move, because what the metric counts is the conditions. A deliberate exception, and the
# report says so.
def run(workspace: str | Path, *, objective: str | None = None, config_path: str | Path,   # noqa: C901
        run_id: str | None = None, resume: str | Path | None = None,
        model_script: dict[str, list[str]] | None = None,
        stop_request: Callable[[], str | None] | None = None,
        already_submitted: Callable[[str], tuple[str, str | None] | None] | None = None) -> RunState:
    """Walk the graph. `model_script` replaces the model with written-down commands, per node.

    `stop_request` is asked between nodes whether the run should stop, and answers with the status to
    stop under or None to carry on. **Between nodes, not during one**: a node mid-flight is inside a
    sandbox command or a model call and nothing here can reach into it, so a stop lands when the node
    that is running finishes. Saying so is better than a button that appears not to work.

    **`already_submitted` is the seam a node's own record needs.** A node that submitted and was killed
    before this loop recorded its pass leaves a completion behind, and nothing in the loop could ask: the
    gap between `agent.run(task=...)` returning and `_record(...)` has no hook, so the scheduler ran the
    node again from the start. Given this callable, the loop asks **before** running a node, records the
    pass from what it is told, and does not run the node at all. It answers `(submission, route)` for a
    node that has finished, and `None` for one that has not — and it is the caller's, because where a
    node's record lives is the node's business and not the graph's.
    """

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
        # Why the *previous* attempt stopped is not why this one might. A pause sets `reason` so the
        # record says why it left off, and without clearing it here a run that paused, resumed and
        # finished kept saying `asked` — which the view reads as "stopped on request".
        state.reason = ""
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
            stopped = _asked_to_stop(stop_request, state, run_dir)
            if stopped is not None:
                return stopped
            step = _next_step(graph, state, decided, run_dir, order, ready)
            if step is None:
                break
            if step.refused:
                _cease(state, step, settle, run_dir)
                continue
            # **Asked before the node runs.** A node whose completion is already recorded has
            # submitted, and running it again is the duplicate the whole recovery path exists to avoid:
            # the pass is recorded from what the record says instead.
            settled = _settled_already(already_submitted, step)
            if settled is not None:
                if not _record(state, graph, run_dir, decided, settled, settle):
                    print(json.dumps({"run": str(run_dir), "status": state.status,
                                      "stopped_at": settled.node_id}, ensure_ascii=False), flush=True)
                    return state
                continue
            agent = _agent_for(graph, step.node_id, step.directory, models, secret_file, config_path,
                               inputs=step.inputs, trace=step.trace,
                               script=None if model_script is None else model_script.get(step.node_id))
            # **A cursor without a trace means the node never actually started.** The scheduler writes
            # the cursor before dispatching, so a kill in between leaves a node marked as interrupted
            # with nothing to continue from — and resuming reads a trace file that was never written,
            # which fails the whole run rather than running the node. Started fresh is the honest reading:
            # nothing of it happened.
            if step.resuming and step.trace is not None and Path(step.trace).exists():
                outcome = agent.resume(_messages(step.trace))
            else:
                outcome = agent.run(task=step.task)
            result = _result_of(step.node_id, graph.nodes[step.node_id].agent, step.directory,
                                step.number, outcome, step.inputs)
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
