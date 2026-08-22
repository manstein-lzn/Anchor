#!/usr/bin/env node
// Drive an Anchor session (zenmux deepseek) to produce a statetune
// retrospective: cognition migration history + diagnosis toward final goals.

import { AnchorRuntime } from "../src/runtime.js";

const STATE = "/root/statetune/.anchor/state.json";
const OUTPUT = "/root/statetune/docs/RETROSPECTIVE_COGNITION_HISTORY_20260822.md";

const prompt = `你正在 /root/statetune 工程内工作。你的上下文中已经有一份权威 State(44 条 beliefs,覆盖 tgraph_alignment / core_unified / core_looperset / u1_raw 等工作流)。本任务是:对 statetune 做一次**完整的历史追溯与整理**,产出一份给人看的 md 报告。

## 产出文件(唯一允许写入的文件)

${OUTPUT}

## 数据来源(全部只读)

1. 主仓与所有 worktree 的 git 历史:
   git -C /root/statetune log --reverse --format="%ci %s"
   以及 .worktrees/*/ 下各自的 git log(注意各线的起止时间)
2. docs/ 下文档的创建时间线(ls -la --time-style=+%F 按日期聚类),重点读关键转折点的文档内容
3. projects/statetune_core/ 的 NOW.md / PROJECT_STATUS.md / HANDOFF.md 及其 git 历史
4. 你的上下文中的 State 认知快照(44 条 beliefs 是整理工作的起点,但不是终点——你要追溯的是它们**之前**的演化过程)

## 报告结构要求

1. **认知演化史**(主体):按时间阶段重构"我们曾经相信什么 → 什么证据推翻或推进了它 → 认知如何升级"。每个阶段讲清楚因果,例如:早期基线建立 → 协议纪律的确立(为什么)→ 各探索线的开启与关闭(每个关闭线:当初的假设是什么、什么结果证伪了它、留下了什么戒律)→ 统一 state/consumer 方向的形成 → 当前前线(u1_raw + L-CV)。
2. **结构性诊断**:从历史中提炼反复出现的模式性问题。例如:控制面分裂(NOW/manifest/HANDOFF 各说各话)、真相散落 worktree、手工维护状态不可持续、负结果没有被制度化记录导致的重复弯路——用史实支撑,不要空谈。
3. **最终目标的差距与方向**:statetune 的最终目标是 (a) TGraph paper Table 2 完整对齐(layout 四集合 + tile),(b) 统一 state+consumer 并在 TenSet/LOOPerSet 分别验证。基于认知史给出:距离目标还差哪几步、最大的风险是什么、建议的收敛顺序。

## 风格约束

- 给人看:重叙事、重因果链,禁止罗列代码片段和数据表格堆砌;
- 数字只在支撑论点时出现,且必须注明口径;
- 中文,3000-6000 字。

完成后,按协议在回复末尾输出 anchor-state-delta 申报块(learned 里写本次任务最重要的 1-3 条发现,belief_ops 可把值得长期保留的新认知加入 State)。`;

const { runtime } = await AnchorRuntime.create({
  statePath: STATE,
  cwd: "/root/statetune",
  codexHome: "/root/.anchor-openrouter",
  purpose: "work",
});

try {
  console.log("[driver] session ready, model:", runtime.session?.model?.id ?? "(see logs)");
  const outcome = await runtime.runTask(prompt, { maxSegments: 10 });
  console.log("[driver] outcome:", JSON.stringify(outcome));
  const m = runtime.metrics;
  console.log(`[driver] compiles=${m.compilations} avg_compile_ms=${m.compilations ? (m.total_compile_ms / m.compilations).toFixed(1) : "-"}`);
  const state = await runtime.state();
  console.log(`[driver] state revision=${state.revision} beliefs=${state.beliefs.length}`);
} finally {
  runtime.dispose();
}
