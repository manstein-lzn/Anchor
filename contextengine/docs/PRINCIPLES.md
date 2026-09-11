# Principles

## Plan before authority

A first prompt is evidence of intent, not a complete task state. New sessions
remain read-only until discussion produces a complete structured proposal and
the user confirms it.

## One session, one Anchor

Each Pi session identity has at most one Task. Resume reads it directly; tree
navigation does not roll it back, and a fork receives a new identity.

## State is authority

The confirmed Task and immutable cognition Checkpoints live in one SQLite
authority. Pi stores conversation plus small mode and proposal audit entries.

## Context is a projection

Bound execution receives `Pi prefix + RecoveryView + Pi active window`. Anchor
does not resummarize or trim the active window on ordinary model calls.

## Update is a lifecycle node

Compact invokes a dedicated tool-less cognition task. Its output becomes state
only after validation and versioned commit.

## Mechanical truth stays mechanical

Git, hashes, and project checks establish workspace changes and completion.
Agent prose does not.

## Failure is non-destructive

Rejected initialization writes nothing. Rejected Update preserves the prior
state and active Pi window.

## Keep the Extension thin

Use Pi's public hooks. Do not add a wrapper runtime, provider layer, scheduler,
parallel state, or speculative infrastructure.
