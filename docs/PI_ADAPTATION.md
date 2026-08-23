# Pi Adaptation Boundary

Anchor is a focused fork/runtime, not a rewrite of Pi.

## What stays from Pi

Keep Pi's existing implementation for:

- provider and model adapters;
- Responses/streaming transport;
- Agent tool loop;
- built-in tools and extension hooks;
- interruption, retry, and abort behavior;
- session tree persistence;
- interactive, print, and RPC modes;
- display and usage accounting.

The implementation currently depends on Pi coding agent `0.84.2` through npm.
The global npm installation is not edited. A later release can pin a fork or
workspace checkout when Anchor needs Pi source changes rather than its public
`transformContext` seam.

Anchor's interactive CLI constructs Pi's `AgentSessionRuntime` and mounts the
official `InteractiveMode` on an Anchor runtime host. This keeps the Pi command
surface and terminal behavior unchanged while rebinding the State Context
transform whenever Pi switches, forks, or resumes a session. With the default
`.anchor/state.json` path, the State store follows the active session's project
cwd. An explicit `--state` path remains fixed across session switches.

## The narrow replacement

The model-facing seam is Pi's `Agent.transformContext` callback. Anchor adds a
State Context provider at that boundary:

```text
read State snapshot
      |
compile ContextEnvelope
      |
convert envelope to model messages
      |
run the unchanged Pi Agent loop
```

The session tree still receives messages for UI, audit, and recovery. It is not
used as the normal source for the next long-task Context.

## State update hook

Context replacement alone is not enough. The runtime must capture the result of
each meaningful model/tool cycle:

```text
assistant result + tool observations
          |
     State reducer
          |
   State revision + events
          |
     next projection
```

The first implementation should update mechanically observable facts first:
tool calls, command results, changed files, errors, artifacts, hashes, and
explicit lifecycle transitions. Semantic conclusions require an explicit
validated state-delta format; they must not be inferred from arbitrary prose
and silently committed.

## Invocation boundaries

Do not rebuild Context for every internal tool message if Pi is still inside one
model/tool invocation. Keep a bounded local turn context for that loop. After a
completed invocation or a meaningful State transition, commit observations and
compile the next Context.

## Compaction policy

Anchor State Context mode disables Pi's normal threshold-based transcript
compaction in memory by default. This does not modify the user's Pi settings
file, and manual `session.compact()` plus provider-overflow recovery remain
available. During migration:

1. Anchor disables Pi's automatic threshold compact in its in-memory settings;
2. detect a provider length/context overflow at `agent_end`;
3. queue Pi's existing `session.compact()` before the next Prompt only for that
   overflow case;
4. preserve the State and Evidence revision independently of the transcript;
5. remove or demote fallback only after restart and long-task acceptance tests.

## Migration modes

### Shadow

Pi behaves normally. Anchor compiles a Context in parallel, records compile
latency and size, and compares recovered facts with the existing session view.

### State Context

The model receives the State projection. The full transcript remains persisted
and visible to the UI, but is no longer the default long-task prompt source.

### Restart recovery

Terminate the process, create a fresh Pi runtime, load State and Evidence, and
continue without replaying the full transcript.

### Fallback retirement

Only after repeated long-task tests pass should normal transcript compact be
retired. A provider overflow fallback may remain indefinitely as a defensive
guard.

## Acceptance checks

The fork is not ready when it merely starts Pi with a different prompt. It must
demonstrate:

- stable Context size across hundreds of synthetic turns;
- no normal transcript compact in a long run;
- correct State revision after each accepted result;
- rejection of stale or malformed results;
- recovery after process termination;
- preservation and retrieval of Evidence and Artifact hashes;
- tool failure and retry recovery;
- no capability escalation through prompt text;
- measured `T_compile` p50/p95 without full-history scans;
- short-task behavior that remains compatible with Pi.
