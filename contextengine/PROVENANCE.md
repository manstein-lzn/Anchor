# Provenance

**This directory is a verbatim copy of an earlier, unrelated project that also
happened to be called Anchor. It is archived for study, not yet integrated.**

None of the files under this directory are part of the current Anchor runtime.
Nothing here is imported, executed, or exercised by the test suite. It is a
reference archive.

## What it is

`anchor-pi` — "Durable planning and cognition updates for long-running Pi
sessions". A Pi extension, plus a dependency-free Python state core:

```text
src/extension.js      30,905 B   the Pi extension itself
src/update.js         40,155 B   the cognition update protocol
src/reducer.js         9,100 B   deterministic materialisation of a proposal
src/context.js         3,460 B   the context projection
src/store.js           4,459 B
python/anchor_core/   35,209 B   the durable state core (stdlib only)
docs/                99,424 B    product spec, architecture, update protocol, reviews
test/                62,870 B    six suites
```

## Why it is here

The current repository replaced a repository of the same name, so the earlier
history became unreachable by ordinary means — no branch, tag or reference
pointed at it any more. The commits are recoverable through GitHub's public
events, which record every push with its `before` and `head` SHAs:

```text
2026-08-31  PublicEvent / CreateEvent
2026-09-01  f18e1a34..  ->  f61ff03e..     "feat: bootstrap Anchor at first compact"
2026-09-04  f61ff03e..  ->  feefcf6494     "Fix Anchor recent-context retention and receipt recovery"
2026-09-07  feefcf6494  ->  2a3c714cb1     (the current project's history takes over)
```

The archived tree is the worktree of **`feefcf64940e`**, fetched as a tarball
from `codeload.github.com`. Every file is byte-identical to that commit; the
copy was verified by comparing `sha256sum` over the whole tree.

Cut-off commit: `feefcf64940e` (2026-09-04T15:45:22Z, "Fix Anchor
recent-context retention and receipt recovery").

**Those commits are unreferenced, so GitHub may garbage-collect them.** This
directory is now the durable copy.

## Why it is worth studying

Its subject is the one this project keeps arriving at: what an agent's context
should contain over a long run, and what part of its accumulated state is
authority. Several of its statements are further along than the approach the
current project has been exploring:

- **Checkpoint = State revision + source frontier**, where the frontier names the
  exact history boundary the cognition has absorbed. A checkpoint without a
  frontier is not allowed to replace anything in the transcript. That turns
  "cognitive drift" into a checkable proposition.
- **Context is a projection**: `fixed prefix + latest checkpoint + active window`,
  read deterministically, with no LLM call and no re-summarising. It divides
  responsibility cleanly: before the frontier the checkpoint answers, after it
  the active window does.
- **Prompt handles intelligence; code handles mechanical truth.** The model
  proposes semantic operations; code materialises them, owns identity and copy,
  and emits a Transition Certificate. Copy-on-carry makes "keep this fact" a
  structural guarantee rather than a request the model can half-honour.
- **Update is a transactional staging boundary**: request-local, side-effect
  free, discarded on failure, with exactly one durable commit and no silent
  degradation of a rejected proposal.

`docs/PRINCIPLES.md` is eight paragraphs and is the fastest way in.
`docs/PRODUCT_SPEC.md` defines the objects; `docs/ANCHOR_UPDATE_PROTOCOL_V2.md`
(internally v3) is the mechanism.

## Status

Unmodified and unread beyond its documentation. It runs on Node 22.19+ and Pi
0.84.2 with `typebox`; it is not wired into anything here. Do not move files out
of this directory or edit them in place — an analysis can be corrected, an
archive cannot.
