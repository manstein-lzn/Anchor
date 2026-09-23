# Anchor

Define an agent graph, run it, and each node works in its own directory inside a sandbox. That is
the whole thing.

```bash
mkdir -p /tmp/survey && cp examples/graphs/academic-simple.json /tmp/survey/graph.json
anchor-graph /tmp/survey --objective "写一篇综述"
```

`anchor-graph` takes the directory holding `graph.json`, not the graph file itself, and leaves its
runs in a `runs/` beside it.

A graph is a JSON file. A node is an **agent** or an **op**, and it has a workspace of its own.
An edge says which nodes' work a node starts from, and what it carries is **a pointer**: the
predecessor's workspace, mounted read-only in the sandbox at `/in/<node>`, its history included.
Nothing is copied. A node writes into its own workspace and whatever is in it when it finishes is what
the next node is pointed at.

## Start the development services

Use the repository script from a terminal or dialog:

```bash
./scripts/dev.sh              # start in the background
./scripts/dev.sh status
./scripts/dev.sh stop
./scripts/dev.sh restart
```

The script starts `anchor-serve` on `127.0.0.1:8077` and Vite on `127.0.0.1:5173`, detaches both from
the terminal, and keeps their PIDs and logs under `.local/dev/`. Closing the terminal or dialog does
not stop them. Future development should use this script rather than launching either process in the
foreground; use `status` before starting another copy.

## Deep academic research

`examples/graphs/deep-academic-research.json` maintains research in each node's workspace:
framing → investigation → independent challenge → writing → peer review → paper.
Challenge can return to investigation or framing. Peer review routes evidence gaps back to
investigation, invalid framing back to framing, and writing defects back to the author. Returning
nodes read the actual feedback under `/in/`; the reviewer first verifies its previous objections.
Optional improvements do not block delivery. The final artifact is `report/paper.md`, a scholarly
review with methods, mechanism-based analysis, limitations and traceable references.

There is no prescribed number of searches, research cycles or manuscript revisions. Omitted
`max_rounds` and omitted/null agent `max_steps` mean no execution-count budget; explicit limits
remain operator choices. `max_steps: 0` permits no model requests. Persisted explicit budgets
remain binding on recovery, even if the graph later omits a limit. Tool timeouts and invalid-output
retry handling remain separate from healthy research progress. Manual Stop remains available.

Run tests with the project interpreter, not system Python:

```bash
./.venv/bin/python -m pytest -q tests/test_examples.py tests/test_node_controlflow.py
```

The tests exercise feedback delivery and completion beyond the old limits using scripted models
in the real sandbox. They do not establish the quality of a real research paper. In WebUI, select
`deep-academic-research`, enter the research objective, and start a new run manually.

```json
{
  "objective": "what this graph is for, as the default task text",
  "agents": {
    "planner": {
      "model": "models.academic",
      "writes": ["plan.md"],
      "instructions": "You decide what to look for…"
    },
    "gatherer": {
      "model": "models.academic",
      "network": true,
      "reads": ["plan.md"],
      "writes": ["sources.md", "notes.md"],
      "instructions": "You gather academic evidence…"
    },
    "writer": {
      "model": "models.academic",
      "reads": ["notes.md"],
      "writes": ["paper.md"],
      "instructions": "You write the paper, References section included…"
    }
  },
  "ops": {
    "has-references": {
      "run": "grep -q '^## References' /in/write/paper.md",
      "reads": ["paper.md"]
    }
  },
  "nodes": [{"id": "plan", "agent": "planner"},
            {"id": "gather", "agent": "gatherer"},
            {"id": "write", "agent": "writer"},
            {"id": "check", "op": "has-references"}],
  "edges": [{"from": "plan", "to": "gather"},
            {"from": "gather", "to": "write"},
            {"from": "write", "to": "check"}]
}
```

An agent is a model loop that finishes by running `anchor-done` or `anchor-route`. An op is one
command and no model at all: **its exit code is the verdict**, and its output is what it says. Both
are nodes in every other respect — the same workspace, the same pointer, the same commit, the same
record — which is what makes an op a second kind of node rather than a second way of running one.

## A graph can contain graphs

A file declares its agents once, declares any number of graphs beside them, and a node runs one of
those graphs instead of an agent:

```json
{
  "agents": {"drafter": {"model": "models.academic", "instructions": "…"},
             "gatherer": {"model": "models.academic", "instructions": "…"}},
  "graphs": {
    "revise-a-draft": {
      "entry": "draft", "exit": "settle", "max_rounds": 4,
      "nodes": [{"id": "draft", "agent": "drafter"}, {"id": "settle", "agent": "drafter"}],
      "edges": [{"from": "draft", "to": "settle"}]
    }
  },
  "nodes": [{"id": "gather", "agent": "gatherer"}, {"id": "write", "graph": "revise-a-draft"}],
  "edges": [{"from": "gather", "to": "write"}]
}
```

**What a run reads is the expansion**, not the file: every module inlined, its nodes named for the
module they came from. `write/draft` and `write/settle` are ordinary nodes, so an edge into `write`
attaches to `write/draft` and an edge out of it leaves from `write/settle` — the module's `entry` and
`exit`.

**Rounds are counted per level.** A scope is a node at the level above — which is exactly what a
module node becomes when it is expanded — so a module node carries a `max_rounds` like any other, and
there it means how many times its parent may enter it. The `max_rounds` inside means how many rounds
each of the module's nodes may take *within one visit*. Both restart at each entry, which is what lets
a loop of modules and a loop inside one compose: an outer loop does not spend the inner loop's budget.
A graph whose inner loop needs both of its rounds and whose outer loop comes back still finishes.

Expansion rather than nesting at run time, for one reason above the others: **a node id is a directory
name**, so `write/draft` lands in `runs/<run>/write/draft/` and the filesystem mirrors the structure
the author drew. Nothing in the runner knows a module exists. It also settles identity without a
version to declare — a module is inlined into the file, so the file's own digest covers which module
it was.

Three things are refused, each because the alternative is a name meaning two things:

- **A graph containing itself.** No finite expansion, and no finite identity: its content would
  include its own content. Execution cycles stay allowed — a node may route back to an earlier one,
  and does.
- **`/` in a node id**, when the file declares graphs. `a/b` written by hand and "node b of module a"
  would otherwise be the same string. A file with no `graphs` block does not expand, so it may use
  the separator — which is what lets a run write out the expansion it read.
- **A module declaring `agents`, `ops`, `objective` or `graphs`.** There is one agent pool per file,
  so a role is defined once and referenced from anywhere; and one objective per run, because that is
  the task every node is answering.

A node may also carry `"with": "…"`, appended to its role's own instructions. That is what makes
declaring a role separately from its nodes worth doing: two nodes can share one and still be asked
for different things.

## The model, in one place

Seven statements. Each is a decision with a reason and a rejected alternative, and `DECISIONS.md` has
them from ADR-056 on; this is the shape they add up to.

**A node has one workspace, kept across its passes.** It is a directory of its own, and a node that
runs again finds what it left. Its own passes are in its own git history, so a node can read how its
work got to where it is.

**An edge carries a pointer to a commit, never a copy.** The predecessor's workspace is mounted
read-only at `/in/<node>`, at the commit that pass was frozen at, with the repository behind it. A
commit cannot move, so what a node read stays answerable and the same input gives the same run.
Nothing in the design requires an author to move a file from one node to another.

**A node can reach the work behind its inputs, and the following stops at a back edge.** A back edge
says the loop came round again, and the state it carries has already superseded the round it came
from; following one would re-mount every earlier round, so what a node is handed would grow with how
long the run had been going. What it is handed is therefore bounded by the shape of the graph. The
prompt pushes the inputs and merely indexes the rest: a node must understand what the previous hand
gave it, and everything before that is findable rather than required.

**A scope is a node one level up, and rounds are counted per level.** A module node's `max_rounds` is
how many times its parent may enter it; the `max_rounds` inside is how many rounds each of its nodes
may take within one visit. Nested loops therefore compose without either spending the other's budget.

**A node is an agent or an op, and both declare the same interface.** An agent is a model loop; an
op is one command whose exit code is the verdict. Everything else is shared — the workspace, the
pointer, the commit, the record — so an op is the same mechanism with a program deciding instead of a
model. Both declare `reads` and `writes`, and the reason is not documentation: a declaration can be
**checked**. At load, every file a node says it reads must be one that something it can be handed
writes. That is the class of failure the runtime cannot report — a node wired to nothing reads
nothing, does the work anyway, and submits — and it is refused where the author is, with the message
saying what would have worked.

An op is a command rather than a function because everything a node runs has to run inside the sandbox.
What the command is written in is the author's business: a console script, a python file and a line of
shell are the same thing here. `examples/graphs/academic-gated.json` is the shape — agents write, an
op decides, and the op's exit code is what sends the work back.

**Completion is an action, not a sentence.** A node cannot stop by talking. `anchor-done`, or
`anchor-route` when it chooses where the graph goes, is the only way a pass ends, so a turn that
merely says it is finished leaves the run resumable instead of recording a success.

**A run is exercised end to end with no provider.** `ANCHOR_MODEL_SCRIPT` replaces the model and
nothing else — the loop, the sandbox, the mounts, the commits and the record stay real — so a change
to any of them can be checked without paying a provider for it. `tests/test_loop_provider_free.py`
runs a loop inside a module inside a loop that way.

Two habits come with these, and both were learned by getting them wrong:

- **A field, a file or a promise that nothing reads is worse than one that is refused.** Several of
  the bugs here were things that were written and never consulted, or consulted in two places that
  disagreed with each other.
- **When a run is in flight, follow the node's trace.** `runs/<id>/<node>.trace.jsonl` says what a
  node is doing from its first turn. Two thousand wasted model turns happened while nobody watched it.

## What is ours and what is not

**Ours:** the graph, the directories, the sandbox, and the literature tools.

**Not ours:** the agent loop. That is [PydanticAI](https://github.com/pydantic/pydantic-ai) with
`pydantic-ai-harness` (MIT), and it is the only one: ADR-062's migration finished at M5, and the loop
it replaced, [mini-swe-agent](https://github.com/SWE-agent/mini-swe-agent), is out of the tree along
with the second completion parser that came with it. Keeping a fallback would have been a second
implementation that rots untested. The reason the loop is theirs at all: a loop that cannot be
stopped by talking is a property of the loop, and writing one that has that property is harder than it
looks. Theirs has it — a response without a command is a format error and is retried, and a run ends
only when a command asks for submission. In a CLI, someone follows up when an agent stops halfway. In
a node, nobody does — so the node must not be able to stop that way.

The loop is behind `run_agent_node(request, *, model, capabilities=())`, which returns a
`NodeOutcome`: the graph hands over a request and reads back a status, and never sees a harness
message or a checkpoint. ADR-062 freezes that seam, the statuses and the budget rule; the runner
behind it changes without the graph changing.

## The sandbox

Every command a node runs goes through `bwrap`:

- the node's own directory is writable, everything else is read-only
- the network is off unless the node's agent says `"network": true`
- the node's PATH carries our console scripts — `anchor-scholarly` among them, and `anchor-done` /
  `anchor-route`, which are how a node finishes

An allowlist of commands was considered and rejected: the loop runs commands through a shell, so the
shell is the entry point and the sandbox is the boundary. An allowlist in front of a shell is a
second, weaker boundary that the shell steps around.

## The literature tools

```bash
anchor-scholarly search      --query "learned cost models" [--source crossref|arxiv|openalex] [--limit 8] [--offset 0]
anchor-scholarly search-many --queries-file queries.txt [--budget 420]
anchor-scholarly sources
anchor-scholarly read        --url "https://arxiv.org/pdf/2401.00001" [--offset 0] [--page-start 0]
anchor-scholarly read-many   --urls "u1,u2,u3"
anchor-scholarly citations   --identifier 2401.00001 [--direction cited_by|cites]
```

JSON on stdout; a failure exits non-zero and says why on stderr, which is the only failure signal a
shell-using agent can act on. Sources are rate limited and sometimes refuse, and that is information
rather than a dead end — an agent that hits a 429 can try another source, and the trace will show it
did. A long document does not fit in one answer: `read` returns a `next_offset` (and, for a PDF, a
`next_page_start`), and passing it back is how the rest of the paper is reached.

They are a command on purpose. It keeps keys, rate limiting and the SSRF check inside a process we
control rather than inside a sandbox, and it means the same tools work from a shell, from any agent
body, and from ours.

## Where a run leaves things

```
<workspace>/graph.json
<workspace>/runs/<run id>/
  run.json              where the run got to, what each node said, and the commit it left
  graph.json            the graph this run actually read, every module already inlined
  <node>/               the node's own workspace — and its own git repository
  control/<node>/       the node's own record: its step store, its budget, its completion fact
  .views/<node>-<c>/    one predecessor's tree, written out at the commit it was pinned to
  <node>.trace.jsonl    the conversation of the node's first pass
  <node>-2.trace.jsonl  and of its second, if it ran again
```

A node's workspace is kept across the passes of a loop, so a node revising its own work finds it still
there. Each pass is frozen as a commit in that node's repository, made by Anchor rather than by the
node: the history is the record of the node's work, and a node can read it — `git log`, `git show`,
`git diff HEAD~1` — but `.git` is mounted read-only, so it cannot rewrite it. The commit message is the
node's own summary, so its log is the chain of what it said it was doing.

`run.json` names each pass's commit, which is what makes a pass readable after a later one has written
over it. The run directory carries the graph it read for the same reason: the run can be read without
the workspace still holding the file it came from. `control/` is the node's own business — the
scheduler knows one path (`control/<node>`) and nothing about what is inside it.

The trace sits beside its node's workspace, never inside it, and there is one per pass. Inside it is a
file the agent can read, and one did: it found its own conversation, concluded that nothing prior
existed except the trace file, and reasoned about that instead of its task. Two passes in one file
would replay as a conversation with two beginnings, which is not the one either of them had.

`<node>.trace.jsonl` is the debugging surface. Without it, what an agent did has to be inferred by
re-running it; with it, "it searched 47 times and adapted around a source that kept refusing" is a
thing you read.

## What is not here

No runs database, no leases, no graph versions, no approval gates, no evidence ledger, no context
engine, no services. Those existed and were removed: they were built before anything ran end to end,
and what they mostly did was make runs stop without saying so.

What survives of recovery is reading the run back and stepping again — `--resume`, and `anchor-serve`
restarting the runs it finds unfinished. Two records are read rather than one: `run.json` says what the
scheduler recorded, and `control/<node>` says what the node itself recorded, and when they disagree
about whether a node finished the run **stops and says so** instead of choosing — the two readings lead
to opposite actions, and guessing between them runs a pass twice or never runs it. The canvas in
`apps/web` is a view of the same `graph.json` a run reads, and `anchor-serve` publishes it and the runs
together. None of this is the machinery that was removed: there is no lease to reconcile and no version
to publish.

`DECISIONS.md` keeps the record of that, including the parts that were mistakes, and `OPEN.md` holds
what is still undecided — with the arguments on each side and with what has already been refused, so
none of it has to be worked out twice.

**Not built yet, as against deliberately absent**, and worth keeping apart when deciding what to do
next:

- **Nothing waits for the outside world.** A node's files all come from inside the graph, so a run is
  either working or over. An **input op** — one whose workspace is filled by a trigger, and whose run
  is therefore `waiting` rather than finished — is the next step, and it is what a graph that answers
  a question or answers an alert needs.
- **A node never caches.** Every pass runs. The commit is the record of what a pass produced, not a key
  for skipping one — so a graph re-run from the top redoes everything.
- **Nodes run one at a time.** The design admits parallel ones: a pointer names a commit rather than a
  live directory, so a node cannot be disturbed by a neighbour writing while it reads. The scheduler
  walks one node at a time and does not use that yet.
- **A run starts from a call, not from an event.** `anchor-serve` has `POST /trigger` and answers `409`
  while that graph is already running; nothing watches for a change and starts one.

- **The canvas knows about a module and does not let you open one.** A module node draws as a subgraph
  and the inspector says what it is, with no drill-in editing. An op is drawn and its command and
  `reads`/`writes` are shown, but they are read-only: op definitions are edited in the file.
- **A graph's size is whatever its author drew.** Work whose *number of pieces* is unknown until the
  data arrives has to be absorbed by a node looping over it, one pass per item with a fresh context
  and a commit each — which is most of what fanning out would give, minus doing it at the same time.
  Running many pieces at once would need node instances created at run time, and node identity is
  static today (`id` → directory → counters → trace name). Worth knowing before designing toward it.
