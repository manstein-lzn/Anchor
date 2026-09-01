# Anchor Development Contract

This document is normative for runtime changes.

## Core model

- Anchor exists to preserve correct task cognition across compaction, restart,
  and long-running work. It is not a general Agent runtime or governance layer.
- Anchor State is the minimal sufficient cognition for correct continuation,
  not a transcript summary or an inventory of everything once known. Its size
  should follow current task complexity rather than elapsed task history.
- A Task contains the user-confirmed goal, acceptance criteria, constraints, and
  non-goals for one Pi session.
- A first compaction may lazily create a provisional Task without Planning;
  provisional Contract fields are inferred only from the Pi Episode and remain
  revisable until explicitly confirmed.
- A Checkpoint is the authoritative current cognition plus the source frontier
  it has absorbed. A Checkpoint without a valid frontier cannot replace history.
- An Episode is the uncovered work selected by Pi's compaction preparation.
- Context is a bounded projection of the latest Checkpoint plus Pi's current
  active window. Context is never authoritative State.
- The transcript records what happened. It remains available for UI, audit, and
  debugging, but is not the normal long-task oracle.
- Model output is a candidate Checkpoint. It becomes durable truth only after
  schema, Task identity, parent version, frontier, and provenance checks.
- Agent identities are disposable invocation policies, not owners of memory.
- Information has three attention levels: Active Cognition is projected,
  Dormant Knowledge is represented by recoverable references, and Event Archive
  is retained for audit or recovery without participating in normal cognition.
- Current cognition preserves a causal model: the Contract and current
  Situation define the remaining gap; constraints and still-relevant Experience
  justify current Intent. It is not a chronology grouped into categories.

## Update rules

- The Update Agent's semantic evidence is exactly the previous Checkpoint plus
  `preparation.messagesToSummarize` and `preparation.turnPrefixMessages`. A
  deterministic control envelope may also carry the immutable Task Contract
  and target frontier; neither is an Episode directive or new evidence.
- Never send the complete branch or transcript to the Update Agent.
- Update produces a complete current cognition snapshot, not prose summary,
  chronology, or a free-form delta. It must submit that candidate through the
  request-local `anchor_submit_update` function with JSON-schema constrained
  arguments; free-text JSON is not an Update runtime protocol. If deterministic
  validation rejects a candidate, Anchor may send one validation-only correction
  request; it must still reject the candidate if the correction is invalid.
- Bootstrap at the first compaction is a separate initialization task. It may
  create Checkpoint 0 from the exact Pi Episode, but must preserve unknowns and
  never invent user-confirmed requirements.
- Update reconstructs the minimum cognition needed for future action. For each
  previously active cognition item it must explicitly choose `carry`, `revise`,
  `resolve`, `supersede`, `demote`, or `archive`; no item may disappear silently.
- Retention is decided by future behavioral value: keep an item active only when
  forgetting it could cause a wrong decision, a constraint violation, repeated
  failed or expensive work, or loss of a currently necessary next step.
- `demote` requires an exact recovery reference. `revise`, `resolve`,
  `supersede`, and `archive` require a reason and source. Forgetting removes
  information from current attention, never from every recovery surface.
- Update emits a Transition Certificate covering the previous active set. The
  certificate is validation and audit material, not normal model Context.
- Newer explicit user corrections supersede older statements. Unresolved
  conflicts, failed paths and their reasons, and unverified hypotheses remain
  explicit until evidence resolves them.
- Tool failure is not successful evidence. Model confidence is not evidence.
- Proposal evidence sources must be bound to an allowed immutable surface:
  `episode:`, `checkpoint:`, `artifact:`, or `pi:`. Carry may preserve legacy
  source strings, but newly asserted or revised cognition must use an allowed
  source.
- The Task goal, current directive, accepted next action, and directive history
  are distinct. An elliptical continuation resolves first against the accepted
  next action, then the previous directive, never the original goal by default.
- History is retained as Experience only while it changes future strategy. A
  failed path records its cause and retry condition, not merely that an event
  occurred.
- Checkpoint commit and Pi compaction append are separate durable writes. Use a
  content-bound receipt and idempotent replay; do not claim cross-file atomicity.
- Receipt delivery is valid only when Task identity, Checkpoint version and
  hash, event identity, and the complete frontier all match the Checkpoint.

## Context rules

- Do not call an LLM merely to compile Context on the hot path.
- Do not use lossy token trimming as Anchor's primary quality strategy.
- Do not impose private token, tool-count, time, segment, or checkpoint budgets
  below the selected model's context window. Pi and the provider own window
  handling and user interruption.
- Preserve provider replay invariants. A tool call and its tool result are one
  indivisible replay unit.
- Store large evidence as immutable Artifacts and project only relevant slices
  or references.
- State normalization may remove superseded prose from the current projection,
  but must preserve revisions, conflicts, provenance, and source links.
- Projection contains the immutable Contract core, latest Active Cognition, a
  compact Knowledge Index, and Pi's active window. It excludes the complete
  Checkpoint object, transition history, resolved detail, and old summary chains.
- Recall is explicit and targeted. Recovered detail enters Pi's current work and
  may be promoted by a later Update; recall must not permanently rebloat Context.

## Runtime boundaries

- Pi owns model transport, streaming, project tool execution and permissions,
  retry, interruption, transcript, session tree, TUI, and context thresholds.
- Anchor may attach one request-local, side-effect-free submission function to
  Bootstrap or Update model transport. Anchor consumes and validates its
  arguments directly; the function is not registered as a Pi session tool, is
  never executed, and produces no tool-result transcript entry.
- Anchor owns Planning, Task and Checkpoint persistence, Update, validation,
  deterministic projection, and compact receipt recovery.
- Anchor Planning may temporarily restrict model tools to a read-only set.
  Active mode restores Pi's tools; Anchor does not claim to sandbox project
  paths, shell commands, or third-party tools.
- Default State is stored under Pi's agent data directory and keyed by Pi
  session identity. Normal Anchor use must not write runtime State into the
  project workspace.
- Normal sessions remain State-free until the first compaction boundary. A
  failed bootstrap falls back to Pi's native compaction and writes no Anchor
  State.
- Anchor State is session-level external truth. Transcript tree navigation does
  not roll it back. A new Pi session does not inherit writable Anchor State.
- Do not add writer leases, reviewer authorities, schedulers, agent pools,
  roles, vector memory, or a transcript protocol without a proven core need.

## Performance

- The hot path reads the materialized latest Checkpoint, compiles one bounded
  projection, and invokes Pi.
- Never rescan the full EventLog, transcript, or all Artifacts per model turn.
- Measure projection latency and model-facing input size with p50/p95 metrics.
- Under stationary task complexity, repeated Updates must make projected Anchor
  input approach a plateau rather than grow with Checkpoint count.

## Documentation and verification

- Keep current behavior and target behavior distinct in documentation.
- When changing a normative rule, update `docs/ARCHITECTURE.md` and this file
  before implementation.
- Every non-trivial reducer, projector, or persistence change needs a focused
  runnable test.
- Long-task acceptance must include multiple compactions, process restart,
  stale candidate rejection, tool failure, user correction, and evidence
  provenance.
- Long-task acceptance must also test transition coverage, demotion and exact
  recall, over-forgetting, repeated-failure prevention, and projected-size churn.
