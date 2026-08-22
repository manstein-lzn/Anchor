# Anchor Architecture

Status: normative baseline, version 0.1

## 1. Problem

Traditional coding Agents use a growing conversation as memory:

```text
system prompt + all previous messages + tool results
                         |
                        model
                         |
                 append another turn
```

The same transcript is asked to carry current truth, historical detail,
decisions, failures, permissions, and temporary reasoning. As work continues,
it grows toward the model window. A conventional compact then removes a large
and poorly understood portion of the history, often keeping irrelevant detail
and losing a fact that matters later. The Agent continues with a degraded model
of the task and repeats the cycle.

Anchor changes the memory topology without replacing Pi's proven execution
loop.

## 2. Core thesis: State as Context

The core unit of long-running cognition is not an Agent identity or a complete
conversation. It is a versioned task State and a purpose-specific Context
compiled from that State.

```text
State(t) + Evidence(t) + Purpose(t)
               |
        ContextEngine
               |
          Context(t)
               |
             model
               |
   Result(t) + observations(t)
               |
       validate and reduce
               |
            State(t+1)
```

The model still performs the same basic operation, `input -> output`. The
runtime stops treating the input as the entire transcript and stops treating
the output as an automatic state mutation.

## 3. Runtime layers

### 3.1 Durable State

State is the current authoritative representation of the task. A practical
State contains:

- task goal and acceptance criteria;
- constraints and explicit exclusions;
- current lifecycle phase and revision;
- completed actions and verified conclusions;
- failed, blocked, and cancelled work with causes;
- decisions and unresolved questions;
- current plan and next legal action;
- Artifact paths, hashes, and structured indexes;
- Evidence references and provenance;
- capability scope and relevant policy revision;
- active invocation/snapshot identity when work is in flight.

State should be materialized and incrementally updated. It must not require a
full EventLog replay for every model call.

### 3.2 EventLog

EventLog is append-only history: observations, transitions, tool events,
rejections, retries, and lifecycle changes. It supports audit, diagnosis, and
reconstruction. It does not answer the question “what is true now” without a
state reducer or materialized State.

### 3.3 Evidence and Artifacts

Evidence is a durable reference to a fact, output, command result, test result,
or external observation. Artifacts hold large content outside the Context.
Evidence should carry a stable locator, hash where applicable, type, source,
and creation or observation revision.

Anchor never needs to choose between “keep every byte in Context” and “delete
the evidence.” It stores the evidence externally and projects the relevant
content or reference when the current purpose requires it.

### 3.4 ContextEngine

ContextEngine is a deterministic state-to-context compiler. Its input is:

```text
authoritative State
relevant Evidence index
invocation purpose
capability view
current snapshot/revision
optional bounded local observation
```

Its processing is:

1. read a consistent State snapshot;
2. select the projection policy for the purpose;
3. include current facts, constraints, unresolved work, and next action;
4. attach relevant evidence references or bounded content slices;
5. include provenance, revision, and capability metadata;
6. render a model-facing Context envelope;
7. record compilation metrics and the source revision.

Its output contains:

```text
ContextEnvelope {
  purpose,
  state_revision,
  goal,
  acceptance,
  current_facts,
  completed_work,
  failures_and_blockers,
  decisions,
  open_questions,
  evidence_refs,
  relevant_artifacts,
  allowed_actions,
  next_action,
  provenance
}
```

ContextEngine does not write truth, grant capability, or silently summarize
away an unresolved conflict. It can report that evidence is missing or the
State is inconsistent.

### 3.5 Invocation and Pi execution

An Invocation binds one Context, State revision, purpose, model configuration,
and capability view. Pi performs the model and tool loop. Tool calls may have a
small local turn context while the invocation is active. At the invocation
boundary, relevant observations are reduced into State and the next Context is
compiled afresh.

The runtime therefore preserves Pi's normal abilities:

- streaming;
- tool calls and tool results;
- interruption and retry;
- provider/model selection;
- session tree and UI behavior.

Only the model-facing context source changes.

### 3.6 Result validation and State reduction

The model output is a candidate Result. The reducer accepts only explicit,
validated changes:

```text
Result + tool observations
          |
 schema/capability/version/transition checks
          |
   State delta + EventLog + effects
          |
 atomic commit at expected revision
```

An invalid or stale Result becomes a rejection event. It does not overwrite a
newer State. External effects use stable identifiers and an outbox or equivalent
durable handoff when the host requires it.

## 4. State normalization (the new compact)

Anchor may normalize State, but this operation is different from transcript
compact.

Normalization can:

- fold a completed action into a current fact;
- replace an obsolete plan with its successor while retaining provenance;
- close a resolved question;
- retain a failure cause and its recovery decision;
- move verbose observations to EventLog or Artifact storage;
- update a bounded Evidence index;
- materialize a new State revision.

Normalization must not:

- erase the only copy of a fact;
- remove the source or hash of Evidence;
- resolve a conflict by guessing;
- let a model-generated summary become authoritative without checks;
- mutate State without a revision and audit record.

Normal operation should update State incrementally. A larger normalization pass
is triggered by a meaningful lifecycle event, a known growth threshold, a
handoff/recovery boundary, or an explicit maintenance action. It is not run on
every individual tool call.

## 5. Context purposes

One universal prompt is not required. A projection is selected by purpose:

- `work`: current goal, constraints, next action, relevant evidence;
- `resume`: completed work, failures, pending work, last accepted revision;
- `review`: claim, acceptance criteria, evidence, unresolved risks;
- `verify`: expected result, validation commands, artifact bindings;
- `acceptance`: final state, provenance, required authorities and receipts.

All purposes use the same State authority. They differ only in what is
projected and how much supporting evidence is attached.

## 6. Why normal transcript compact is no longer the main mechanism

If the next invocation is compiled from State, the Agent does not need the
entire prior transcript. A session can run for a very long time without
periodically summarizing and replacing its own conversation.

This does not remove the model's finite context window. A large single Artifact
or a genuinely broad question may still require staged retrieval or a
purposeful summary. The guarantee is narrower and useful:

> Anchor avoids unbounded transcript growth in the normal long-task path.

Anchor disables Pi's automatic threshold compact in the active settings
projection. Pi's manual compact and provider-overflow recovery remain an
emergency fallback. They are not the normal State Context path and must not be
used as a substitute for maintaining State and Evidence.

## 7. Latency model

The turn budget is:

```text
T_turn = T_compile + T_model + T_tools + T_commit
```

`T_compile` is local deterministic work. It must not contain an extra LLM call.
The hot path reads a materialized State and bounded indexes, keyed by State
revision, purpose, and projection policy. It must not scan the complete
EventLog, read every Artifact, or recompute every hash.

The runtime should record p50 and p95 for compilation, State reads, Context
size, model calls, tool calls, and commit. A slow compiler is a defect to
measure and fix, not a reason to return to full-history prompting.

## 8. Roles, Bubbles, and Agent identity

Anchor does not require a permanent Master, Role, or Bubble ontology.

- A role is an invocation policy and acceptance perspective.
- An Agent is a disposable model invocation.
- A Bubble is optional isolation for tools, permissions, cancellation, or
  parallel execution.
- None of them owns project truth or long-term memory.

If a Bubble is used, it may propose a Result or State delta. It cannot directly
write global State. The host validates capability, revision, transition, and
provenance before commit.

## 9. MetaLoop relationship

MetaLoop and Anchor overlap in the model-facing recovery layer, but not in
durable truth.

MetaLoop is well suited to remain the durable outer kernel for:

- project and task lifecycle;
- Attempts and checkpoints;
- Evidence and artifact hashes;
- Git/workspace stamps;
- acceptance and independent review;
- recovery and legal next transitions.

Anchor should consume that State through a thin adapter and compile Context for
Pi. It should not copy MetaLoop's SQLite truth, create a second Task ontology,
or reimplement acceptance. Conversely, MetaLoop should not force Agent-facing
history protocol when a Context projection can derive the same recovery view.

The target relationship is:

```text
MetaLoop durable kernel
          |
      State adapter
          |
Anchor ContextEngine
          |
       Pi runtime
```

## 10. Security and authority

Prompt text cannot grant permissions. Capabilities come from validated State and
host policy. Every host tool checks the effective capability view and trusted
workspace/path boundaries at execution time.

The Context may describe allowed actions, but it is not the authorization
decision. The host checks again. Model output cannot create a capability,
change its own revision anchor, or bypass a transition.

## 11. Current and target behavior

Current implementation baseline:

- Pi uses an append-only session tree and compaction-aware message projection.
- AgentHub has a deterministic ContextEngine and State-driven workflow pieces.
- MetaLoop persists task, Attempt, checkpoint, Evidence, workspace, and
  acceptance facts.
- Anchor now includes a minimal JSON StateStore, JSONL audit log, deterministic
  ContextEngine, validated state_delta reducer, a cognition (belief) layer with
  deterministic revision ops (add/confirm/refute/supersede/amend, all with
  provenance), bounded invocations (working-set budget with deterministic
  elided-work digest and continuation from state), and a Pi `transformContext`
  adapter. Every invocation ends with a mandatory cognition declaration
  (fenced `anchor-state-delta` block: state_delta + belief_ops) that is
  parsed, validated, and committed with revision checks; a missing
  declaration after real work triggers exactly one nudge. Tool observations
  are audit-only EventLog records and are never auto-reduced into State.
  It is a local first implementation, not yet a production durable
  database or full MetaLoop adapter.

Target Anchor behavior:

- Pi's model/tool loop remains intact;
- State and Evidence become the normal model-facing context source;
- session transcript remains for audit/UI, not long-term prompt memory;
- State updates are incremental, versioned, and validated;
- normal long tasks do not need transcript compact;
- Pi compact remains only as a migration/emergency fallback until verification.

## 12. Non-goals

Anchor is not:

- a new multi-agent scheduler;
- a Bubble orchestration framework;
- a vector-memory product;
- a transcript database that replaces durable State;
- a second project-management or acceptance system;
- a model-based summarizer on every turn;
- a token-minimization project.
