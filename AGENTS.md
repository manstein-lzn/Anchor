# Anchor Development Contract

This document is normative for runtime changes.

## Core model

- Durable State, Evidence, and Artifacts outlive model invocations.
- Agent identities are disposable invocation policies, not owners of memory.
- Context is a bounded, purpose-specific projection compiled from authoritative
  State. Context is never the authority.
- EventLog records what happened; it does not answer what is currently true.
- Model output is a candidate Result. It becomes durable truth only after
  schema, capability, version, transition, and provenance checks.
- Authorization comes from validated capability state, never from prompts,
  role names, or model output.
- A Bubble, when used, is an isolated disposable execution transaction. It may
  propose state changes but cannot write global truth directly.

## Context rules

- Do not send the complete transcript as the normal long-task memory.
- Do not call an LLM merely to compile Context on the hot path.
- Do not use lossy token trimming as the primary quality strategy.
- Store large evidence as immutable Artifacts and project relevant slices or
  references into Context.
- Keep raw transcript for audit, UI, and debugging; it is not the task oracle.
- State normalization may merge superseded facts, but must preserve provenance,
  revisions, conflicts, and links to the original evidence.
- Keep Pi's transcript compact as an emergency provider-overflow fallback until
  State Context has passed long-task verification. It is not the normal path.

## Runtime boundaries

- Pi remains responsible for model transport, streaming, tool execution,
  interruption, retry, and user-facing session behavior.
- Anchor is responsible for State, Evidence, Context projection, Result
  validation, and deterministic State reduction.
- Default Anchor runtime data is session-scoped and stored under Pi's agent data
  directory, keyed by Pi session identity. Normal `anchor` use must not create
  or modify files in the project workspace. Project-local State is allowed only
  when the user explicitly supplies a State path.
- MetaLoop may provide durable project/task/attempt/evidence/acceptance storage;
  Anchor must not duplicate its truth or create a second writer.
- Do not add Master, Bubble, Role, scheduler, agent pool, vector memory, or
  transcript protocol unless a concrete requirement proves it necessary.

## Performance

- The hot path is: read materialized State, read bounded indexes, compile the
  requested projection, and invoke Pi.
- Never rescan the full EventLog or all Artifacts for every model turn.
- State normalization is incremental or event-triggered, not a full scan per
  tool call.
- Measure context compilation latency and input size with p50/p95 metrics.

## Documentation and verification

- Keep current behavior and target behavior distinct in documentation.
- When changing a normative rule, update `docs/ARCHITECTURE.md` and this file
  before implementation.
- Every non-trivial reducer, projector, or persistence change needs a focused
  runnable test.
- Long-task acceptance must include process restart, stale Result rejection,
  tool failure recovery, and evidence provenance checks.
