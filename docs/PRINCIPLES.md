# Principles

## 1. State is the long-term memory

The task's authoritative State contains the facts needed to continue work:
goal, acceptance criteria, constraints, current phase, completed work, failed
work, unresolved questions, decisions, artifacts, evidence references, and the
next legal action. It is versioned and persisted.

The State must remain useful after the original Agent process and conversation
are gone.

## 2. Context is a projection, not a database

`ContextEngine` compiles a Context for a particular purpose and invocation.
Typical purposes are `work`, `resume`, `review`, `verify`, and `acceptance`.
The projection may choose what to show, but it may not invent facts, grant
permissions, or become a second source of truth.

The model should see enough information for the current decision, not every
event that ever happened.

## 3. Transcript is transport and evidence

Pi's transcript is valuable for streaming, UI, debugging, and audit. It is not
the durable task memory. A full transcript can be retained without being sent
to the next model invocation.

Tool outputs become Evidence or Artifacts through explicit handling. Large
content stays external and is retrieved by reference when needed.

## 4. State整理 is not transcript compact

Traditional compact deletes or summarizes an overgrown conversation after the
model context is already near its limit. Anchor instead keeps the transcript
out of the normal long-task Context and incrementally normalizes State:

- completed actions are merged;
- superseded facts are replaced but traceable;
- unresolved work and constraints remain;
- evidence references and hashes remain;
- the original EventLog and Artifacts remain available.

This is semantic materialization, not blind deletion. It can be deterministic
code. An optional model-assisted summary is never allowed to overwrite truth
without validation and provenance.

## 5. Agent invocations are disposable

An Agent is a short-lived invocation of a model under a Context and capability
view. Roles are invocation policies. A Bubble, if used, is an isolation and
execution boundary, not a memory owner or global writer.

The system's durable identity is the task State and its revision chain.

## 6. Results must cross a trust boundary

A model response is a proposal. Before it updates State, the runtime validates:

1. output schema;
2. invocation and snapshot identity;
3. capability and authorization scope;
4. legal transition and expected revision;
5. evidence and provenance requirements.

Stale or malformed results are rejected without overwriting newer State.

## 7. Quality before token minimization

Context is bounded because it is a projection, not because information should
be aggressively cut. The priority is task completion and information
saturation. Token estimates are a safety signal for a provider window, not the
optimization objective.

## 8. Local speed is a design constraint

Context compilation is local deterministic work, not another Agent call. The
hot path must read materialized State and bounded indexes. Full-log replay,
full-artifact scanning, and model summarization do not belong in every turn.

The total turn remains:

```text
compile Context + model call + tool execution + State commit
```

Compilation must be measured and kept small compared with model and tool
latency. Normalization can run incrementally or on meaningful lifecycle events.
