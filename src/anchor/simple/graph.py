"""A workspace is a graph: the structure, the permissions, and every run it has had.

    <workspace>/
      graph.json          the nodes, the edges between them, and what each agent may do
      runs/
        2026-09-13T22-30-00/
          plan/  gather/  write/  review/
        2026-09-13T22-30-00/

The graph lives in the workspace rather than beside it, because the workspace is the unit: point at
one and everything a run needs is there. Runs accumulate beside each other and are never merged —
there is no state carried from one to the next, so the history is a record rather than a dependency.

An edge says where the graph may go, not when. A node with one way out follows it; a node with more
than one names its choice, and the edge is selected or rejected accordingly. Routing is a decision
the node makes, so it does not have to express it as data.

**A graph can contain graphs.** A file declares agents once, declares any number of graphs beside
them, and a node may be one of those graphs instead of an agent. What a run executes is the
*expansion* of that: every module inlined, its nodes named for the module they came from
(`review/draft`), one flat graph with no modules left in it. The file stays the thing a person edits
and the expansion is the thing a run reads.

Expansion rather than nesting at run time, for one reason above the others: a node id is a directory
name, so `review/draft` lands in `runs/<run>/review/draft/` and the filesystem mirrors the structure
the author drew. Nothing in the runner has to know a module exists. It also makes identity immediate —
a module is inlined into the file, so the file's own digest covers which module it was, with no
version to declare.

A module is not a namespace for agents. There is one agent pool per file, so a role is defined once
and referenced from anywhere, which is the whole point of declaring it separately from the nodes.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path

#: Separates a module's name from the name of a node inside it, once the module is inlined. A name
#: an author writes may not contain it: `a/b` written by hand and "node b of module a" would be the
#: same string, and nothing downstream could tell them apart.
SEP = "/"

#: How many times a node may run in one pass when nothing says otherwise.
DEFAULT_MAX_ROUNDS = 3


@dataclass(frozen=True)
class Interface:
    """What a node says it reads and what it promises to write, in file names inside its workspace.

    The only interface there is. A node is handed files and leaves files, whether a model or a
    program did the work, so the same declaration describes both and the same check applies to both.
    """
    reads: tuple[str, ...] = ()
    writes: tuple[str, ...] = ()


@dataclass(frozen=True)
class Agent(Interface):
    model: str = ""
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
class Op(Interface):
    """A node's work when no model is involved: one command, and its exit code is the verdict.

    The same thing as an agent in every way the graph can see — a workspace of its own, a read-only
    pointer to what came before, one commit per run, one action that finishes it — and different in
    exactly one: what decides the work is done is a program rather than a model. A model can talk
    itself into believing it has finished. `grep -q '^## References' paper.md` cannot.

    A command rather than a function, and not for convenience: everything a node runs has to run
    *inside the sandbox*, and an in-process function would run outside it, with the host in reach.
    What the command is written in is the author's business — a console script, a python file, a line
    of shell are the same thing here.
    """
    run: str = ""
    network: bool = False
    wall_time_limit_seconds: int = 3600


@dataclass(frozen=True)
class Node:
    """One node: a role or an op, in a scope, and whatever this use adds to what it already says."""

    id: str
    agent: str = ""
    op: str = ""
    # Appended to the role's instructions. Two nodes may share a role and differ here, which is what
    # makes a role worth declaring separately from the nodes that use it.
    with_: str = ""


@dataclass(frozen=True)
class Graph:
    nodes: dict[str, Node]
    agents: dict[str, Agent]
    ops: dict[str, Op]
    out_edges: dict[str, tuple[str, ...]]
    in_edges: dict[str, tuple[str, ...]]
    objective: str = ""
    # Where a run starts, already resolved through any modules.
    entry_node: str = ""
    # Each node carries the ceiling of the graph that declared it: a module's bound belongs to the
    # module, not to whoever includes it.
    max_rounds: dict[str, int] = field(default_factory=dict)
    # How many times each module may be entered, keyed by the scope its node id became. Counting is
    # per level: a graph's `max_rounds` bounds its own nodes' rounds, and a module node's bounds how
    # many times its parent enters it. Both restart at each entry, which is what makes a loop of
    # modules and a loop inside one compose without either spending the other's budget.
    module_rounds: dict[str, int] = field(default_factory=dict)

    def entry(self) -> str:
        """Where a run starts.

        Declared, or inferred when it is unambiguous. A graph with a loop has no node without an
        incoming edge, and one that does not loop usually has exactly one — so the inference is a
        convenience, not the rule. Resolved while the graph was expanded, so this cannot fail here.
        """
        return self.entry_node

    def routes(self, node_id: str) -> tuple[str, ...]:
        """The ways out of a node. More than one means the node must choose."""
        return self.out_edges.get(node_id, ())

    def ceiling(self, node_id: str) -> int:
        """How many times this node may run in one pass."""
        return self.max_rounds.get(node_id, DEFAULT_MAX_ROUNDS)

    def definition(self, node_id: str) -> Interface:
        """What this node runs: an agent or an op. One question, whichever it is."""
        node = self.nodes[node_id]
        return self.ops[node.op] if node.op else self.agents[node.agent]

    def reads(self, node_id: str) -> tuple[str, ...]:
        return self.definition(node_id).reads

    def writes(self, node_id: str) -> tuple[str, ...]:
        return self.definition(node_id).writes


def edges(graph: Graph) -> list[tuple[str, str]]:
    """Every edge, in the order the nodes and their targets were declared."""
    return [(node, target) for node, targets in graph.out_edges.items() for target in targets]


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
    return parse(json.loads(Path(path).read_text(encoding="utf-8")))


# -- naming ------------------------------------------------------------------------------------


def scope_of(node_id: str) -> str:
    """Which module a node belongs to. The empty string is the graph the run started from.

    A scope is a node at the level above — that is what a module node becomes when it is expanded —
    so a node id is a path and its scope is that path without its last segment.
    """
    return node_id.rsplit(SEP, 1)[0] if SEP in node_id else ""


def _check_name(name: object, kind: str, where: str) -> str:
    if not isinstance(name, str) or not name:
        raise ValueError(f"{where}: a {kind} name must be a non-empty string, not {name!r}")
    return name


def _check_node_id(node_id: object, where: str, *, allow_sep: bool) -> str:
    """A node id is a directory name once the graph runs, and a scope prefix once it expands.

    `SEP` is therefore reserved — except in a file that declares no graphs, which cannot be expanded
    into anything and so has nothing for one name to collide with. That is exactly the shape an
    already-expanded graph has, which is what lets a run write the graph it read back out as a file
    and have that file be a real one.
    """
    name = _check_name(node_id, "node", where)
    if SEP in name and not allow_sep:
        raise ValueError(
            f"{where}: the node name {name!r} contains {SEP!r}, which separates a module from its "
            f"contents once a graph is expanded — here that could be the same string as another "
            f"graph's node, and nothing downstream could tell them apart")
    return name


def _files(value: object, where: str, what: str) -> tuple[str, ...]:
    """File names an interface declares. Inside the workspace, and nothing else.

    A path that could point outside would make the declaration uncheckable and the check worthless:
    an interface that can name anything describes nothing.
    """
    if value is None:
        return ()
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ValueError(f"{where}: {what} must be a list of file names")
    for name in value:
        if not name or name.startswith("/") or ".." in name.split("/"):
            raise ValueError(
                f"{where}: {what} names {name!r}, which is not a path inside the workspace. A node "
                f"is handed files where it stands and leaves files where it stands.")
    return tuple(dict.fromkeys(value))


def _agent(name: str, spec: dict) -> Agent:
    where = f"agent {name!r}"
    if "model" not in spec:
        raise ValueError(f"{where} needs a \"model\"")
    return Agent(model=spec["model"], instructions=spec.get("instructions", ""),
                 network=bool(spec.get("network", False)),
                 max_steps=int(spec.get("max_steps", 0)),
                 wall_time_limit_seconds=int(spec.get("wall_time_limit_seconds", 3600)),
                 reads=_files(spec.get("reads"), where, "\"reads\""),
                 writes=_files(spec.get("writes"), where, "\"writes\""))


def _op(name: str, spec: dict) -> Op:
    where = f"op {name!r}"
    if not isinstance(spec, dict):
        raise ValueError(f"{where} must be a JSON object")
    run = spec.get("run")
    if not isinstance(run, str) or not run.strip():
        raise ValueError(
            f"{where} needs a \"run\": the command this node executes. An op is a command, so a "
            f"node that runs one without a command has nothing to do and would finish having done it.")
    return Op(run=run, network=bool(spec.get("network", False)),
              wall_time_limit_seconds=int(spec.get("wall_time_limit_seconds", 3600)),
              reads=_files(spec.get("reads"), where, "\"reads\""),
              writes=_files(spec.get("writes"), where, "\"writes\""))


def _check_node(item: object, where: str, *, allow_sep: bool) -> dict:
    if not isinstance(item, dict) or "id" not in item:
        raise ValueError(f"{where}: every node needs an \"id\": {item}")
    _check_node_id(item["id"], where, allow_sep=allow_sep)
    kinds = [key for key in ("agent", "op", "graph") if key in item]
    if len(kinds) > 1:
        raise ValueError(f"{where}: node {item['id']!r} has {', '.join(repr(k) for k in kinds)}; a "
                         f"node is one of them, and which one it is is the whole of what it is")
    if not kinds:
        raise ValueError(f"{where}: node {item['id']!r} needs an \"agent\", an \"op\" or a "
                         f"\"graph\"")
    has_graph = "graph" in item
    if has_graph and "with" in item:
        # Read by nothing: a module has no instructions of its own to add to. `max_rounds` is a
        # different matter and is allowed here — at this level the module *is* a node, so it has a
        # ceiling like any other, and its ceiling is how many times this graph may enter it.
        raise ValueError(
            f"{where}: node {item['id']!r} is a graph, so 'with' would have nothing to apply to — "
            f"it belongs on the nodes inside that graph")
    if "with" in item and "op" in item:
        # An op has no instructions to add to, and its parameters are written in its command. A
        # `with` here would be read by nothing, which is the thing this file refuses rather than
        # accepts quietly.
        raise ValueError(
            f"{where}: node {item['id']!r} is an op, so 'with' would have nothing to apply to — an "
            f"op is a command, and anything that varies per use is written in that command")
    if "with" in item and not isinstance(item["with"], str):
        raise ValueError(f"{where}: node {item['id']!r} has a non-string \"with\"")
    if "max_rounds" in item and int(item["max_rounds"]) < 1:
        raise ValueError(f"{where}: node {item['id']!r} has a \"max_rounds\" below 1")
    return item


def _infer_entry(body: dict, where: str) -> str:
    targets = {edge["to"] for edge in body.get("edges") or ()}
    starts = [item["id"] for item in body["nodes"] if item["id"] not in targets]
    if len(starts) != 1:
        raise ValueError(
            f"{where}: a graph needs an explicit \"entry\" node when it has a loop or several "
            f"starts; nodes with no incoming edge: {sorted(starts)}")
    return starts[0]


def _check_body(body: object, where: str, *, module: bool) -> dict:
    """One graph body: the file's own, or one declared in its `graphs` block.

    The two differ in exactly one direction. A module may not declare what the file declares, and
    must say where its result comes from, because that is where its parent's edges attach.
    """
    if not isinstance(body, dict):
        raise ValueError(f"{where} must be a JSON object")
    if module:
        for key in ("agents", "ops", "objective", "graphs"):
            if key in body:
                raise ValueError(
                    f"{where} declares {key!r}, and only the file does. There is one agent pool per "
                    f"file so a role is defined once and referenced from anywhere, and one objective "
                    f"per run because that is the task every node is answering — a module that "
                    f"carried its own would have nowhere to put it.")
        if not body.get("exit"):
            raise ValueError(
                f"{where} needs an \"exit\": it names the node whose directory is this module's "
                f"result, and the parent's edges leaving this module attach to it.")
    if not body.get("nodes"):
        raise ValueError(f"{where} needs at least one node")
    # A file with a `graphs` block expands, so its own node ids share one namespace with the
    # modules' and may not contain the separator. A file without one does not expand.
    allow_sep = not body.get("graphs") and not module
    ids = [_check_node(item, where, allow_sep=allow_sep)["id"] for item in body["nodes"]]
    if len(set(ids)) != len(ids):
        raise ValueError(f"{where} declares the same node id twice: {sorted(ids)}")
    seen: set[tuple[str, str]] = set()
    for edge in body.get("edges") or ():
        for end in ("from", "to"):
            if edge.get(end) not in ids:
                raise ValueError(f"{where}: edge names an unknown node: {edge}")
        pair = (edge["from"], edge["to"])
        if pair in seen:
            raise ValueError(f"{where}: the same edge is declared twice: {edge}")
        seen.add(pair)
    return body


def _require_known(references: list[tuple[str, str]], pool: dict[str, dict], where: str) -> None:
    for source, target in references:
        if target not in pool:
            raise ValueError(f"{where}: {source} refers to {target!r}, which this file does not "
                             f"declare: {sorted(pool)}")


def _reject_reference_cycles(pool: dict[str, dict], root: dict) -> None:
    """Which graph contains which, checked before anything is inlined.

    A graph that contains itself has no finite expansion, and no finite identity either: its content
    would include its own content. Execution cycles are a different thing and stay allowed — a node
    may route back to an earlier node, and does.

    The root is checked for references but not for cycles: it is not in the pool, so nothing can
    refer back to it.
    """
    state: dict[str, int] = {}          # 1 = on the current path, 2 = finished
    refers = {name: [item["graph"] for item in body["nodes"] if "graph" in item]
              for name, body in pool.items()}
    _require_known([(f"node {item['id']!r}", item["graph"])
                    for item in root["nodes"] if "graph" in item], pool, "graph")

    def visit(name: str, path: list[str]) -> None:
        state[name] = 1
        _require_known([(f"graph {name!r}", target) for target in refers[name]], pool, "graph")
        for target in refers[name]:
            if state.get(target) == 1:
                raise ValueError(
                    "graphs contain each other in a cycle: "
                    + " -> ".join([*path, name, target])
                    + ". A graph that contains itself has no finite expansion.")
            if state.get(target) is None:
                visit(target, [*path, name])
        state[name] = 2

    for name in pool:
        if state.get(name) is None:
            visit(name, [])


# -- expansion -----------------------------------------------------------------------------------


@dataclass(frozen=True)
class _Expansion:
    nodes: dict[str, Node]
    edges: list[tuple[str, str]]
    max_rounds: dict[str, int]
    module_rounds: dict[str, int]
    entry: str
    exit: str | None


def _expand(body: dict, pool: dict[str, dict], prefix: str) -> _Expansion:
    """Inline every module in one graph body, naming what comes from one after the module.

    `sides` is the whole trick: a local node contributes a pair of flattened names — where an edge
    arriving at it should attach, and where an edge leaving it should depart. For an agent node both
    are itself; for a module they are the module's own entry and exit. Edges are then rewritten to
    the right side of whatever they point at, and nothing downstream knows a module was there.
    """
    nodes: dict[str, Node] = {}
    max_rounds: dict[str, int] = {}
    module_rounds: dict[str, int] = {}
    edges: list[tuple[str, str]] = []
    sides: dict[str, tuple[str, str]] = {}
    ceiling = int(body.get("max_rounds", DEFAULT_MAX_ROUNDS))

    for item in body["nodes"]:
        flat = f"{prefix}{item['id']}"
        if "graph" in item:
            inner = _expand(pool[item["graph"]], pool, flat + SEP)
            nodes.update(inner.nodes)
            max_rounds.update(inner.max_rounds)
            module_rounds.update(inner.module_rounds)
            edges.extend(inner.edges)
            assert inner.exit is not None, "a module declares an exit, checked before expansion"
            # At this level the module is a node, so it carries a ceiling like any other: how many
            # times this graph may enter it. Defaulted from this body's, as every node's is.
            module_rounds[flat] = int(item.get("max_rounds", ceiling))
            sides[item["id"]] = (inner.entry, inner.exit)
        else:
            nodes[flat] = Node(id=flat, agent=item.get("agent", ""), op=item.get("op", ""),
                               with_=item.get("with", ""))
            max_rounds[flat] = int(item.get("max_rounds", ceiling))
            sides[item["id"]] = (flat, flat)

    for edge in body.get("edges") or ():
        # Leaving the source: its exit side. Arriving at the target: its entry side.
        edges.append((sides[edge["from"]][1], sides[edge["to"]][0]))

    entry = body.get("entry") or _infer_entry(body, "graph")
    exit_node = sides[body["exit"]][1] if body.get("exit") else None
    return _Expansion(nodes, edges, max_rounds, module_rounds, sides[entry][0], exit_node)


def feeders(graph: Graph, node_id: str) -> set[str]:
    """Every node whose output this one can be handed: its in-edges, and their forward lineage.

    The static form of what `_handed` does at run time, and a superset of it — a run selects one way
    out of each node, this follows all of them. So a file that is not here cannot arrive, and the
    check built on it refuses only graphs that cannot work.
    """
    back = back_edges(graph)
    out: set[str] = set()
    stack = list(graph.in_edges.get(node_id, ()))
    while stack:
        current = stack.pop()
        if current in out or current == node_id:
            continue
        out.add(current)
        for source in graph.in_edges.get(current, ()):
            if (source, current) in back:
                continue
            stack.append(source)
    return out


def _check_interfaces(graph: Graph) -> None:
    """A file a node says it reads has to be one that something it can be handed writes.

    This is the class of failure the runtime cannot report, because there is nothing wrong with it:
    a node whose input is wired to nothing reads nothing, does the work anyway, and submits. The
    `revise-loop` example was exactly that — `done` was told to copy `draft.md`, its only edge came
    from `review`, and the reviewer wrote only `review.md`. The graph loaded, the run finished, and
    the loop inside it had never read anything at all.

    Refused at load, where the author is, rather than left to be noticed in a transcript.
    """
    for node_id in graph.nodes:
        wanted = set(graph.reads(node_id))
        if not wanted:
            continue
        # Its own writes count: a node keeps its workspace between passes, so what it left last time
        # is there for it this time.
        produced = set(graph.writes(node_id)) | {name for source in feeders(graph, node_id)
                                                 for name in graph.writes(source)}
        missing = sorted(wanted - produced)
        if not missing:
            continue
        reachable = sorted(feeders(graph, node_id))
        handed = "; ".join(f"{name} writes {', '.join(graph.writes(name)) or 'nothing'}"
                           for name in reachable) or "nothing"
        raise ValueError(
            f"node {node_id!r} reads {', '.join(missing)}, and nothing it can be handed writes "
            f"{'them' if len(missing) > 1 else 'it'}. It can be handed: {handed}")


def parse(raw: dict) -> Graph:
    """Validate a graph, whether it came from a file or from someone typing it into a page.

    Raises with the reason rather than returning something half-built, so an editor can show the
    message next to what the author wrote. The same function guards both, so a graph the page accepts
    is a graph a run will accept.

    The result is the expanded graph: one flat set of nodes, each already naming its agent. Callers
    that want to draw what the author wrote read the file, not this.
    """
    where = "graph"
    body = _check_body(_root(raw), where, module=False)
    pool: dict[str, dict] = {}
    for name, module in (raw.get("graphs") or {}).items():
        _check_name(name, "graph", where)
        pool[name] = _check_body(module, f"graph {name!r}", module=True)
    _reject_reference_cycles(pool, body)

    expansion = _expand(body, pool, "")
    out_edges: dict[str, list[str]] = {node: [] for node in expansion.nodes}
    in_edges: dict[str, list[str]] = {node: [] for node in expansion.nodes}
    for source, target in expansion.edges:
        out_edges[source].append(target)
        in_edges[target].append(source)

    graph = Graph(nodes=expansion.nodes,
                  agents={name: _agent(name, spec) for name, spec in (raw.get("agents") or {}).items()},
                  ops={name: _op(name, spec) for name, spec in (raw.get("ops") or {}).items()},
                  out_edges={node: tuple(targets) for node, targets in out_edges.items()},
                  in_edges={node: tuple(sources) for node, sources in in_edges.items()},
                  objective=raw.get("objective", ""), entry_node=expansion.entry,
                  max_rounds=expansion.max_rounds, module_rounds=expansion.module_rounds)
    # Only the one it actually has: an agent node's `op` is empty and an op node's `agent` is, so
    # checking both unconditionally would report the empty string as an undeclared name.
    unknown = sorted(
        node.id for node in graph.nodes.values()
        if (node.agent and node.agent not in graph.agents)
        or (node.op and node.op not in graph.ops)
        or not (node.agent or node.op))
    if unknown:
        raise ValueError(f"nodes name an agent or an op that is not declared: {unknown}")
    graph.entry()          # fail at load rather than at the first step of a run
    _check_interfaces(graph)
    return graph


def _root(raw: object) -> dict:
    if not isinstance(raw, dict):
        raise ValueError("a graph must be a JSON object")
    if "nodes" not in raw:
        raise ValueError("a graph needs a \"nodes\" key")
    if not raw.get("agents") and not raw.get("ops"):
        raise ValueError("a graph needs an \"agents\" or an \"ops\" key with something in it")
    if not raw["nodes"]:
        raise ValueError("a graph needs at least one node")
    return raw


def to_dict(graph: Graph) -> dict:
    """The expanded graph as JSON: what a run actually read, in the shape a graph file has.

    Written into a run's directory so the run says which graph it ran without depending on the
    workspace still holding the file it came from.
    """
    return {
        "objective": graph.objective,
        "entry": graph.entry_node,
        "agents": {name: {"model": agent.model, "instructions": agent.instructions,
                          "network": agent.network, "max_steps": agent.max_steps,
                          "wall_time_limit_seconds": agent.wall_time_limit_seconds,
                          **({"reads": list(agent.reads)} if agent.reads else {}),
                          **({"writes": list(agent.writes)} if agent.writes else {})}
                   for name, agent in graph.agents.items()},
        "ops": {name: {"run": op.run, "network": op.network,
                       "wall_time_limit_seconds": op.wall_time_limit_seconds,
                       **({"reads": list(op.reads)} if op.reads else {}),
                       **({"writes": list(op.writes)} if op.writes else {})}
                for name, op in graph.ops.items()},
        "nodes": [{"id": node.id, **({"agent": node.agent} if node.agent else {"op": node.op}),
                   **({"with": node.with_} if node.with_ else {}),
                   "max_rounds": graph.ceiling(node.id)}
                  for node in graph.nodes.values()],
        "edges": [{"from": source, "to": target} for source, target in edges(graph)],
    }
