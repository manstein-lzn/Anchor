# What is not decided

> **历史归档（2026-09-25）**：保留当时的讨论、计划与验证证据，不代表当前实现或待办。
> 文中的“下一步”“待实施”“必须”及旧阅读顺序仅适用于当时阶段，不作为后续开发指令。
> 当前入口见 [项目 README](../../README.md)，唯一升级方向见 [Plugin 设计](../plugins.md)。

A handover, written for a session that does not have the conversation this came from.

`README.md` says what the runtime is. `DECISIONS.md` says why it is that way — **ADR-056 onward** is
this branch, everything above it describes a design that was deleted. This file says what is still
open, with the arguments on each side and with what has already been refused, so that none of it is
re-derived from scratch.

**A decision here is not a decision until it is in `DECISIONS.md` and in the tests.** Anything below
that reads like a conclusion is a proposal.

---

## In flight: the agent node runtime is being replaced

ADR-062 is **the executor**. Since M3 the graph's AI nodes go through
`run_agent_node`/`run_op_node`, behind `NodeRequest`/`NodeOutcome` in `src/anchor/node/__init__.py`, and
mini-swe-agent is no longer on any path the scheduler takes. `ARCHITECTURE_AGENT_NODE_MIGRATION.md` is
the plan (M0–M5) and `AGENT_NODE_VALIDATION_RESULT.md` is what has been measured; the fault matrix runs
against the new seam and passes, B1–B8 and C1–C9 included.

**The migration is finished.** M5 landed and `simple/agent.py` is gone, with the second completion
parser (`_check_finished`) that lived in it, `tests/test_agent_resume.py`, and `mini-swe-agent` out of
`pyproject.toml`; `pydantic-ai-slim` and `pydantic-ai-harness` moved from
`requirements/node-verification.txt` into it, at the versions ADR-062 pins. The acceptance run is
`AGENT_NODE_MIGRATION_ACCEPTANCE.md` — the §12 matrix, 235 tests, and the 26-window fault matrix.

What is left as a stated boundary rather than as work:

- Historical runs are read-only and are not converted; an old trace is not dressed up as a
  `RecoveryRef` (M4). Old `runs/` records keep being shown by the view and are not re-run.
- `scripts/recovery_windows.py`'s A6 still reports `blocked`. It is a limit of where the harness lets a
  hook sit, not a missing seam — §9's read is `_settled_already` and B8 exercises it in a real graph.
  `AGENT_NODE_MIGRATION_ACCEPTANCE.md` §4 keeps the argument.

---

## Where the runtime is

Verified, tested, and in the working tree. Nothing below needs re-litigating.

**A graph** is one JSON file: `agents`, `ops`, `graphs` (modules), `nodes`, `edges`, `objective`,
`entry`, `max_rounds`.

**A node** is one of three things *as written* and one of two *at run time*:

| written | at run time |
|---|---|
| `{"id": …, "agent": …}` | an **agent** — a model loop |
| `{"id": …, "op": …}` | an **op** — one command, and its exit code is the verdict |
| `{"id": …, "graph": …}` | a **module**, which expands away entirely |

A scope is a node one level up, so a module's nodes are named `write/draft` and that id is a directory
name. Rounds are counted per level (ADR-058).

**What an edge carries is a pointer to a commit**, never a copy. A node is mounted its selected
in-edges **and**, following forward edges, the work those were built from — and the following stops at
a **back edge**, because a back edge carries the loop's current state and that state has superseded the
round it came from (ADR-056, ADR-057).

**A node's workspace is its own and is kept across its passes.** Each pass is frozen as a commit in
that node's own repository, made by Anchor outside the sandbox. So a node's output is exactly what it
wrote, and it is attributable to it by construction.

**Both kinds declare `reads` and `writes`**, and at load every file a node says it reads must be one
that something it can be handed writes. That check exists because a node wired to nothing reads
nothing, does the work anyway, and submits — which is invisible in a run's status (ADR-061).

**Completion is an action.** An agent finishes by running `anchor-done` or `anchor-route`; an op
finishes by exiting 0, and routes by printing `ANCHOR_ROUTE:` or calling `anchor-route`.

**The whole thing is exercised end to end with no provider.** `ANCHOR_MODEL_SCRIPT` names a JSON file
mapping a node id to the commands it should be given; only the model is replaced. `tests/` uses it for
a loop inside a module inside a loop, for a gate whose branch is taken by `grep`, and for an agent and
an op in one graph (ADR-059).

**235 tests.** `pytest tests/ -q`, `ruff check src/ tests/ scripts/`, `mypy src/anchor/`.

Where things live: `src/anchor/simple/graph.py` (schema, expansion, the interface check),
`src/anchor/simple/run.py` (the scheduler, the prompt, the record, the commit),
`src/anchor/simple/node_bridge.py` (the scheduler's node: an agent node to `run_agent_node`, an op node
to `run_op_node`), `src/anchor/node/` (the node runtime — `__init__.py` is the contract, `agent_runtime`
and `adapter` are the agent half, `op_runtime` the op half, `recovery` the node's own state machine),
`src/anchor/serve.py` (the service), `examples/graphs/` (five checked graphs plus one gated example).

There is no `simple/agent.py` any more; it was mini's loop, and it and `mini-swe-agent` were deleted at
M5. The sandbox helpers it used to hold live in `src/anchor/runtime/`.

---

## The frame that took the longest, and is worth keeping

### A ReAct loop is a graph cycle, and the observation is a file

```
ReAct:   reason → act → observe → reason → …
                          ↑
                    where does the observation come from?
```

An agent node *is* a ReAct loop (PydanticAI with `pydantic-ai-harness` since M3), so the loop already
exists — but inside a node it is invisible to the graph: not checkpointed, not attributable, not
gateable. The harness changed what is behind that; the observation the graph reads is still a file.

Expressing it as a cycle makes the observation a **file**, and a file can be written by a program or by
a person instead of by the model's own reading of a tool result. `examples/graphs/academic-gated.json`
is that shape running today:

```
write(agent) ──→ structure(op: grep) ──┐
   ↑                                    │
   └──────── observation in check.txt ───┘
```

The writer cannot argue with `grep`. It is the same loop, with an observation that cannot be talked
around.

**So the two application patterns are not two things.** Plan-and-execute is the graph's static shape.
ReAct is a cycle in it. The only variable is who writes the observation: a program, another model, or a
person.

### A run is a process with an address, not a function

| | an LLM call | a graph run |
|---|---|---|
| where the work is | in the connection | **on disk** |
| addressable | no | **yes** (`runs/<id>/`) |
| resumable | no | **yes** (`--resume`, and `anchor-serve` on startup) |
| the result | the return value | **a declared output** |

This is why **async is not a compromise**: an LLM call is a function, a graph run is a job. Borrow the
*addressing* from LLM APIs (a URL, a key, a name that selects what runs — and the name being a graph is
natural). Do not borrow the *timing*: it would teach callers that this is a call they can retry freely,
when it has real side effects and real cost.

And the reassuring half: **an LLM call that times out loses the work; a graph run that times out does
not.** The result is in the workspace, in the commit, in `run.json`.

### Three shapes, and only one of them needs a mechanism

| | shape | covered by |
|---|---|---|
| **A** | one-shot job: trigger → run → result stored | today, once the result is declared |
| **B** | fast request/response: the same, with a bounded wait | A plus `?wait=` |
| **C** | the work is interrupted by something from outside | **not built — see the open question** |

A is the workhorse. B is a switch on A, not a second mechanism. C is the one that is genuinely
missing, and it is what an on-call agent, an approval, or a conversation with a person needs.

### What a graph is actually for

Not capability: a single agent with a good context does most work. The graph exists for three things,
and they are the reason to pay the cost of a lossy handoff (a node sees files, never the reasoning that
produced them):

1. **State that outlives any context.** Not "large" — unbounded. A node's context is only ever "my
   inputs and my job".
2. **A decision about completion the doer cannot make.** This is the whole of `完成是一个动作`. It
   cannot be expressed inside a single agent, because whatever decides is also the agent and can be
   talked around. It needs a boundary.
3. **Capabilities that change per step.** `gatherer` has network, `writer` does not — and that is the
   sandbox, not a sentence in a prompt. A single agent's capability set is the union and is monotonic.

---

## Open question 1: is `agent` a sibling of `op`, or a kind of `op`?

**As implemented** (siblings):

```json
"agents": {"writer": {"model": …, "instructions": …, "reads": […], "writes": ["paper.md"]}},
"ops":    {"structure": {"run": "grep -q '^## References' /in/write/paper.md", "reads": […]}},
"nodes":  [{"id": "write", "agent": "writer"}, {"id": "check", "op": "structure"}]
```

**As proposed** (one table, one relation):

```json
"ops": {"writer":    {"kind": "agent", "model": …, "instructions": …},
        "structure": {"kind": "shell", "run": …}},
"nodes": [{"id": "write", "op": "writer"}, {"id": "check", "op": "structure"}]
```

For the second: there is only ever one "node → definition" relation rather than two, and **adding a
third form later does not add a table** — and a third form is known to be coming (see open question 3).
Against it: "a node is an agent or an op" reads naturally, and it is what the schema does now.

**The cost of changing is small, and the reason is worth knowing**: the runtime already treats them as
one thing. `Graph.definition(node_id)` returns either, `Graph.reads`/`Graph.writes` go through it
without asking, and `_handed` and `_record` never ask which. Two places do distinguish them — `parse`,
and one branch in `_task` that says whether the completion rule is an action or an exit code.

## Open question 2: a probe an op does not need

`SandboxEnvironment.__init__` runs one trivial command with the node's real mounts before the loop
starts, and refuses if it fails. It was added because a sandbox that could not run a command made every
command fail while the model retried for two thousand turns before anything said so (ADR-059).

**For an op that probe is redundant**: the op's command *is* a probe, and a sandbox that cannot start
reports itself as a non-zero exit with bwrap's stderr, which is a clearer failure than the probe's.
Removing it saves one bubblewrap invocation per op node.

Against: it is a conditional path, and conditional paths are where the bugs in this file have come
from. The saving is roughly 50ms against agent nodes measured in minutes.

---

## Open question 3 (the substantial one): the outside world

**Nothing waits for the outside world.** Every file a node gets comes from inside the graph, so a run
is either working or over. A graph can be started from a call, but it cannot be interrupted by one, and
it cannot be answered.

### The mechanism, as proposed

> **A node whose input is filled from outside instead of by an edge.**

One declaration, used four times:

| use | what it is |
|---|---|
| **trigger** | the first such node — the run starts when its files arrive |
| **ask** | a later such node — the run is not finished until its files arrive |
| **reply** | the same node, filled — the run continues |
| **a live conversation** | the same node filled quickly, once per turn |

This is not four mechanisms. It is one declaration, and it lands exactly where every other input lands:
**files in a workspace, committed** — so it is recorded, pinned, and reachable by everything
downstream. It declares `writes`, so a trigger that omits one is refused **at the boundary** rather
than discovered by a node mid-run, and the load-time interface check counts it as a producer.

**`waiting` should not be a state to build.** It is a derived conclusion: a run that has reached a node
declared to be filled from outside, and that has not been filled, is not finished and is addressable.
The thing to record is the declaration; the status is computed.

### What it needs that does not exist

- **An address for a run that is not finished.** `runs/<id>/run.json` already is one; the scheduler
  would have to be able to stop with one of these unfilled rather than reporting `finished`.
- **A way to deliver files into a named node's workspace of a run that already exists**, and to commit
  them so they are pinned like everything else.
- **Concurrency.** `anchor-serve` refuses a second trigger for a graph with `409` while one is running.
  For a service that must go: every run already has its own directory, so the filesystem is ready and
  the `409` is a deliberate simplification that has outlived its purpose.
- **A declared result.** Today a run's "result" is "everything in every workspace", which is not an
  answer. `"result": {"from": "respond", "reads": ["answer.md"]}` is the symmetric twin of the input
  declaration, and it is checkable the same way.

### Design space, not yet chosen

- **Naming.** Is it an `op` with no `run` (so the two kinds stay two), a third key beside `agent` and
  `op` (a third kind, which is what the last two rounds of discussion have been avoiding), or
  `"external": true` on a node? The leaning is **an op with no `run`**, because its "execution" is the
  outside putting files there.
- **Addressing.** A signal to the run with a node name (`POST /runs/<id>/signals {"node": "ask", …}`)
  or to the node's path directly? The leaning: **to the run, naming the node** — because the node's
  expanded path (`write/draft`) is the graph's internal structure and the caller should not have to
  know it, while the name the author chose is part of the graph's interface.
- **The API shape.** Async-first (`201` with a handle), `GET /runs/<id>/events` as SSE of the node-level
  events that are already printed as JSON lines, `GET /runs/<id>/result`, and an optional bounded
  `?wait=<seconds>` for graphs that happen to be fast. **Never a blocking default**: a ten-minute HTTP
  request is a trap for clients, proxies and gateways.
- **What the graph is for, at the boundary.** Both directions are ops, so both are nodes: an **intake
  op** turns the payload into the files the graph wants, and an **emission op** sends the result out.
  That means neither is an API-layer hook, and both are checked, recorded, attributable and routable
  like anything else.

### Sharp edges, named now rather than discovered later

- **SSRF.** If the payload carries a `reply_to` and the emission op posts to it, **the caller chooses
  where Anchor makes an outbound request**. Either destinations are declared by the graph and the
  payload may only choose among them, or there is an allow-list. This must be decided with the
  mechanism, not after it.
- **Secrets, and why an op may have them.** A model gets its key out of band (in the client, never in
  the sandbox). An op that posts to a platform **must** have a token, which means a secret inside the
  sandbox. The rule that follows from the two kinds rather than being bolted on: **`secrets` may be
  declared on an op and refused on an agent**, because an op cannot improvise and an agent can. Giving
  an agent a credential means giving it to whatever the agent decides to do.
- **Sending once.** An op that exits non-zero fails the pass, and a retry sends again. For a
  notification that is nothing; for a charge it is not. There is no idempotency key anywhere in this
  branch. **This is the hardest part of the whole area**, harder than the interface.
- **Cost and rate.** A run can cost dollars and take an hour; a key is not only a permission but a
  budget. Nothing tracks or limits spend today.
- **Latency is the author's, not the mechanism's.** How fast a turn is, is how long the nodes between
  the two boundaries take. A graph is not a chat model: a turn costs what its nodes cost. What the
  runtime owes is the **stream** — the node-level events — so a person can watch.

### Also in scope, and not yet built

- **Idempotency of the trigger itself**: a retried `POST` starts a second run.
- **The adapter layer.** `apps/web` draws a module without letting you open it, and shows an op's
  command and its `reads`/`writes` without letting you edit them. `serve.py`'s `trigger` takes only
  `{"graph", "objective"}`.

---

## Closed: things that look open and are not

**Dynamic fan-out.** Work whose *number of pieces* is unknown until the data arrives. Today one node
absorbs it by looping — one pass per item, **a fresh conversation each pass**, a commit and a trace
each — which is most of what fanning out would give, minus doing it at the same time. Measured: this
covers bounded context, per-item checkpoints and per-item attribution, and a shell op can verify the
count. **What is genuinely not covered is parallelism at an unknown N**, which is a performance
property rather than an expressiveness one. The obstacle to it is that node identity is static (`id` →
directory → counters → trace name); created-at-run-time instances would touch the foundation. Deferred
on purpose, and nobody should re-derive it as a capability gap.

**`waiting` as a state.** It is derived (see open question 3), not built.

**Stateless multi-turn calls, like an LLM API.** This solves *cheap* conversation — the caller carries
the context — and it is wrong for **expensive** work interrupted by a person, because the state would
have to be carried by the caller. A graph's state belongs on disk behind an address.

**A shared workspace with one owner at a time.** Refused, ADR-060: it is a lock rather than a pointer,
it makes what a node can see depend on runtime ownership rather than on the graph, a crash corrupts
shared state, and two nodes writing one file is a silent overwrite that cannot be detected.

**Mounting every ancestor, or a `reads` field naming extra nodes.** Refused, ADR-057. Following
back-edges re-mounts every earlier round — measured at 3 commits in round one and 30 by round ten — and
a field an author can forget reintroduces the silent failure the interface check exists to remove.

**A module node's `max_rounds`.** Was refused, and is now meaningful: at the level above, a module *is*
a node, and there it means how many times the parent may enter it (ADR-058).

---

## Appendix: the measurements and failures that constrain this

Each of these cost real time, and each of them is the reason for something in the design.

- **A pointer has to name a commit, not a directory.** A directory is live: its owner writes to it
  again, and once nodes may run at the same time it can change while a neighbour is reading it.
- **Following a back edge to build lineage is unbounded.** Measured on a three-node loop: 3 commits
  handed over in round one, 9 by round three, 30 by round ten. Stopping at the back edge keeps it
  constant, and `back_edges()` already computed that set for loop detection.
- **Two passes of one node at two commits are two bindings at one path**, and bubblewrap leaves
  whichever it applied last. `work/check` read `draft@1` while being handed `draft@2`, asked for
  another round, and the loop never converged — visible only because it hit the ceiling. One mount per
  node, newest pass wins.
- **`git archive` of an empty tree is not an empty tar.** Python's `tarfile` refuses it outright, so a
  node that only routed broke the next node's startup.
- **`tarfile`'s strictest filter refuses an absolute symlink.** One node leaving `ln -s /usr/bin/python3
  .` behind made the next node fail to start, reported as a tar error about a good commit.
- **`os.readlink` answers with a relative path.** `Path("python3").parent` is `.`, so the sandbox got
  `--ro-bind . .` — bubblewrap's working directory over the sandbox root — and every command failed
  with `Can't mkdir /tmp`. A node retried for **two thousand turns** before anything said so, which is
  why the trace matters and why the construction probe exists.
- **`_check_finished` read `if not self.routes`**, so only a node with *no* way out could finish the
  ordinary way. Every non-terminal node was told by its prompt to run `anchor-done` and refused for
  doing it. It survived because a model reads the correction and routes instead.
- **A node the ceiling turns away stays turned away.** In a cycle of two or more it remained "fresh"
  through the edge coming back into it, so the run spun printing `stopped` forever.
- **An outer loop spent the inner loop's budget**, and the run stopped with `ship` skipped. Counting is
  per level, and both restart at each entry.
- **The examples were silently wrong.** Their instructions said "the file is already in your
  directory", which stopped being true when edges became pointers. `revise-loop`'s drafter never saw
  the review, so the loop spent its whole ceiling doing nothing while reporting `finished`.
- **Three example graphs could not load at all**, against a schema that had been deleted, and nothing
  noticed because no test had ever looked at an example.
