# Development Handoff

Status: cognition v3, exact Checkpoint recall, and lazy first-compact Bootstrap complete; takeover and churn acceptance remain, 2026-08-31

Anchor is now a Pi package. `src/extension.js` owns the public lifecycle;
`python/anchor_core/` is the embedded durable state implementation. The former
wrapper runtime and standalone CLI have been removed.

Current lifecycle:

```text
new interactive session -> Normal Pi (no Anchor prompt)
Normal -> first Pi compact -> Bootstrap -> provisional Task + Checkpoint 0
Normal -> /anchor start -> read-only Planning
Planning -> candidate -> agent-settled seal -> Accept / Revise / Cancel
Accept -> Task + Checkpoint 0 -> Active
Active -> Pi Episode -> Checkpoint Update -> next context window
resume -> same session State; tree -> no rollback; fork -> new Normal session
```

New-session State defaults to
`<Pi agent dir>/anchor/sessions/<session-id>/anchor.db`. The State locator is
derived from Pi session identity. Mode entries are also identity-bound and read
from the complete session, so tree navigation cannot change the Task and a fork
cannot inherit writable State. Normal mode creates no Anchor State or model
context until its first compact. Normal use writes no runtime files into the
project.

SQLite now contains only `meta`, one `task`, and immutable `checkpoints`.
Contract and Checkpoint hashes fail closed on corruption. The former Project,
Attempt, Evaluation, role, assignment, workspace-governance, and event-log
surfaces have been deleted. This is a pre-release clean cut with no old-schema
migration.

The Update path now commits `anchor.checkpoint.v1` containing
`anchor.cognition.v3`, `anchor.transition.v1`, a source frontier, parent hash, and provenance. It never
reads the full branch. If SQLite commit succeeds before Pi appends its
compaction entry, the same frontier replays the stored receipt without invoking
the model again. Receipt delivery is branch-local and requires matching Task,
version, Checkpoint hash, event ID, and complete frontier. Resume rejects a
changed Pi frontier instead of creating a duplicate Checkpoint, and an Active
mode whose Task is missing remains blocked rather than falling back to Normal.

The Update protocol no longer appends a control instruction as a fake user
message. Its system prompt contains the complete `anchor.cognition.v3` and
Transition Certificate shape and requires item identity, provenance, and explicit
dispositions. The deterministic
control envelope carries the immutable Contract and target frontier; only the
previous Checkpoint and Pi Episode are semantic evidence.

Normal sessions now remain entirely unobtrusive until the first Pi compact. That
boundary invokes a dedicated Bootstrap Agent over the same Episode and atomically
creates a provisional Task plus Checkpoint 0. Bootstrap failure falls back to
Pi's native compact without writing valid Anchor State. A provisional Checkpoint 0
also participates in crash-window receipt replay.

Known boundary: this slice has no separate provisional-to-confirmed Contract
upgrade command. Explicit later user corrections are carried by the current
cognition/Update path; add a Contract revision interaction only when real use
shows that inferred bootstrap scope needs formal confirmation.

Verified baseline:

```bash
npm test
npm run check
git diff --check
npm pack --dry-run
PI_OFFLINE=1 PI_SKIP_VERSION_CHECK=1 pi --offline -e /root/Anchor --help
```

Results on 2026-08-25: 14/14 tests pass, syntax checks pass, the 17,014-byte package
contains 12 intended source files with no Python bytecode, and Pi discovers
`--anchor` plus `--anchor-state` from the local Extension.

Local interactive use:

```bash
pi -e /root/Anchor
```

A real `cwiseai/gpt-5.6-sol` audit completed Planning, four Updates, restart,
fork isolation, tool failure recovery, user correction, evidence provenance,
crash-window receipt replay, and mismatch failure injection. It exposed the
control-message and partial-receipt checks fixed above. A real threshold
auto-compact produced an Anchor receipt and continued normally; overflow compact
remains unverified. Planning currently uses Pi's default
compact summary until real use proves a dedicated Planning summary is necessary.
Pi checks model/auth before its Extension compact hook, so exact receipt replay
still needs a usable Pi model configuration. Planning restores one process-wide
tool snapshot because Pi has no per-Extension tool ownership API; concurrent
tool-list changes by another Extension may be overwritten when Planning exits.
Do not reintroduce an Anchor wrapper executable, parallel state writer, or
one-prompt automatic Task creation.

The cognition v3 slice is now implemented. Checkpoints use Situation,
Experience, Intent, and Knowledge Index; active items have stable IDs and
provenance; Update emits `anchor.transition.v1`; the durable boundary validates
coverage, sources, replacement IDs, and demotion references; and Context projects
Contract core plus current cognition instead of the complete Checkpoint object.
Exact recall now resolves immutable Checkpoint item locators through the Active
`anchor_recall` tool or `/anchor recall`; it verifies the Checkpoint and optional
item content hash and returns the result to Pi's current work window. It does not
rewrite State. The next code stage is limited to fresh-Agent takeover checks and
long-update churn acceptance. See the normative details in
`docs/ARCHITECTURE.md` v0.5.
