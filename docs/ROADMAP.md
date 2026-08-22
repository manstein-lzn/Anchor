# Roadmap

This is an implementation order, not a promise that every future feature is
required.

## Phase 0: contract baseline

- keep the current docs and development contract authoritative;
- choose the Pi fork/workspace source;
- define the minimal State, Evidence, ContextEnvelope, Result, and revision
  shapes;
- add a fixture for a completed task and an interrupted task.

## Phase 1: pure projector

- implement a deterministic ContextEngine against a read-only State source;
- support `work` and `resume` purposes first;
- expose compile latency, source revision, and rendered size;
- do not change Pi's execution behavior yet.

## Phase 2: shadow runtime

- compile State Context beside Pi's normal session context;
- compare goal, completed work, failures, evidence, and next action;
- measure overhead and identify missing State fields;
- keep all changes reversible.

## Phase 3: State Context runtime

- inject the compiled Context at Pi's model-facing boundary;
- retain the transcript for UI and audit;
- add deterministic reducers for tool observations and explicit state deltas;
- persist State revisions and provenance;
- retain Pi compact only as a diagnosed fallback.

## Phase 4: recovery and correctness

- restart from State without replaying the full transcript;
- reject stale Result revisions;
- test failed tools, cancelled work, retry, and duplicate delivery;
- bind Artifact and Evidence hashes;
- test capability and path boundaries.

## Phase 5: long-task evaluation

- run the same tasks through traditional Pi and Anchor State Context modes;
- compare completion quality, recovery quality, context size, compile latency,
  model calls, and tool calls;
- include unfinished, failed, and completed tasks;
- test hundreds or thousands of turns without normal transcript compact.

Only after these checks should the project decide whether to remove any legacy
session-context or compaction path. Deleting Pi behavior before the State path
is proven creates an avoidable recovery risk.
