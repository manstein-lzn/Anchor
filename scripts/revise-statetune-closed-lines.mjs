#!/usr/bin/env node
// Third sync: capture closed-worktree conclusions (falsified hypotheses, closed
// branches, binding end-state judgments) extracted by the gpt-5.6-sol review
// thread from the 5 closed exploration worktrees.
//
// Usage: node scripts/revise-statetune-closed-lines.mjs [statePath]

import { StateStore } from "../src/state.js";

const statePath = process.argv[2] ?? "/root/statetune/.anchor/state.json";
const store = new StateStore(statePath);
const revision = (await store.read()).revision;
const WT = "/root/statetune/.worktrees";

await store.applyBeliefOps([
  // ---- statetune_core_btw ----
  { op: "add", belief: {
    id: "ct:v12-campaign-closed", kind: "negative_result", scope: "workstream:core_tenset", confidence: "high",
    text: "TenSet v12 完整 campaign（3 Summary + 3 Direct + 1 Fusion）为负结果：固定最终评估 RMSE=0.1534/R2=0.1657/pw=0.7672/P@1=0.5647/P@5=0.6832，明显低于 Local Model #1（0.0827/0.7319/0.8474/0.8195/0.8847）。campaign 已关闭，禁止从 v12 指标继续重调或补训。",
    source: ".worktrees/statetune_core_btw/docs/TENSET_FULL_CAMPAIGN_V12_RESULT_AND_ROOT_CAUSE.md",
  } },
  { op: "add", belief: {
    id: "ct:v12-optimizer-protocol-lesson", kind: "doctrine", scope: "workstream:core_tenset", confidence: "high",
    text: "v12 失败发生在 Fusion 之前（Summary/Direct 均未恢复强模型），Fusion 不是主因。根因教训：相同 corpus exposure ≠ 相同随机优化协议——v12 固定 workload 顺序、每 epoch 10,865 次更新、约 650 candidates/step；成功的 legacy 路径约 7,034 次全局 reshuffle 更新、约 1,003 candidates/step。两者不可视为可替代。",
    source: ".worktrees/statetune_core_btw/docs/TENSET_FULL_CAMPAIGN_V12_RESULT_AND_ROOT_CAUSE.md",
  } },

  // ---- statetune_core_gradient_stable_v1 ----
  { op: "add", belief: {
    id: "cu:gradient-cancel-falsified", kind: "negative_result", scope: "workstream:core_unified", confidence: "high",
    text: "V4.15 证伪'跨 workload 梯度抵消是绝对能力失败主因'：每 workload 梯度平均余弦 0.98281、负余弦比例 0、cancellation ratio 0.99244。micro-update 提升 train Spearman（0.580→0.668）却降低 eval Spearman（0.385→0.268）；workload-scale-balanced loss eval Spearman 仅 0.365；冻结 donor 蒸馏 variation retention 0.286 未过门槛。",
    source: ".worktrees/statetune_core_gradient_stable_v1/docs/UNIVERSAL_COST_MODEL_V4_15_CAPABILITY_FUNNEL_RESULT.md",
  } },
  { op: "add", belief: {
    id: "cu:no-gradient-rescue", kind: "constraint", scope: "workstream:core_unified", confidence: "high",
    text: "不得再把 gradient balancing、micro-update 或 workload-scale balancing 当作绝对能力失败的单独 rescue，也不得据此开启 scope topology 或 TenSet campaign。现有证据只支持'Compile-State/Cost-State consumption 与跨 workload calibration 的联合问题'，不支持'Consumer expressivity 已被唯一证明不足'。",
    source: ".worktrees/statetune_core_gradient_stable_v1/PROJECT_STATUS.md",
  } },
  { op: "add", belief: {
    id: "ct:v5-input-contract-gate", kind: "constraint", scope: "workstream:core_tenset", confidence: "high",
    text: "V5 的 TenSet 停止点不是 native source 缺少程序状态（Lowered Full-State 中已有 33 个跨候选不变的 program owners）。真正未关闭的是 typed value/kind/symbol vocabulary 的 collision closure、inference-visible workload facts 与 train-only normalizer 的 source binding。这些输入契约完成前不得开始 metric training。",
    source: ".worktrees/statetune_core_gradient_stable_v1/docs/UNIVERSAL_GRADIENT_STABLE_V5_INPUT_PROJECTION_RESULT.md",
  } },

  // ---- statetune_core_absolute_logratio_v1 ----
  { op: "add", belief: {
    id: "cu:absolute-v2-v3-falsified", kind: "negative_result", scope: "workstream:core_unified", confidence: "high",
    text: "两个 shared absolute 补救假设均被证伪：V2 加 772 维 extensive/structural formation 后 TenSet ordering 有提升但 absolute R2 仍约为零，LOOPerSet R2 从 0.113 降至 -0.211；V3 仅 Huber→candidate MSE，TenSet R2 -0.023 / LOOPerSet R2 0.085。不得把 ordering gain 当成 absolute capability，不得继续加 epoch、调 loss、恢复 ranking authority 或追加 generic residual。",
    source: ".worktrees/statetune_core_absolute_logratio_v1/docs/UNIVERSAL_ABSOLUTE_V2_V3_RESULT.md",
  } },
  { op: "add", belief: {
    id: "cu:scope-contract-must-be-explicit", kind: "finding", scope: "workstream:core_unified", confidence: "high",
    text: "absolute graph 的 scope contract 无效：必须由 adapter 显式形成 program-static baseline + candidate-local schedule delta，不能在 Core 中按 binding incidence 推断。实证：LOOPerSet 48 workload 中出现 718 个 target 不同但完整 Universal graph 相同的候选对（其中 642 对可由被遗漏的 dense access state 区分）；TenSet 把 156,113 个 numeric atoms 全放入 inferred static stream、delta 中为 0，静态跨候选不变性 0/48。",
    source: ".worktrees/statetune_core_absolute_logratio_v1/docs/UNIVERSAL_ABSOLUTE_ROOT_CAUSE_DIAGNOSIS_V1.md",
  } },
  { op: "add", belief: {
    id: "cu:zero-training-gates-before-training", kind: "doctrine", scope: "workstream:core_unified", confidence: "high",
    text: "在 graph scope、candidate identity 和 program baseline 可观测性通过 zero-training positive-control gates 之前，禁止训练 baseline-plus-delta 架构；gradient balancing 只能在 graph identifiability 之后作为训练策略因素，不能用来 rescue V3 MSE。",
    source: ".worktrees/statetune_core_absolute_logratio_v1/docs/UNIVERSAL_ABSOLUTE_ROOT_CAUSE_DIAGNOSIS_V1.md",
  } },

  // ---- statetune_core_semantic_v1 ----
  { op: "add", belief: {
    id: "cu:v416-calibration-not-promoted", kind: "negative_result", scope: "workstream:core_unified", confidence: "high",
    text: "V4.16 direct deployed-score calibration 只部分恢复候选变化：teacher eval Spearman 0.169→0.228、variation retention 0.190→0.319，仍低于预设 0.50 floor；real-target Spearman 反降（0.217→0.214）。该 calibration branch 不晋升；scope topology、TenSet、protected roles、full training 保持关闭。",
    source: ".worktrees/statetune_core_semantic_v1/docs/UNIVERSAL_COST_MODEL_V4_16_FINAL_RESULT.md",
  } },
  { op: "add", belief: {
    id: "cu:deployment-form-doctrine", kind: "doctrine", scope: "workstream:core_unified", confidence: "high",
    text: "保留的部署形态只能是 Cost-State -> candidate-local raw absolute deployment score；centered/scale/log-std/ranking 只可作 training-only auxiliaries。输入预测的 multiplicative workload scale 不得控制 deployment；scalar-only normalization 与 another late residual 都不是有证据支持的路线。下一步方向：检查 owner/candidate relational geometry 或 adapter 是否丢失 donor-visible local state。",
    source: ".worktrees/statetune_core_semantic_v1/docs/UNIVERSAL_COST_MODEL_V4_16_FINAL_RESULT.md",
  } },

  // ---- statetune_core_semantics_v4 (low confidence: no status docs) ----
  { op: "add", belief: {
    id: "ct:serialization-parity-only", kind: "finding", scope: "workstream:core_tenset", confidence: "medium",
    text: "Generic FullStateProjection→structured-GRU 序列输入上，三种 numeric serialization mode 中 current（argument atoms + owner-coordinate numerics）达到 pw >= native。该 sweep 只证明 numeric serialization 本身不劣于 native，不能把编码差异单独列为已证伪瓶颈；且无完整多折/多 seed/paper-protocol artifact，不得升级为 alignment 证据。",
    source: ".worktrees/statetune_core_semantics_v4 commit bc773e7",
  } },

  // ---- conflict-check amendments ----
  { op: "amend", id: "ct:official-granularity-hypothesis", set: {
    text: "假设（因果未隔离）：official recipe 在当前 dense stat features 上仅 pw 0.62-0.65，低于自研 rank head；官方 0.89 可能依赖其 per-store 164D 粒度。注意：absolute graph scope、candidate representation 与 optimizer-protocol parity 是更早的必要门槛，不得把 per-store 164D 粒度当作唯一根因。",
  } },
  { op: "amend", id: "cl:training-recipe-facts", set: {
    text: "L-CV 全量面训练配方事实：target=log_speedup、log-Huber(0.5)、softplus-margin rank 辅助 w=0.05、plateau patience2、grad-clip 1.0、不删尾；lr 用 3e-4（官方 5e-5 冷启动不 work，实测）。另有 v12 教训约束：批次顺序、logical-batch grouping 与 exposure geometry 也是优化协议的一部分（v12 同 exposure 但固定顺序而失败），复现 legacy 结果时必须整体对齐 optimizer protocol 而非只对齐 lr/loss。",
  } },
], { expectedRevision: revision });

const final = await store.read();
console.log(`closed-lines sync applied at rev ${final.revision}; active beliefs:`,
  final.beliefs.filter((b) => b.status === "active").length, "/", final.beliefs.length);
