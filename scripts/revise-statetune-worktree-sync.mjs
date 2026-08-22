#!/usr/bin/env node
// Second sync: capture the live worktree frontline (.worktrees/*) that the
// main-repo entry points do not reflect. Freshest lines first:
//   statetune_u1_step_numeric_v6        (NOW.md updated 2026-08-19)
//   statetune_core_looperset_crossval_v1 (HANDOFF.md updated 2026-08-13)
//
// Usage: node scripts/revise-statetune-worktree-sync.mjs [statePath]

import { StateStore } from "../src/state.js";

const statePath = process.argv[2] ?? "/root/statetune/.anchor/state.json";
const store = new StateStore(statePath);
const revision = (await store.read()).revision;
const WT = "/root/statetune/.worktrees";

await store.applyBeliefOps([
  // ---- workstream:u1_raw — the actual current frontier ----
  { op: "add", belief: {
    id: "u1:current-frontier", kind: "finding", scope: "workstream:u1_raw", confidence: "high",
    text: "当前最活跃前线是 U1 raw adapter -> Cost-State -> V7 tied-readout Consumer 线（state_id u1_local_champion_raw_u1_comparison_v1_complete）。local champion 对比（同一 2083-train/27-test 面）：champion P@1 0.8204/P@5 0.8709/pw 0.8891；U1 control P@1 0.8252/P@5 0.9055/pw 0.7727；U1 candidate P@1 0.7956/P@5 0.8784/pw 0.8136。剩余差距在 State/Consumer 信号传递或目标对齐，不是数据量。",
    source: ".worktrees/statetune_u1_step_numeric_v6/NOW.md", as_of: "2026-08-19",
  } },
  { op: "add", belief: {
    id: "u1:data-scarcity-rejected", kind: "negative_result", scope: "workstream:u1_raw", confidence: "high",
    text: "'数据太少'作为 U1 差距主因已被冻结诊断否定：V1.4 直接 runner（64 train + 12 dev，3040 paired candidates，7680 steps，零失败）三 seed 的 Peak@5 delta 全为负，决策 rejects_data_scarcity_as_primary_cause。",
    source: ".worktrees/statetune_u1_step_numeric_v6/docs/u1_work_packages/U1_V7_TIED_READOUT_SCALE_DIAGNOSTIC_V1_4/RESULT.md",
  } },
  { op: "add", belief: {
    id: "u1:next-is-preregistered-hypothesis", kind: "constraint", scope: "workstream:u1_raw", confidence: "high",
    text: "U1 下一步是新的预注册（preregistered）Peak@5 representation/objective 假设实验；不允许数据扩张、rescue 或重调参。",
    source: ".worktrees/statetune_u1_step_numeric_v6/NOW.md", as_of: "2026-08-19",
  } },
  { op: "add", belief: {
    id: "u1:champion-transplant-negative", kind: "negative_result", scope: "workstream:u1_raw", confidence: "high",
    text: "完整 local champion 流水线移植到 V1.4 raw membership 上反而全面落后当前 U1（candidate 领先 champion -0.129/-0.123/-0.126 于 P@1/P@5/pw）。冠军系统整体搬迁不是路线。",
    source: ".worktrees/statetune_u1_step_numeric_v6/docs/u1_work_packages/U1_LOCAL_CHAMPION_SCALE_COMPARISON_V1/RESULT.md",
  } },

  // ---- L-CV established facts (勿再争论级) enrich looperset/unified ----
  { op: "add", belief: {
    id: "cl:established-facts", kind: "protocol", scope: "workstream:core_looperset", confidence: "high",
    text: "L-CV 已查证事实（2026-08-13，勿再争论）：本地 looperset_v2_pact_*_compact.jsonl.gz 与 HF Mascinissa/LOOPerSet pact25 同名文件字节一致（train 5,668,703 / val 658,436 行）；标签聚合 min(execution_times) 经 30 条抽样全对验证（median 全错）；所谓'官方 base 0.93/0.31'实为用户自训 p6_official_v8_candidate_24_01，官方从不发布权重；LOOPer 论文 29% 出自其自产 28M 更大数据，与 pact25 不同口径——pact25 小面上达 29% 应表述为'数据效率高于官方'而非'复现论文'。",
    source: ".worktrees/statetune_core_looperset_crossval_v1/HANDOFF.md", as_of: "2026-08-13",
  } },
  { op: "add", belief: {
    id: "cl:training-recipe-facts", kind: "finding", scope: "workstream:core_unified", confidence: "high",
    text: "L-CV 全量面训练配方事实：target=log_speedup、log-Huber(0.5)、softplus-margin rank 辅助 w=0.05、plateau patience2、grad-clip 1.0、不删尾；lr 用 3e-4（官方 5e-5 在我们冷启动不 work，实测）；官方 hidden256+aux+warm-start 是 40M 级容量，我们没有——决策是调 lr/ckpt-select 适配而不改 loss/schedule 结构。",
    source: ".worktrees/statetune_core_looperset_crossval_v1/HANDOFF.md",
  } },
  { op: "add", belief: {
    id: "cu:l-cv-milestone", kind: "finding", scope: "workstream:core_unified", confidence: "high",
    text: "MILESTONE：unified consumer 在 pact25 5.67M small face（same-data）上 MAPE 跨过 29%（calib 28.8-29.0%）；full 658,436 行 base 34.7%→calib 28.5-29.0%。对比用户 v8 自训基线 27.65%（20-40M 参数）差 1.16pp。LOOPer 论文需 28M 数据才到 29% => 统一线展示约 5x 数据效率。",
    source: ".worktrees/statetune_core_looperset_crossval_v1 git log 0f23a31", as_of: "2026-08-13",
  } },

  // ---- meta-cognition about the repo itself ----
  { op: "amend", id: "cu:control-plane-desync", set: {
    text: "控制面不一致比 manifest 层更严重：研究前线散落在 .worktrees/* 各自的 NOW/HANDOFF 中（u1_step_numeric_v6 至 2026-08-19 仍在推进），主仓 AGENTS/HANDOFF 路由到 L-CV/StatePlan(v2)，而 ACTIVE_WORK_MANIFEST/NOW/PROJECT_STATUS 还停在旧 T.5/state_id。任何单一入口都不完整；在控制面重新同步前，不得把任何单点状态当作全局权威。",
  } },
  { op: "add", belief: {
    id: "gl:worktree-topology", kind: "protocol", scope: "global", confidence: "high",
    text: "statetune 的工作真相分布：活跃线在 .worktrees/statetune_u1_step_numeric_v6（u1 raw，最新）与 .worktrees/statetune_core_looperset_crossval_v1（L-CV）；已关闭探索线：statetune_core_btw、gradient_stable_v1、absolute_logratio_v1、semantic_v1、semantics_v4（各自有收尾 commit）；universal_value_tokenizer 为封存 legacy。接手任何工作前先核对对应 worktree 的 NOW/HANDOFF 与其 git log 时间戳。",
    source: ".worktrees/*/ git log -1 --format=%ci",
  } },
], { expectedRevision: revision });

await store.update((state) => {
  state.open_questions = [
    "[meta] 控制面重新同步：把 .worktrees 前线(u1_raw + L-CV)状态回写主仓 NOW/ACTIVE_WORK_MANIFEST/PROJECT_STATUS,消除 T.5 时代残留;同步前不执行任何训练动作。",
    "[core_looperset] Looper 半身 0.9196/0.2881 已被接受并停止继续优化——阶段性接受还是最终门槛已修改?需要用户裁决。",
    "[u1_raw] 预注册 Peak@5 representation/objective 假设的具体内容待定义(由用户/下一 session 决定)。",
  ];
  state.next_action =
    "[meta] 完成控制面同步:以本 State 为唯一权威快照,回写主仓状态文档;之后 [u1_raw] 定义并预注册 Peak@5 表征/目标假设实验。";
  return state;
}, { event: "state.worktree_sync_applied", data: { source: ".worktrees frontline NOW/HANDOFF docs" } });

const final = await store.read();
console.log(`worktree sync applied at rev ${final.revision}; active beliefs:`,
  final.beliefs.filter((b) => b.status === "active").length, "/", final.beliefs.length);
