#!/usr/bin/env node
// Apply the expert review feedback (gpt-5.6-sol codex thread, 2026-08-21) to the
// bootstrapped statetune State as provenance-carrying belief ops + state update.
//
// Usage: node scripts/revise-statetune-from-review.mjs [statePath]

import { StateStore } from "../src/state.js";

const statePath = process.argv[2] ?? "/root/statetune/.anchor/state.json";
const store = new StateStore(statePath);
const revision = (await store.read()).revision;

// ---- A: additions + stale-belief replacement (with provenance) ----
await store.applyBeliefOps([
  // A1: local milestone already reached
  { op: "add", belief: {
    id: "tg:xla-random-milestone", kind: "finding", scope: "workstream:tgraph_alignment", confidence: "high",
    text: "layout:xla:random 已达成 local method milestone：本地 source-style 重建协议、folds 6-9、best checkpoint、3 seeds rank-average ensemble（无论文式 10x permutation TTA）下 Tau=0.704166。仅单 collection 局部里程碑。",
    source: "docs/TGRAPH_LAYOUT_XLA_RANDOM_STAGE_H_ENSEMBLE_ACCEPTANCE_20260617.md",
  } },
  { op: "add", belief: {
    id: "tg:alignment-not-yet", kind: "finding", scope: "workstream:tgraph_alignment", confidence: "high",
    text: "截至当前仅 layout:xla:random 局部方法里程碑成立；尚无四个 layout collections 全对齐或 tile:xla 对齐证据。layout paper alignment 与 full TGraph paper alignment 均未达成。",
    source: "AGENTS.md",
  } },
  { op: "add", belief: {
    id: "tg:stage-h-tta-limit", kind: "constraint", scope: "workstream:tgraph_alignment", confidence: "high",
    text: "Stage H 使用 cross_config_layers=0（每 config 独立打分），config-permutation TTA 是 no-op；0.704166 是三 seed ensemble 结果，不得表述为论文 10x permutation TTA。",
    source: "docs/TGRAPH_METHOD_THEORY_COMPARISON_20260612_ZH.md",
  } },
  // A2: blind folds finished; replaces the stale low-confidence belief
  { op: "add", belief: {
    id: "sh:blind-11-13-final", kind: "negative_result", scope: "workstream:tgraph_alignment", confidence: "high",
    text: "Blind folds 11/12/13 完成（seed=20260604，每折 6000 steps）：best Tau=0.60619/0.66791/0.30291，best mean=0.52567；final Tau=0.50799/0.66791/0.30285，final mean=0.49292；fold11 best-final drift=-0.09820。Stage H 在未参与决策折上的稳定性被否定。",
    source: "/root/autotune/analysis/tgraph_source_style_stage_c_repair_20260601/runs/ (graph_state_h192_stage_h_tau_only_rank_stratified_amp_blind_seed20260604_fold{11,12,13}_s6000/summary.json)",
  } },
  { op: "supersede", id: "sh:blind-11-13-shape", by: "sh:blind-11-13-final" },
  { op: "amend", id: "tg:folds-6-9-not-global",
    set: { source: "handover.md + blind folds 11-13 summary.json（实证确认）" } },
  // A3/A4/A6/A7: core unified line
  { op: "add", belief: {
    id: "cu:final-goal", kind: "doctrine", scope: "workstream:core_unified", confidence: "high",
    text: "Core 最终形态是两套私有 dataset adapter + 一套 canonical StatePlan + 一张 dataset-neutral consumer 图；TenSet 与 LOOPerSet 使用各自私有训练协议和分别训练的权重。统一架构不等于共享权重。",
    source: "projects/statetune_core/docs/UNIFIED_STATE_CONSUMER_FINAL_GOAL_V1.md",
  } },
  { op: "add", belief: {
    id: "cu:control-plane-desync", kind: "finding", scope: "workstream:core_unified", confidence: "high",
    text: "控制面不一致：AGENTS/HANDOFF 已路由到 L-CV 与统一 StatePlan(v2)，但 ACTIVE_WORK_MANIFEST/NOW/PROJECT_STATUS 仍保留旧 T.5/state_id 与已关闭的 model-forward 权限描述。在控制面重新同步前，T.5 或任何训练动作都不是当前合法 next_action。",
    source: "projects/statetune_core/{AGENTS.md,ACTIVE_WORK_MANIFEST.json,HANDOFF.md}",
  } },
  { op: "supersede", id: "ct:canonical-position", by: "cu:control-plane-desync" },
  { op: "supersede", id: "ct:t5-authority", by: "cu:control-plane-desync" },
  { op: "add", belief: {
    id: "cu:hard-share-negative", kind: "negative_result", scope: "workstream:core_unified", confidence: "high",
    text: "J1 跨数据集 hard-share 联合训练为负结果；后续只验证同一 state/consumer 架构在各数据集分别训练成立，共享权重不再是默认路线。",
    source: "projects/statetune_core/docs/looperset_crossval/METHOD_GOAL_UNIFIED_STATE_CONSUMER.md",
  } },
  { op: "add", belief: {
    id: "cu:claim-boundary", kind: "protocol", scope: "workstream:core_unified", confidence: "high",
    text: "声明边界：implemented ≠ transported ≠ consumed ≠ optimized ≠ frozen-inference-used ≠ causally validated。同名类或共享源码不能单独证明 C2/C3；负向与 null control 必须保留。",
    source: "projects/statetune_core/contracts/STATETUNE_PAPER_CLAIM_LADDER.md",
  } },
  // A5: LOOPerSet workstream was entirely missing
  { op: "add", belief: {
    id: "cl:pact25-status", kind: "finding", scope: "workstream:core_looperset", confidence: "high",
    text: "官方 pact25 compact 已核对，标签聚合为 min(execution_times)。unified small+protect_top10 在截断 651,561-row val 上 Spearman=0.9196 / MAPE=0.2881。注意口径：29% 是 pact25 5.67M 面的数据效率里程碑，不是 LOOPer 论文 28M 数据复现；0.9293/0.2765 是用户自训 v8，不是官方发布模型。",
    source: "projects/statetune_core/HANDOFF.md",
  } },
  // A8 handled above (tg:stage-h-tta-limit). A9: low-loss mapping doctrine
  { op: "add", belief: {
    id: "gl:low-loss-mapping", kind: "doctrine", scope: "global", confidence: "high",
    text: "低损映射戒律：模型不能恢复 tensor contract 已擦除的语义。训练前必须先审计字段覆盖、collision/truncation、typed value、binding、model-consumer coverage 与 evaluator parity；失败后按 counterexample refinement 修 IR，而不是继续盲调模型。",
    source: "docs/STATETUNE_LOW_LOSS_MAPPING_DOCTRINE_20260524.md",
  } },

  // ---- B: corrections to existing beliefs ----
  { op: "amend", id: "ct:a-protocol-final", set: {
    confidence: "high",
    text: "TenSet 历史冠军基线（A-protocol：OrderScale + warmup/cosine LR + val-selected checkpoint + test-once，official 27-holdout）：pw 0.8776 / P@1 0.8903 / P@5 0.9312。统一线的 TenSet 验收是在同协议下超过此基线；官方 Table-3 M#3（>=0.89/>=0.88/>=0.96）是比较目标，不是本架构验收线。",
  } },
  { op: "amend", id: "ct:table3-is-model3", set: {
    kind: "protocol",
    text: "与官方 Table 3 比较时必须对准 Model #3 RANKING 行（official SegmentSumMLP + probabilistic LambdaRank；官方牺牲回归 R2=-1818）。Table 3 是三个独立模型，比错行会制造假差距。仅适用于官方比较场景，不等同 Core 统一线的总目标。",
  } },
  { op: "amend", id: "ct:d-ensemble-closed", set: {
    text: "仅 schedule shared-head + rankhead_v1 的 blend/cross-tower 分支关闭（2026-08-10）：无 blend 达到 M#3 triple 且均低于 schedule 基线自身数字。不据此禁止未来不同 consumer/checkpoint 组合的 ensemble。",
  } },
  { op: "amend", id: "ct:levers-exhausted", set: {
    kind: "finding", scope: "workstream:core_unified",
    text: "clean-27 上已测试且未改善 pw 的手段：head/objective 变体（rankhead v1/v2、dual-tower）、B0 numeric channel（噪声内持平）。",
  } },
  { op: "add", belief: {
    id: "ct:margin-falsified", kind: "negative_result", scope: "workstream:core_unified", confidence: "high",
    text: "'margin 丢 pair 导致 pw 低' 被证伪：pairwise coverage 98.8% dense（|delta|>0.02）。margin 影响 P@1 但不影响 pw。",
    source: "projects/statetune_core/NOW.md",
  } },
  { op: "add", belief: {
    id: "ct:official-granularity-hypothesis", kind: "finding", scope: "workstream:core_unified", confidence: "medium",
    text: "假设（因果未隔离）：official recipe 在当前 dense stat features 上仅 pw 0.62-0.65，低于自研 rank head；官方 0.89 可能依赖其 per-store 164D 粒度。此归因尚未经隔离实验验证。",
    source: "projects/statetune_core/NOW.md",
  } },
  { op: "amend", id: "ct:dual-tower-invalidated", set: {
    text: "历史 invalidated claim：dual-tower '> Table-3 (0.892)' donor 声明因比较尺度不匹配而无效。仅指该次 donor 比较，不代表所有 dual-tower 架构无效。",
  } },
  { op: "amend", id: "gl:naming-debt", set: {
    kind: "finding",
    text: "实现仍主要位于 execution_state_costmodel 包，而公共项目名为 statetune（现状事实）。",
  } },
  { op: "add", belief: {
    id: "gl:naming-migration-policy", kind: "doctrine", scope: "global", confidence: "high",
    text: "迁移完成前保持 legacy 包稳定；新公共 API 放在 statetune 下，待有明确兼容计划再迁移实现。",
    source: "docs/KNOWN_ISSUES.md",
  } },
], { expectedRevision: revision });

// ---- C: structural fixes (goal split, acceptance, constraints, questions, action) ----
await store.update((state) => {
  state.task.goal =
    "两个顶层目标。(1) TGraph：完成 layout paper alignment（四个 layout collections 对齐 Table 2 同协议族）及 tile:xla 对齐；(2) Core：经两套私有 dataset adapter + canonical StatePlan 收敛到一张 dataset-neutral consumer 图，并在 TenSet 与 LOOPerSet 上分别按各自协议验证。";
  state.task.acceptance = [
    "TGraph: 四个 layout collections 达到 Table 2 目标（同 metric/协议族）",
    "TGraph: tile:xla 达到 M_tile = 0.9694 +/- 0.0021",
    "Core/TenSet: 同协议超过 TenSet 冠军基线 pw 0.8776 / P@1 0.8903 / P@5 0.9312",
    "Core/LOOPerSet: 待裁决 0.9196/0.2881 的接受性质后设定验收线",
  ];
  state.task.constraints = state.task.constraints
    .filter((item) => !item.includes("T.5"))
    .concat([
      "在控制面重新同步前，不得把 T.5 或任何训练动作当作当前合法 next_action。",
      "每个声称的数字必须声明口径：fold 数/聚合方式/seeds/checkpoint 选择/TTA/ensemble。",
    ]);
  state.open_questions = [
    "[core] 控制面重新同步：manifest/NOW/PROJECT_STATUS 与 AGENTS/HANDOFF 的路由不一致，需确认当前合法研究入口、权限边界并重写状态文档。",
    "[core_looperset] Looper 半身 0.9196/0.2881 已被接受并停止继续优化——这是阶段性接受还是最终门槛已被修改？需要用户裁决。",
  ];
  state.next_action =
    "[core] 完成控制面同步（重写 NOW/ACTIVE_WORK_MANIFEST 使其与 AGENTS/HANDOFF 一致），在此之前不执行任何训练或优化动作。";
  state.evidence.push(
    { path: "/root/statetune/docs/TGRAPH_LAYOUT_XLA_RANDOM_STAGE_H_ENSEMBLE_ACCEPTANCE_20260617.md", type: "experiment_result" },
    { path: "/root/statetune/projects/statetune_core/docs/UNIFIED_STATE_CONSUMER_FINAL_GOAL_V1.md", type: "design_doc" },
    { path: "/root/statetune/projects/statetune_core/HANDOFF.md", type: "canonical_status" },
  );
  return state;
}, { event: "state.expert_review_applied", data: { reviewer: "codex gpt-5.6-sol thread 01a0193f continuation" } });

const final = await store.read();
const stats = {
  total: final.beliefs.length,
  active: final.beliefs.filter((b) => b.status === "active").length,
  confirmed: final.beliefs.filter((b) => b.status === "confirmed").length,
  superseded: final.beliefs.filter((b) => b.status === "superseded").length,
};
console.log(`review applied at rev ${final.revision}; beliefs:`, JSON.stringify(stats));
