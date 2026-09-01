# Anchor Architecture

Status: normative target, version 0.5

Anchor is one Pi Extension whose purpose is to preserve correct task cognition
across compaction, restart, and long-running work. It does not replace Pi and is
not a general Agent runtime, memory service, or governance layer.

This document distinguishes the target architecture from the current runtime.
Sections 1 through 14 define the target. Section 15 records what the repository
implements today and must not be read as a claim that the complete target is
already available.

## 1. Problem

A long-running Agent transcript mixes durable intent with temporary reasoning,
tool output, errors, corrections, obsolete plans, and conversation. As it grows,
the model must repeatedly infer what is currently true from a chronology. A
normal compact reduces length, but it cannot reliably decide which old detail
will still change future behavior.

Two failure modes follow:

- conservative summaries retain everything and become another growing ledger;
- aggressive summaries silently remove a correction, constraint, failure cause,
  or unresolved dependency that later changes the right action.

Anchor separates the authorities:

```text
Pi transcript       what happened
Anchor Checkpoint   what must currently be understood
Artifacts           exact evidence too large for current attention
Pi active window    recent work not yet absorbed by a Checkpoint
```

The transcript remains available for UI, audit, and debugging. It is not the
normal oracle for continuing a long task.

## 2. Core thesis: minimal sufficient cognition

Anchor does not try to remember as much as possible. It continuously constructs
the smallest cognition that still lets a fresh Agent continue correctly:

```text
S(t+1) = Update(S(t), Episode(t))
C(t)   = Project(S(t)) + Pi Active Window
```

- `S` is authoritative task cognition, not a transcript summary.
- `Episode` is the exact uncovered work selected by Pi.
- `Update` assimilates, revises, resolves, forgets, and indexes.
- `Project` deterministically exposes current cognition to the next invocation.
- `C` is disposable model Context, never durable truth.

The optimization objective is behavioral, not a token quota:

```text
minimize size(Project(S))

subject to:
a fresh Agent given Project(S) + the recent window can still choose the
correct next action without reconstructing the task from full history.
```

State is therefore a task-specific sufficient statistic. This is an engineering
criterion, not a claim that code can prove semantic equivalence to full history.

Under stable task complexity, projected Anchor Context should approach a stable
size even as transcript length and Checkpoint count continue to grow.

## 3. Forgetting is attention allocation

Forgetting is not failed storage or physical deletion. It is the deliberate
movement of information out of current attention.

Update evaluates information with one counterfactual question:

> If the next Agent cannot see this item, could it make a wrong decision,
> violate a constraint, repeat failed or expensive work, or lose a necessary
> next step?

The answer determines its attention level:

```text
Active Cognition
  Directly affects current decisions or action.
  Always projected.

Dormant Knowledge
  Not currently actionable, but losing it could make future recovery expensive
  or unsafe. Represented by a concise, exact recovery reference.

Event Archive
  No longer expected to affect normal work. Old Checkpoints, transcript, and
  Artifacts retain it for explicit recovery, audit, or debugging.
```

These are cognitive levels, not merely storage tiers. Dormant information does
not consume detailed model attention. Archived information does not participate
in normal task cognition.

Recall is explicit and targeted:

```text
Dormant reference or archive cue
              |
          exact recall
              |
       Pi current work window
              |
         a later Update
              |
  promote, retain, or forget again
```

Recovered material does not become permanently projected merely because it was
read once.

## 4. Ownership

```text
Pi
  model transport, tools, permissions, transcript, active window,
  compaction boundary, retry, interruption, session tree, and TUI

Anchor Extension
  mode, Planning, Update Agent, validation, deterministic projection,
  recall, and compact receipt replay

Anchor State
  one session Task, immutable Checkpoints, current materialized head,
  and immutable Artifacts when exact externalized evidence is needed
```

Pi remains the authority for execution and workspace effects. Anchor does not
implement a project sandbox, scheduler, reviewer authority, role system, Agent
pool, or parallel writer protocol.

## 5. State model

### 5.1 Task Contract

One Pi session may own one Anchor Task. Its Contract contains:

```text
goal
acceptance criteria
constraints
non-goals
risks
verification
confirmed execution plan
```

An explicitly planned Contract is user-confirmed and cannot be silently
rewritten. A first compaction may instead create a `provisional` Contract from
the exact Pi Episode. Provisional fields may be revised by later explicit user
corrections or an eventual Planning confirmation; unknown requirements remain
unknown rather than being invented.

Normal Context projects only the Contract core required for decisions: goal,
acceptance criteria, constraints, and non-goals. Other Contract material remains
durable and may be projected when relevant.

### 5.2 Checkpoint

A Checkpoint is an immutable revision containing:

```text
Checkpoint
  task identity
  checkpoint version
  parent version and content hash
  source frontier
  current cognition
  transition certificate
  provenance
```

The latest valid Checkpoint is current truth. Older Checkpoints preserve revision
history and recovery material; they are never concatenated into normal Context.

The source frontier binds a compact Checkpoint to the Pi session,
`firstKeptEntryId`, split-turn state, and a digest of the exact Episode. Planning
creates Checkpoint 0 with a frontier bound to the accepted proposal. A Checkpoint
without a valid frontier cannot replace any history.

### 5.3 Current cognition

The structure is stable because it describes cognitive obligations. Its contents
are dynamic and rebuilt at every Update:

```text
Cognition
  Situation
    current understanding
    confirmed facts
    active hypotheses
    unresolved conflicts
    blockers

  Experience
    decisions that still constrain choices
    failed paths, causes, and retry conditions
    lessons that still alter future strategy

  Intent
    current user directive
    accepted next action
    next plan
    open questions

  Knowledge Index
    currently relevant Dormant Knowledge and Artifact references
```

The immutable Contract supplies Purpose and therefore is not duplicated inside
Cognition.

Cognition preserves a causal model rather than a chronology:

```text
Contract goal - Situation = current gap

current gap + Contract constraints + relevant Experience = Intent
```

For example, this is not useful Experience:

```text
Command X was run and failed.
```

This may be useful Experience:

```text
Path X fails because condition Y is absent; do not retry until evidence Z exists.
```

### 5.4 Cognition items

Items that must survive individual Updates have stable identity and local
provenance. A conceptual item is:

```json
{
  "id": "fact-17",
  "kind": "fact",
  "statement": "APPROVAL-77 remains pending.",
  "sources": ["episode:4:tool-result:2"],
  "relevance": "Blocks release readiness."
}
```

Stable identity exists to account for transitions, not to turn State into a
knowledge graph. Item kinds are limited to the cognitive obligations above.
No generic relation system or vector representation is part of the core.

Current facts require evidence provenance. Hypotheses must remain visibly
unverified. A model's confidence is not evidence. A failed or interrupted tool
call may support a failure, observation, or blocker; it cannot prove completion.

### 5.5 Knowledge references and Artifacts

A knowledge reference contains only what is needed to decide whether and how to
recall information:

```text
reference identity
short relevance cue
exact locator
content hash when content-addressable
source Checkpoint or Episode provenance
```

A locator may identify an immutable Anchor Artifact, old Checkpoint item, Pi
entry/tool result, or independently immutable project evidence. `demote` is valid
only when its locator is resolvable and its content binding can be checked.

The Knowledge Index is itself subject to semantic forgetting. It is not a list
of every Artifact ever created. Information that no longer has plausible future
behavioral value leaves the current index and remains discoverable only through
explicit archive inspection.

Anchor does not require vector search. Exact references and bounded textual
lookup are sufficient until real recall failures prove otherwise.

## 6. Update

Pi owns the boundary and supplies the Episode:

```text
previous Checkpoint
+ preparation.messagesToSummarize
+ preparation.turnPrefixMessages
                 |
                 v
          Update Agent
  request-local schema-constrained submission
  assimilate / revise / resolve / forget / index
                 |
                 v
 Candidate Cognition + Transition Certificate
                 |
                 v
       deterministic validation
                 |
                 v
          durable Checkpoint
                 |
                 v
       Pi CompactionResult receipt
```

The Update Agent's semantic evidence is exactly the previous Checkpoint and that
Episode. A deterministic control envelope may also carry the immutable Contract
and target frontier. They constrain the transition but are not new evidence or
Episode directives. Anchor never sends the complete branch or transcript.

The candidate crosses the model boundary only as the arguments of one
request-local `anchor_submit_update` function. The function has no implementation
or side effects: Anchor does not register it as a Pi session tool, execute it, or
append a tool result. Providers that support strict JSON-schema tools constrain
sampling directly; other supported providers still receive one mandatory
function choice and Anchor applies the same deterministic validation to its
arguments. A candidate rejected by deterministic validation may receive one
validation-only correction request; the second invalid candidate is rejected
without changing the previous Checkpoint.

Update is not a summary task. It performs five semantic operations:

```text
Assimilate   incorporate new facts, outcomes, and user direction
Revise       correct old cognition using newer evidence
Resolve      close conflicts, questions, and blockers whose outcome is known
Forget       remove material that no longer changes future behavior
Index        demote costly but potentially reusable detail to exact references
```

It emits a complete new cognition snapshot. It does not emit chronological prose
or a free-form patch.

## 7. Transition Certificate

Every previously active cognition item receives exactly one disposition:

```text
carry       remains active with the same identity and meaning
revise      remains active but is corrected by named new evidence
resolve     leaves the active set because its issue has a known outcome
supersede   leaves the active set because a newer directive or fact replaces it
demote      leaves detailed attention but remains exactly recoverable
archive     no longer has expected future behavioral value
```

Each non-carry disposition records a reason and source. `revise` identifies its
updated item; `supersede` identifies the replacement; `demote` supplies a valid
knowledge reference. Resolution outcomes that still affect future behavior must
be represented by a successor fact, decision, or lesson.

The complete set of dispositions is the Transition Certificate. It is stored
with the Checkpoint for validation and audit, but is excluded from normal model
Context.

The certificate makes forgetting accountable without pretending that code can
judge all natural-language truth. Deterministic validation can ensure:

- no previous active item disappears silently;
- carried items still exist and revised items have successors;
- new authoritative facts cite allowed sources;
- newer user corrections explicitly supersede old directives;
- failures are not transformed into successful completion evidence;
- demoted information has a content-bound recovery path;
- Task, Contract, parent, frontier, and provenance remain valid.

The model still decides semantic relevance and meaning. The validator establishes
coverage and evidence discipline, not universal semantic correctness.

## 8. Context projection

Every Active model request receives:

```text
Pi fixed system / developer / tool prefix
+ immutable Task Contract core
+ latest Active Cognition
+ compact Knowledge Index
+ Pi compaction-aware active window, unchanged
```

The latest real user message remains in its natural conversation position. A
new explicit directive in Pi's active window takes immediate precedence and is
folded into cognition at the next Update.

Projection does not include:

- the complete Checkpoint persistence object or receipt;
- the Transition Certificate;
- directive prose history;
- resolved issue detail;
- full Artifact content or large tool output;
- repeated facts;
- old Checkpoints or summary chains.

Projection is deterministic local work. It does not invoke an LLM, scan the full
transcript, replay every Checkpoint, or impose an Anchor-private token budget.
Pi and the provider continue to own active-window handling and context limits.

## 9. Initialization and session lifecycle

```text
new interactive session -> Normal Pi (no State)

Normal -- first Pi compact --> Bootstrap Update -> provisional Task + Checkpoint 0

Normal -- /anchor start --> read-only Planning
                                |
                         sealed proposal
                                |
                       explicit user Accept
                                |
                   Task + Checkpoint 0 -> Active

Active -> Pi compact boundary -> Anchor Update -> next Active window
```

Planning uses the current Pi conversation as negotiation material. The model may
inspect with read-only tools and ask focused questions. It must clarify the goal,
acceptance, constraints, non-goals, risks, verification, and initial causal
understanding before proposing.

`anchor_propose` creates a candidate and ends the proposing turn. Only after Pi
persists the complete turn may the proposal be sealed for review. A later user
prompt or different current tree leaf makes it stale. Accept is idempotent by
proposal hash.

Default State lives at:

```text
<Pi agent dir>/anchor/sessions/<session-id>/anchor.db
```

The Pi session identity locates the Task. Resume reopens the same State without
calling a model. Transcript tree navigation does not roll State back. A fork or
new Pi session receives a new identity and does not inherit writable State.

User-visible modes remain only `Normal`, `Planning`, and `Active`. Update is a
transaction, blocked is a health condition, and completion is Task State.
Normal mode has no Anchor prompt, model tool, State file, or projection. Its
first compaction may lazily bootstrap Anchor; if bootstrap fails, Pi's native
compact proceeds and no Anchor State is written.

## 10. Compact receipt and recovery

Checkpoint commit and Pi compaction append are separate durable writes. Anchor
does not claim cross-file atomicity.

The committed Checkpoint is also a content-bound receipt. If the process exits
after State commit but before Pi appends the compaction entry, the same Pi
preparation produces the same frontier and Anchor returns the stored receipt
without another model call. Resume completes an undelivered compact before any
new Active request.

A delivered receipt must match:

```text
Task identity
Checkpoint version and content hash
Checkpoint event identity
complete source frontier
```

Partial metadata matches are not delivery. A changed frontier or receipt fails
closed and leaves the prior durable State untouched. Pi's compaction entry is a
readable marker plus receipt metadata, not a second cognition authority.

## 11. Recall

Recall is a small capability, not another memory runtime:

1. resolve one explicit Knowledge reference or archive locator;
2. verify identity and content hash where applicable;
3. return a bounded slice or materialize exact content outside model Context;
4. let the working Agent inspect only what the current question requires;
5. allow a later Update to promote any newly relevant conclusion.

Normal projection never scans all Artifacts or searches the full transcript.
Broad discovery is an explicit diagnostic action and does not rewrite State by
itself. Recalled historical user text is evidence about history, never a current
directive unless the user renews it.

## 12. Quality criterion

Every candidate Checkpoint should pass the semantic handoff question:

> Can a fresh Agent, seeing only the Contract, projected candidate cognition,
> Knowledge Index, and Pi recent window, continue without guessing a critical
> decision or reconstructing the task from full history?

It should be able to answer:

- What is the long-term goal and what is explicitly out of scope?
- What is currently true, uncertain, conflicting, or blocked?
- What does the user currently want?
- Which decisions and constraints still govern choices?
- Which failed paths must not be repeated, and under what retry condition?
- What is the current gap and next concrete action?
- Where can costly omitted detail be recovered exactly?

Missing a consequential answer indicates over-forgetting. Retaining material that
cannot change any of these answers indicates under-forgetting.

This semantic quality cannot be proven entirely by deterministic code. It is
tested through adversarial Update fixtures, fresh-Agent takeover evaluation, and
real long-task behavior. Mechanical validation remains fail-closed around the
parts it can prove.

## 13. Performance and acceptance

The hot path reads the latest materialized Checkpoint, compiles one projection,
and invokes Pi. It never rescans all history or Artifacts. Projection latency and
model-facing size are measured at p50 and p95.

Long-task acceptance includes:

- multiple compactions and process restart;
- stale candidate and receipt mismatch rejection;
- user correction and directive supersession;
- failed tool calls and repeated-failure prevention;
- unresolved conflict retention and later resolution;
- evidence provenance on authoritative facts;
- demotion followed by exact recall and optional promotion;
- over-forgetting checks with a fresh-Agent takeover;
- 50 to 100 Update churn under stationary task complexity.

The churn gate passes when projected Anchor input approaches a plateau rather
than growing with Update count. No arbitrary `maxFacts`, token quota, checkpoint
budget, or lossy `slice(0, N)` may be used to manufacture that result.

If current task complexity itself exceeds the selected model window, Anchor must
surface the condition and rely on explicit task decomposition or externalized
evidence. It must not silently discard required cognition.

## 14. Non-goals and host limits

The core does not include:

- a Pi fork or replacement runtime;
- full-transcript memory in normal Context;
- proactive per-turn or model-chosen compact triggers;
- asynchronous Update merging;
- per-turn model-based projection;
- path or shell authorization;
- workspace writer leases;
- completion governance or reviewer roles;
- schedulers, Agent pools, vector memory, or a knowledge graph;
- Anchor-private context, time, tool-count, or checkpoint budgets.

Pi currently validates model selection and provider authentication before firing
the Extension compact hook. Even exact receipt replay through `ctx.compact()`
therefore requires Pi's model/auth precondition.

Pi also exposes one process-wide active-tool list rather than per-Extension tool
ownership. Planning restores the list captured on entry, so another Extension's
concurrent visible-tool change may be overwritten on exit. Its own execution
hooks remain authoritative.

## 15. Current implementation and target delta

The repository currently implements the lifecycle foundation and the first
minimal-sufficient-cognition slice:

- Pi Extension modes, read-only Planning, sealed proposal review, and explicit
  user acceptance;
- lazy first-compaction Bootstrap Update with provisional Task/Checkpoint 0 and
  native compact fallback;
- session-scoped SQLite containing one Task and immutable Checkpoints;
- `anchor.checkpoint.v1` with source frontier, parent hash, and provenance;
- `anchor.cognition.v3` with Situation, Experience, Intent, Knowledge Index,
  stable item identity, item provenance, and a v2 read/commit compatibility path;
- `anchor.transition.v1` with complete previous-item coverage, source checks,
  replacement checks, and demotion references;
- request-local schema-constrained serial Update over exactly Pi's preparation
  Episode, with direct argument validation and no Pi project-tool execution;
- stale-version CAS, content-bound compact receipt, crash replay, and fail-closed
  mismatch recovery;
- bounded deterministic projection of Contract core, current cognition, and
  frontier metadata followed by Pi's unchanged active window;
- resume, transcript-tree, fork, and Normal-mode isolation.

The remaining target gaps are:

- a pure semantic-proposal reducer is available as an isolated compatibility
  path (`src/reducer.js`), but the primary provider tool and Bootstrap/Update
  runtime still use the v1/v3 full-snapshot protocol;

- the runtime has no separate immutable Artifact store; exact recall currently
  supports only immutable Checkpoint item locators
  (`checkpoint:<version>:item:<id>`);
- old v2 compatibility is intentionally permissive at the durable boundary and
  should be removed after pre-release sessions are no longer relevant;
- provisional Bootstrap has no separate Contract-promotion command yet; later
  explicit corrections are represented through cognition until that need is
  demonstrated;
- focused fresh-Agent takeover and 100-Update projection plateau tests exist;
  real-provider long-task acceptance and production p50/p95 measurements do not
  yet exist;
- overflow compact remains unverified with a real provider.

The implementation order is therefore deliberately narrow:

1. run correction, failure, recall, takeover, restart, and churn acceptance;
2. remove the v2 compatibility path when the pre-release State reset window is
   closed.

Everything else waits for evidence that this cognitive loop is insufficient.

Anchor's architectural identity is:

> Anchor is a long-running Agent cognitive state machine. At Pi lifecycle
> boundaries it uses structured, accountable, and reversible forgetting to turn
> task history into minimal sufficient cognition, so model Context scales with
> current task complexity rather than elapsed history.
