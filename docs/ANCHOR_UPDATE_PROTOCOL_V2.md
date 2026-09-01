# Anchor Update Protocol v2

## 设计提案：从模型生成完整 State 转向语义操作与确定性物化

状态：Proposal
目标版本：Anchor Update Protocol v2
适用范围：Anchor Bootstrap、Update、Checkpoint candidate 生成与验证
不改变：Pi 的 compaction ownership、Anchor 的 Task/Checkpoint 持久化边界、frontier/CAS/receipt 语义

## 1. 摘要

Anchor 的核心持久化模型是正确的：它保留 Task、Contract、Checkpoint、frontier、provenance 和 receipt，并在候选不可信时 fail closed。当前最主要的不可靠性位于 Update Agent 的输出协议，而不是 SQLite 或 Checkpoint 链本身。

当前 Update Agent 被要求一次性生成：

- 完整的新 cognition snapshot；
- 完整的 Transition Certificate；
- 所有旧 item 的 identity；
- 所有 carry、revise、supersede 和 demote 关系；
- 所有 Knowledge Index reference；
- 可能的 content hash。

这让模型同时承担语义判断和确定性 bookkeeping。JSON Schema 可以保证字段类型，却不能保证跨字段关系，例如：

```text
transition.disposition = carry
=> 同一个 item_id 必须存在于新 cognition
```

因此，Anchor 经常遇到“tool call 成功但语义候选失败”的情况，例如：

```text
carried item is missing from cognition
```

Update Protocol v2 改变这个边界：

```text
模型提交语义变更意图
        |
        v
Anchor request-local reducer
        |
        v
确定性生成完整 cognition 与 Transition Certificate
        |
        v
完整候选校验
        |
        v
一次 durable Checkpoint commit
```

模型不再重新打印旧 State，也不再直接维护重复的 cognition/certificate 结构。Anchor 负责把语义操作物化为完整、可验证、可持久化的 Checkpoint candidate。

## 2. 核心原则

### 2.1 Prompt handles intelligence; code handles mechanical truth

模型负责：

- 从 previous Checkpoint 和 Episode 理解当前变化；
- 判断哪些事实、决定、失败和问题仍然影响未来行为；
- 判断旧 item 是 carry、revise、resolve、supersede、demote 还是 archive；
- 提出新的 cognition item、当前 directive 和下一步。

Anchor 负责：

- previous item 的 identity、位置和内容复制；
- item decision 的覆盖、唯一性和引用完整性；
- replacement 与 new item 的关系；
- Transition Certificate 的生成；
- reference 的格式和可恢复性检查；
- content hash 的计算或验证；
- frontier、Task、parent version 和 provenance 校验；
- durable commit 和 compact receipt。

任何可以由确定性代码可靠完成的工作都不应交给模型重复生成。

### 2.2 API 是事务性 staging boundary，不是直接数据库写入接口

模型使用的 API 必须是请求内、无副作用、可丢弃的 staging API：

- 不注册为 Pi session tool；
- 不执行 workspace 工具；
- 不直接写 SQLite；
- 不产生持久化 tool-result transcript；
- 不允许部分操作提前改变 durable State；
- 所有操作先进入 request-local draft；
- 只有完整 draft 通过确定性验证后，才调用 Anchor 的一次 durable update。

这保留 Anchor 对 State commit 的所有权，也避免模型中断、重试和 Pi transcript 与 SQLite 状态之间产生部分提交。

### 2.3 完整 State 仍然是 Anchor 的 durable truth

v2 不降低 cognition、Transition Certificate 或 provenance 的严格性。变化只在于谁负责生成它们：

```text
模型：semantic proposal
Anchor：complete candidate
Store：durable Checkpoint
```

最终写入的 Checkpoint 仍必须包含完整当前 cognition 和完整 Transition Certificate。模型不能通过省略字段绕过覆盖、source、reference、Task identity 或 frontier 校验。

### 2.4 失败不等于成功，修复不等于静默接受

如果 semantic proposal 无法被 reducer 物化，Anchor 必须拒绝它。允许一次带确定性错误反馈的重新提交，但不能：

- 静默把 archive 改成 carry；
- 静默为模型补充未声明的新事实；
- 静默丢弃无法解析的 operation；
- 静默将 provider failure 变成空状态；
- 在候选不完整时写入 Checkpoint。

## 3. 当前实现与目标实现

### 3.1 当前实现

当前流程大致为：

```text
previous Checkpoint + Episode
        |
        v
模型生成完整 anchor.cognition.v3
        |
        +-- 完整 Transition Certificate
        |
        v
TypeBox tool schema
        |
        v
Anchor 语义校验
        |
        v
Checkpoint commit
```

当前已经具备的保护：

- structured tool submission；
- strict JSON Schema provider capability；
- 唯一 tool call；
- 本地 schema 校验；
- Transition Certificate 覆盖校验；
- frontier、parent version、provenance 校验；
- 一次 validation-only correction attempt；
- 非法候选不写 State。

这些保护仍然有效，但无法消除模型重复生成完整 State 所带来的错误面。

### 3.2 目标实现

v2 流程为：

```text
previous Checkpoint + selected Episode
        |
        v
模型提交 semantic update proposal
        |
        v
Anchor 校验 proposal schema
        |
        v
Anchor reducer 读取 previous Checkpoint
        |
        +-- 复制 carry item
        +-- 应用 revise / resolve / supersede / demote / archive
        +-- 插入 new item
        +-- 生成 Transition Certificate
        +-- 生成 Knowledge Index
        |
        v
Anchor 校验完整 candidate
        |
        v
anchor.update(candidate, expected_version)
        |
        v
Pi compaction receipt
```

模型输出不再包含最终 `anchor.checkpoint-candidate.v1`，而是一个版本化的 semantic proposal。

## 4. v2 Semantic Proposal

### 4.1 顶层结构

建议保留现有 tool 名称 `anchor_submit_update`，但改变其 arguments 的语义：

```json
{
  "schema": "anchor.update-proposal.v2",
  "situation": {
    "current_understanding": "The adapter now uses the provider boundary.",
    "new_items": [
      {
        "section": "confirmed_facts",
        "statement": "The provider returns structured tool calls.",
        "sources": ["episode:compact:abc"],
        "relevance": "Determines the next verification step."
      }
    ]
  },
  "intent": {
    "current_directive": "Finish provider verification.",
    "accepted_next_action": "Run the focused test.",
    "next_plan": ["Run the focused test."],
    "open_questions": []
  },
  "item_decisions": [
    {
      "item_id": "fact-1",
      "disposition": "carry"
    },
    {
      "item_id": "decision-2",
      "disposition": "archive",
      "reason": "The implementation now embodies this decision.",
      "sources": ["episode:compact:abc"]
    },
    {
      "item_id": "fact-3",
      "disposition": "revise",
      "replacement": {
        "statement": "The provider returns one structured submission call.",
        "sources": ["episode:compact:abc"],
        "relevance": "Controls the next compaction path."
      }
    },
    {
      "item_id": "failed-4",
      "disposition": "demote",
      "reason": "The failed path remains useful for avoiding a repeated mistake.",
      "sources": ["episode:compact:abc"],
      "reference": "checkpoint:2:item:failed-4"
    }
  ],
  "new_items": [
    {
      "section": "experience.decisions",
      "statement": "Use the structured provider boundary for Update submission.",
      "sources": ["episode:compact:abc"],
      "relevance": "Prevents free-text output failures."
    }
  ],
  "knowledge_index": [
    {
      "cue": "Previous failed Update path",
      "locator": "checkpoint:2:item:failed-4",
      "source": "episode:compact:abc"
    }
  ]
}
```

The exact field names may change during implementation, but the separation is normative:

- `item_decisions` contains decisions about previous active items;
- `new_items` contains genuinely new or replacement cognition;
- `situation` and `intent` contain fields that are not item-by-item transitions;
- the model does not repeat the full previous item body for `carry`;
- the model does not submit the final certificate;
- the model does not submit durable Checkpoint metadata.

### 4.2 Item decision semantics

Each previous active item must be addressed exactly once.

| Disposition | Model submits | Reducer behavior |
|---|---|---|
| `carry` | `item_id` only | Copy the previous item unchanged into the same cognition group |
| `revise` | `item_id`, replacement item, reason, sources | Replace the old item with a new item and emit a revise certificate entry |
| `resolve` | `item_id`, reason, sources | Remove the item; require any still-relevant outcome as a new item |
| `supersede` | `item_id`, replacement item, reason, sources | Remove old item, insert replacement and bind replacement identity |
| `demote` | `item_id`, reason, sources, exact reference | Remove detailed item and create/retain a Knowledge Index reference |
| `archive` | `item_id`, reason, sources | Remove item from active cognition and retain the certificate record |

The reducer must reject:

- unknown `item_id`;
- duplicate `item_id` decisions;
- missing decisions for previous active items;
- `carry` with a replacement payload;
- `revise` or `supersede` without a valid replacement;
- `demote` without an exact recoverable reference;
- replacement items with invalid sources or empty statements;
- a replacement that reuses an unrelated active item ID;
- an operation that creates an item in an invalid cognition section.

### 4.3 Copy-on-carry

`carry` is deliberately small because the reducer owns the old item body:

```text
previous.situation.confirmed_facts[fact-1]
        |
        +-- item_decisions: { item_id: fact-1, disposition: carry }
        |
        v
next.situation.confirmed_facts[fact-1] = exact previous item
```

This makes the invariant deterministic. The model can no longer claim `carry` while accidentally omitting the item body.

If the model wants to change the statement, source, relevance or section, it must use `revise` or `supersede` and provide an explicit replacement.

### 4.4 New item identity

New item IDs should be generated by Anchor, not invented by the model. The model may provide an optional stable proposal key for feedback, but the durable item ID is assigned deterministically from:

- previous Checkpoint version;
- operation index;
- section;
- normalized replacement content;
- source frontier.

The exact ID algorithm must be content-bound and documented before implementation. It must not depend on random UUIDs if deterministic replay is required.

## 5. Deterministic Reducer

### 5.1 Reducer contract

The reducer is a pure function:

```text
reduce(previous_checkpoint, semantic_proposal, target_frontier)
  -> complete_checkpoint_candidate
```

It must not:

- call an LLM;
- inspect the complete transcript;
- execute project tools;
- read arbitrary workspace paths;
- write SQLite;
- mutate the previous Checkpoint;
- invent evidence or user-confirmed requirements.

It may read only:

- the recovered previous Checkpoint;
- the immutable Contract projection required by the Update contract;
- the validated semantic proposal;
- the target frontier;
- deterministic reference metadata already available to Anchor.

### 5.2 Reducer phases

The reducer should execute in this order:

1. Validate proposal schema and version.
2. Build the previous active item ledger:
   ```text
   item_id -> section, field, exact item body
   ```
3. Validate that every previous item has exactly one decision.
4. Validate operation-specific fields.
5. Copy all `carry` items exactly.
6. Apply `revise`, `resolve`, `supersede`, `demote` and `archive` operations.
7. Insert new items and replacements into their declared sections.
8. Normalize current understanding and Intent.
9. Build Knowledge Index references.
10. Generate the complete Transition Certificate.
11. Validate sources, references, replacement IDs and active item uniqueness.
12. Return a complete candidate without writing it.

### 5.3 Certificate generation

The model does not generate the final certificate. Anchor creates it from the accepted operations.

For example:

```json
{
  "schema": "anchor.transition.v1",
  "dispositions": [
    {
      "item_id": "fact-1",
      "disposition": "carry",
      "reason": "Carried unchanged by the reducer.",
      "sources": ["episode:compact:abc"]
    }
  ]
}
```

For `carry`, the reducer can use a standard deterministic reason or preserve a short model-supplied rationale if the protocol requires it. The reason must never be used to override the actual reducer operation.

This removes the duplicated cross-field assertion from model output while preserving the durable audit record.

## 6. Knowledge Index and Hashes

### 6.1 Model responsibility

The model may decide:

- that detail should be demoted;
- which immutable locator should be retained;
- why the reference matters;
- which source supports the reference.

### 6.2 Anchor responsibility

The model must not invent or calculate `content_hash`.

Anchor computes `content_hash` only when it has the immutable content and the relevant ownership boundary permits reading it. If Anchor cannot bind the content, the reference contains no hash and remains subject to the existing recoverability policy.

The target proposal schema should therefore omit model-authored `content_hash` entirely. If a compatibility field is temporarily retained, it must be ignored unless Anchor independently recomputes and matches it.

This removes an irrelevant language-model task and prevents failures such as:

```text
knowledge_index.content_hash must be a sha256 digest
```

from occurring during Bootstrap or Update.

## 7. Bootstrap v2

Bootstrap has an even weaker need for model-authored full State. At first compaction, the model should submit only a provisional semantic proposal:

```json
{
  "schema": "anchor.bootstrap-proposal.v2",
  "title": "Ship the adapter",
  "goal": "Ship the adapter without changing the API.",
  "uncertainties": [
    "Acceptance criteria are not yet confirmed."
  ],
  "intent": {
    "current_directive": "Inspect the adapter.",
    "accepted_next_action": "Inspect the adapter.",
    "next_plan": ["Inspect the adapter."],
    "open_questions": []
  },
  "new_items": []
}
```

Anchor then creates:

- provisional Contract;
- complete initial cognition;
- empty decisions and failed paths unless explicitly supported;
- empty Knowledge Index unless a valid reference is available;
- deterministic frontier and provenance;
- Checkpoint 0.

Bootstrap must never promote unknown information to user-confirmed Contract content.

The Bootstrap model should not produce:

- content hashes;
- Checkpoint receipt fields;
- parent version;
- frontier fields;
- transition certificate;
- durable State version;
- unsupported acceptance criteria or constraints.

## 8. Structured Provider Boundary

### 8.1 Function call

The request-local submission function remains the model transport boundary:

```text
anchor_submit_update
anchor_submit_bootstrap
```

The function arguments are semantic proposals, not final durable Checkpoints.

Provider behavior remains:

- strict JSON Schema where supported;
- mandatory function choice or provider-equivalent required tool choice;
- exactly one submission call;
- no project tools;
- no ordinary session tool registration;
- no direct function executor;
- no free-text JSON fallback.

### 8.2 What JSON Schema should enforce

The schema should enforce local shape:

- known schema version;
- closed objects;
- valid section names;
- non-empty statements;
- non-empty sources where required;
- valid disposition enum;
- operation-specific required fields where expressible;
- no model-authored durable metadata.

The reducer should enforce relational semantics:

- previous item coverage;
- item identity;
- carry copy semantics;
- replacement existence;
- demotion recoverability;
- Knowledge Index consistency;
- source policy;
- Contract and frontier boundaries.

Do not attempt to encode all relational rules into a giant JSON Schema. The schema should remain readable and provider-compatible; the reducer is the correct authority for cross-field invariants.

## 9. Retry and Failure Model

### 9.1 One correction attempt

If the proposal is syntactically valid but reducer validation fails, Anchor may send one deterministic feedback request:

```text
The previous semantic proposal was rejected by deterministic validation:
<bounded validator reason>

Correct only that issue and submit the complete proposal again through
anchor_submit_update. Do not add new evidence or change the Contract.
```

The feedback is control information, not Episode evidence. It must not contain the full rejected model output.

### 9.2 No infinite repair loop

After the correction attempt:

- no Checkpoint is written if validation still fails;
- the old Checkpoint remains authoritative;
- Pi retains the usable active window;
- the error records stage, bounded reason, stop reason and output hash only;
- Anchor does not start a hidden loop of model calls.

### 9.3 Failure categories

Diagnostics should distinguish:

```text
model-transport
submission-shape
reducer-validation
state-commit
receipt-delivery
```

A reducer-validation failure means the provider returned a structured proposal but the proposal was not a valid state transition. It is different from malformed JSON and different from a persistence failure.

## 10. Persistence and Concurrency

The v2 commit boundary remains:

```text
model response
  -> proposal validation
  -> pure reducer
  -> complete candidate validation
  -> Anchor.update(candidate, expected_state_version)
  -> receipt delivery
```

The reducer may run more than once during retry, but it must be pure and deterministic for the same previous Checkpoint, proposal and frontier.

Anchor must continue to reject:

- stale parent versions;
- stale frontier candidates;
- wrong Task identity;
- wrong session identity;
- wrong provenance;
- duplicate or mismatched receipt delivery.

The model must never receive authority to choose a Checkpoint version or bypass compare-and-swap.

## 11. Context and Evidence Boundaries

The semantic proposal receives exactly the current Update evidence:

```text
previous Checkpoint
+ preparation.messagesToSummarize
+ preparation.turnPrefixMessages
```

The request may also contain deterministic control:

- immutable Task Contract;
- target frontier;
- previous active item ledger or IDs;
- schema and operation rules;
- prior reducer validation feedback for one correction attempt.

It must not contain:

- the complete branch;
- the complete transcript;
- unrelated old Checkpoints;
- old summary chains;
- hidden model output from another task;
- arbitrary workspace content not present in the selected Episode or allowed recovery projection.

The reducer is not allowed to use its mechanical access to previous State as a reason to rescan transcript history.

## 12. Migration Plan

### Phase 0: Document and freeze boundaries

- Mark the current full-snapshot protocol as v1 compatibility behavior.
- Define proposal schemas independently from durable Checkpoint schemas.
- Preserve existing v2/v3 State readers and receipt replay.
- Add explicit stage names for submission, reducer and commit failures.

### Phase 1: Implement pure reducer

- Build `reduceUpdateProposal(previous, proposal, frontier)` as a pure module.
- Add focused tests for every disposition.
- Add coverage tests for omitted, duplicate and unknown item IDs.
- Add tests proving `carry` copies the exact previous item.
- Add tests proving reducer output is deterministic.
- Add tests proving the previous Checkpoint is not mutated.

### Phase 2: Add proposal tool schema

- Introduce `anchor.update-proposal.v2`.
- Remove model-authored `content_hash`.
- Remove final Checkpoint metadata from model arguments.
- Keep one request-local structured submission function.
- Validate arguments with TypeBox before reducer execution.

### Phase 3: Move Update runtime to proposal mode

- Send proposal tool to Update Agent.
- Run reducer locally.
- Run existing complete-candidate and semantic validation.
- Commit only reducer output.
- Retain one validation-only correction request.
- Keep old text parser only for explicit compatibility tests and migration tools.

### Phase 4: Move Bootstrap runtime to proposal mode

- Introduce `anchor.bootstrap-proposal.v2`.
- Let Anchor build provisional Contract and Checkpoint 0.
- Ensure content hashes are Anchor-owned.
- Preserve native Pi fallback and no-State-on-failure behavior.

### Phase 5: Remove obsolete output burden

After real-provider acceptance proves the new path stable:

- remove final Checkpoint fields from Update tool arguments;
- remove model-authored transition certificate from the primary path;
- remove model-authored content hashes;
- retain compatibility readers for existing durable State;
- remove old free-text runtime path after a documented migration window.

## 13. Acceptance Criteria

### 13.1 Protocol

- Update and Bootstrap use one structured request-local submission function;
- no free-text JSON is accepted as the runtime submission protocol;
- proposal schema is closed and provider-compatible;
- model cannot select Task identity, Checkpoint version, parent version or frontier;
- model cannot write SQLite or execute project tools through the submission boundary.

### 13.2 Reducer correctness

- every previous active item receives exactly one decision;
- `carry` always preserves the exact previous item body and ID;
- `revise` and `supersede` always produce valid replacements;
- `resolve` and `archive` cannot silently preserve or remove unrelated items;
- `demote` always leaves an exact recoverable reference;
- new item IDs are Anchor-owned and deterministic;
- reducer output is deterministic and side-effect-free;
- reducer never invents evidence, Contract requirements or user confirmation.

### 13.3 Persistence safety

- invalid proposal does not write State;
- invalid reducer result does not write State;
- stale candidate does not write State;
- provider failure does not write State;
- interrupted request does not write State;
- failed receipt delivery remains replayable;
- old Checkpoint remains authoritative after every failure.

### 13.4 Reliability

A real-provider test matrix must include:

- multiple compactions;
- more than one active item in every cognition group;
- carry-only update;
- revise and supersede in the same update;
- demotion with an exact reference;
- unresolved conflict and failed path preservation;
- user correction that supersedes an old item;
- malformed tool arguments;
- invalid cross-field proposal;
- one successful correction retry;
- a correction retry that still fails;
- process restart;
- receipt replay;
- stale candidate rejection;
- Bootstrap with no references;
- Bootstrap with a reference without content hash;
- provider transport failure.

### 13.5 Size and performance

- model-facing proposal size does not grow by repeating every carried item body;
- projected Anchor input approaches a plateau under stationary task complexity;
- reducer latency is measured separately from model latency;
- no full EventLog or transcript scan occurs on the Update hot path;
- retry does not create durable heartbeat records.

## 14. Relationship to MetaLoop

MetaLoop offers a useful architectural lesson, not a drop-in implementation.

The valuable pattern is:

```text
Agent expresses bounded intent
        |
        v
protocol API receives typed input
        |
        v
code reconciles and materializes mechanical truth
        |
        v
one durable lifecycle commit
```

MetaLoop's `task begin -> Work -> attempt finish` path does not ask the Agent to rewrite the complete SQLite protocol state. Its `attempt finish` routine performs reconciliation, Evidence binding, checkpointing, sealing and verification in code. Git remains the workspace truth and SQLite remains the protocol truth.

Anchor should apply the same separation to cognition:

```text
Agent expresses semantic cognition decisions
        |
        v
Anchor reducer materializes complete cognition
        |
        v
Anchor creates Transition Certificate
        |
        v
Anchor persists Checkpoint
```

Anchor should not import MetaLoop's Task/Attempt ontology, Git workspace model or Evaluation chain merely because the protocol pattern is useful. Anchor's existing Task, Checkpoint, frontier and Pi compaction boundaries remain the correct domain model for Anchor.

## 15. Non-goals

This upgrade does not introduce:

- a general Agent runtime;
- a scheduler or daemon;
- writer leases or agent pools;
- vector memory;
- a transcript replacement;
- a second Task ontology;
- automatic semantic truth detection;
- silent candidate repair;
- arbitrary model access to project files;
- a guarantee that every model proposal will be semantically correct;
- direct model authority over durable State.

The goal is narrower: reduce the amount of deterministic State assembly performed by the model while preserving strict cognition and transition validation.

## 16. Design Decision

Anchor Update Protocol v2 should be adopted as the target architecture.

The decisive invariant is:

```text
The model may choose what should change.
The model may not be responsible for reproducing everything that must remain unchanged.
```

This is the boundary that current Anchor lacks. Structured output and retry remain useful safeguards, but they are not substitutes for moving mechanical State construction into the deterministic Anchor reducer.
