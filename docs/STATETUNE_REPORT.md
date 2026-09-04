# 2022-09-01—2026-09-04 学习型编译器代价模型、结构化编译器状态与 LLM/Agentic 邻接工作：StateTune 证据定位

## 1. 执行摘要

本报告按“功能而非标题”将材料分为三条线：A）learned compiler cost/performance model；B）structured compiler-state/program representation；C）独立的 LLM-based/agentic compiler optimization。纳入日期以论文首次公开版本为主，同时记录正式发表信息；同一论文的 arXiv 修订与正式版视为同一工作，不重复计数。

在受限但有主要全文支撑的语料中，A 线从 2022 年 TLP 的 schedule-primitive sequence cost model，发展到 CDMPP 的 compact AST、Pruner 的 draft-then-verify、Tiramisu 的自监督表示预训练、TCL 的 Mamba/continual distillation，以及 2026 年显式建模 action-conditioned latent compiler-state dynamics 的 compiler world model。[evidence:artifact-75f99d3b095ac874fec0c8b0c3a3c8e2] [evidence:artifact-1aedb0873efc36ec0723e1b14a3edfe7] [evidence:artifact-49313e19fc6fec640aacddfa07cb61b2] [evidence:artifact-e34c3b048a6e337f57ce1ca4a7a51b3e] [evidence:artifact-fe88c0b77e97d84f21fe90b34006f3b3] [evidence:artifact-712d7ae5e2914abdcc085e52ebeceb0b]

B 线并非单一路径：MLIR cost model 把 MLIR 当文本序列；TLP 把变换 primitive 当规则序列；PERFOGRAPH 显式保留图关系、数值与 aggregate type；CDMPP 将 AST 规整为 compact AST；Tiramisu 预训练工作保留 polyhedral/AST/computation-vector 结构；compiler world model 则把初始 TensorIR、动作和中间状态组成轨迹。故“IR 输入”“结构化状态”和“动态状态转移”必须分开。[evidence:artifact-6bc6f317a298684b1525e4588156f712] [evidence:artifact-550ccf1322c14134a802cad41102ec2b] [evidence:artifact-712d7ae5e2914abdcc085e52ebeceb0b]

C 线从一次性生成 pass list，推进到 compiler-generated feedback、compiler-specialized foundation model、迭代源码重写，以及 RL 训练的 tool-using agent。它们与 A/B 有接口交叉，但 LLM 通常是候选生成器或策略，而非传统 latency cost model，应单列。[evidence:artifact-a9643286b943561123068bf5288b5784] [evidence:artifact-5c0b8cdccdf40affdd471aedd95a06cd] [evidence:artifact-53a7198ee559426bf93e84b5b289a5a1] [evidence:artifact-4bc5b1d0c8b4da91e0fc89f9e558e8ec] [evidence:artifact-232c5e25f7b3756c0de12d6e585ab7b1]

关键限制：当前 Research Store 没有可引用的本地 StateTune 定义 artifact。因此不能用外部论文替代 StateTune 自身定义，也不能作最终 closest-work 或绝对 novelty 判断；下文只给出条件式定位和需要由本地 claim ledger 触发的边界。

## 2. 范围与方法

检索窗口严格为 2022-09-01—2026-09-04（含端点）。证据优先级为：固定 arXiv 全文/正式论文全文，其次为论文明确链接的官方仓库固定提交。搜索元数据仅用于发现与日期核对，不单独承担方法或性能结论。本文没有运行代码、训练或 benchmark；性能数字均是论文报告值，不是复现结果。

纳入判据：A 线模型须预测 latency、speedup、hardware characteristic 或候选相对质量，并服务优化/搜索；B 线须明确说明状态/程序信息如何构造和编码；C 线须由 LLM 生成优化决策、变换或闭环动作。排除仅把“compiler”作为比喻、仅优化 LLM serving 而无编译决策、或仅有摘要而无已保存全文的候选。

## 3. 三轴 chronology

### A. Learned cost/performance models

- **2022-11（ASPLOS 2023）TLP/MTL-TLP**：直接编码 schedule primitive 的类型与参数，以 Transformer 做回归；集成 Ansor，并用 multi-task learning 处理跨硬件。论文报告相对 TenSet 的 CPU/GPU 搜索时间加速 9.1×/3.0×，MTL-TLP 只用 7% 目标硬件数据。[evidence:artifact-75f99d3b095ac874fec0c8b0c3a3c8e2]
- **2023-02 ML-driven Hardware Cost Model for MLIR**：把高层 MLIR token sequence 输入 Conv1D/MaxPool/FC 等模型，预测 register pressure 与 accelerator utilization；约 20K 训练样本、2K 测试样本，论文报告 RMSE 约 5–7%，但实现仍是 standalone，编译器内集成列为未来工作。[evidence:artifact-6bc6f317a298684b1525e4588156f712]
- **2023-11（EuroSys 2024）CDMPP**：以 compact AST、pre-order positional encoding、硬件特征和 domain-invariant regularization 预测绝对 latency；论文报告 cross-model/cross-device error 14.03%/10.85%。[evidence:artifact-1aedb0873efc36ec0723e1b14a3edfe7]
- **2024-02 初版、2025 ASPLOS：Pruner/MoA-Pruner**：symbolic analyzer 先 draft，小规模候选再由 pattern-aware learned cost model verify，并用 momentum online adaptation 跨平台；论文报告相对 Ansor 在线 tuning 时间 2.6×/4.82×、相对 TenSet/TLP 离线 4.75×/4.05×。[evidence:artifact-49313e19fc6fec640aacddfa07cb61b2]
- **2025-01 Data-efficient Performance Modeling via Pre-training**：用 autoencoder 无监督预训练 Tiramisu computation-vector embedding，再训练 AST-recursive speedup predictor；达到近似 MAPE 时，标注数据从 18M 降至 3.6M（约 5×）。[evidence:artifact-e34c3b048a6e337f57ce1ca4a7a51b3e]
- **2026-04 TCL**：RDU active sampler、Mamba-based sequence cost model、continual knowledge distillation；论文报告只取 10% 数据，并相对 Tenset-MLP 获得 CPU/GPU tuning-time 16.8×/12.48× 与 inference-latency 1.20×/1.13× 改进。[evidence:artifact-fe88c0b77e97d84f21fe90b34006f3b3]
- **2026-06 Compiler World Models**：由 TensorIR encoder、action-conditioned transition model 和 ranking cost model组成；输出相对候选分数而非直接 runtime，论文报告同为 64-trial 时相对 Ansor 的 GPU/CPU representative-subgraph latency 改进 1.37×/1.54×。[evidence:artifact-712d7ae5e2914abdcc085e52ebeceb0b]

### B. Structured compiler-state representations

- **2022 TLP**：状态近似为 schedule primitive 的可逆规则序列；结构是时序 action sequence，而不是 IR graph。[evidence:artifact-75f99d3b095ac874fec0c8b0c3a3c8e2]
- **2023 MLIR model**：有 op-only 与 op+operand 两种 tokenization；后者保留更多 use/operand 信息但仍是线性序列，应归为 serialized IR，不宜称显式图结构。[evidence:artifact-6bc6f317a298684b1525e4588156f712]
- **2023 PERFOGRAPH**：在 ProGraML 图上统一 local identifier 节点、补 store 边、保存常量数值，并以节点链表示多维 aggregate types；这是显式 attributed graph representation。[evidence:artifact-550ccf1322c14134a802cad41102ec2b]
- **2023/2024 CDMPP**：保留 AST leaf/computation vectors，把 loop nesting、range 与 leaf 位置编码为更规则的 compact AST 和 pre-order positional encoding。[evidence:artifact-1aedb0873efc36ec0723e1b14a3edfe7]
- **2025 Tiramisu pre-training**：computation vector 包含 iteration-domain matrix、access matrices、operation vectors、schedule matrices 和 transformation features，再按 AST 递归聚合。[evidence:artifact-e34c3b048a6e337f57ce1ca4a7a51b3e]
- **2026 world model**：显式构造 `(s0,a1,s1,...,aK,sK)` 轨迹；初始 TensorIR 编码为 latent state，动作逐步预测 terminal-state embedding，直接针对静态 snapshot 忽略 trajectory 的问题。[evidence:artifact-712d7ae5e2914abdcc085e52ebeceb0b]

### C. LLM/agentic 邻接趋势

- **2023-09 Large Language Models for Compiler Optimization**：7B 模型从未优化 LLVM IR 一次性生成 pass list；部署时把 pass list 交给 LLVM，论文在 100K 函数上报告相对 `-Oz` 指令数改善 3.01%，而训练标签的 autotuner 使用巨大编译预算。[evidence:artifact-a9643286b943561123068bf5288b5784]
- **2024-03 Compiler-generated Feedback**：编译器检查 pass、instruction count、生成 IR 与编译 IR 的一致性并反馈给模型；单次二阶段反馈增加 0.53%，但论文发现 10 个以上样本时简单 sampling 更强。[evidence:artifact-5c0b8cdccdf40affdd471aedd95a06cd]
- **2024-06 Meta LLM Compiler**：在 Code Llama 上追加 546B compiler-centric tokens，并进一步针对 flag tuning/disassembly 微调；pass list 仍由真实 compiler 执行，论文还引入 PassListEval 做 pass-list 语义筛查。[evidence:artifact-53a7198ee559426bf93e84b5b289a5a1]
- **2025-05 Compiler-R1**：SFT 学习 thought–tool–feedback 协议，随后以 outcome reward 做 RL；输入采用 56 维 AutoPhase features，工具包括 `instrcount` 与 pass-sequence search。论文报告七套数据平均 OverOz 8.46%。[evidence:artifact-232c5e25f7b3756c0de12d6e585ab7b1]
- **2025-06 CompilerGPT**：编译报告、源码、会话历史、编译/测试/计时反馈组成迭代闭环；五个程序、Clang/GCC、GPT-4o/Sonnet 的结果高度不一致，最大 6.5× 不能视为稳定平均收益。[evidence:artifact-4bc5b1d0c8b4da91e0fc89f9e558e8ec]

## 4. Taxonomy

1. **预测对象**：绝对 latency（CDMPP）、relative/ranking score（TLP、world model）、speedup（Tiramisu）、hardware characteristic（MLIR model）。
2. **表示形态**：serialized IR；schedule/action sequence；explicit AST/polyhedral state；attributed program graph；action-conditioned latent state trajectory。
3. **训练口径**：在线 measurement；大型离线 supervised dataset；跨硬件 multi-task/domain adaptation；self-supervised pre-training；active sampling；continual distillation。
4. **闭环角色**：cost model 只排序；cost model 与 search/measurement 交替；LLM 一次性 proposer；compiler-feedback refiner；tool-using RL agent。
5. **Agentic 判据**：至少具备可观察环境反馈、连续决策/修正及可执行 action；仅一次输出 pass list 的 2023/2024 Meta 模型属于 LLM optimizer，但不是强 agentic 闭环。[evidence:artifact-a9643286b943561123068bf5288b5784] [evidence:artifact-4bc5b1d0c8b4da91e0fc89f9e558e8ec] [evidence:artifact-232c5e25f7b3756c0de12d6e585ab7b1]

## 5. 比较矩阵

| 工作 | 主线/任务 | 状态表示 | 模型与决策接口 | 评测要点 | 代码证据状态 |
|---|---|---|---|---|---|
| TLP | A+B；tensor tuning | schedule primitive sequence | Transformer regression → top-k measurement | Ansor/TenSet，CPU/GPU | 论文给官方 URL；本 Store 未固定 commit [evidence:artifact-75f99d3b095ac874fec0c8b0c3a3c8e2] |
| MLIR cost model | A+B；硬件特征预测 | MLIR token sequence | Conv1D/FC → 预测 register/utilization | Intel in-house compiler/accelerator，WIP | 未取得固定官方仓库 [evidence:artifact-6bc6f317a298684b1525e4588156f712] |
| PERFOGRAPH | B；优化分类/分析 | numeric-aware attributed graph | RGCN → 分类 | device mapping、parallelism、NUMA | 本 Store 未固定官方仓库 [evidence:artifact-550ccf1322c14134a802cad41102ec2b] |
| CDMPP | A+B；绝对 latency | compact AST + hardware features | Transformer encoder/regressor | cross-model/device | 官方仓库已定位到 commit `6480e7a...` [evidence:artifact-1aedb0873efc36ec0723e1b14a3edfe7] [evidence:artifact-225b1a435ebf06ecfb215c2eac194311] |
| Pruner | A+B；快速 tuning | temporal dataflow + hardware symbols | symbolic draft → learned verify | 三种 GPU；在线/离线 | 官方仓库已定位到 `0760c3f...` [evidence:artifact-49313e19fc6fec640aacddfa07cb61b2] [evidence:artifact-9c1203bf2f09baef5593be2132384285] |
| Pre-trained Tiramisu | A+B；speedup predictor | polyhedral computation vectors + AST | autoencoder embedding → recursive model | 标注数据效率 | 论文给官方仓库，但本 Store 未固定 commit [evidence:artifact-e34c3b048a6e337f57ce1ca4a7a51b3e] |
| TCL | A+B；跨硬件 tuning | schedule sequence | Mamba + active sampling + distillation | CPU/GPU；不做 CPU↔GPU transfer | 数据仓库已定位到 `d4247e9...`，但系统代码未固定 [evidence:artifact-fe88c0b77e97d84f21fe90b34006f3b3] [evidence:artifact-7949c5dc23efd93f6294d46e18a27f6b] |
| Compiler world model | A+B；candidate ranking | TensorIR/action/state trajectory | encoder + latent transition + ranker | 同预算与 Ansor 比较 | 本 Store 未取得固定仓库 [evidence:artifact-712d7ae5e2914abdcc085e52ebeceb0b] |
| LLM pass optimizer | C；LLVM pass ordering | normalized LLVM IR text | LLM → pass list → LLVM | instruction-count proxy | 本 Store 未固定官方代码 [evidence:artifact-a9643286b943561123068bf5288b5784] |
| CompilerGPT | C；源码重写 | code+report+history | LLM rewrite ↔ compile/test/time | 5 programs，结果异质 | LLNL 仓库定位到 `ab3f06e...` [evidence:artifact-4bc5b1d0c8b4da91e0fc89f9e558e8ec] [evidence:artifact-642e5dca0df6cac7137dba66d1e3e59f] |
| Compiler-R1 | C；agentic pass tuning | 56-D static features + tool feedback | SFT+RL agent → tools | 7 suites、LLVM 10、125 actions | 论文给 URL；commit 获取失败，不作代码完整性结论 [evidence:artifact-232c5e25f7b3756c0de12d6e585ab7b1] |

## 6. StateTune claim definitions 与边界

**Claim ledger 状态**：当前 Store 中没有本地 StateTune 文件的 immutable artifact，故 StateTune 的任务、状态 schema、cost-model 接口、更新频率、搜索/agent 闭环及实验 claim 均为 evidence gap。为遵守“仅以本地文件定义 StateTune”，本报告不把同名外部术语、Anchor 文档或上述论文反向定义成 StateTune。

可审计的边界问题应是：StateTune 状态是否只是 IR serialization；是否包含当前 IR、动作历史和可恢复 provenance；cost model 输出绝对值还是 rank；状态在每个 pass 后还是每次搜索/会话边界更新；执行反馈来自 compiler、真实运行还是代理指标；以及状态表示、模型和搜索预算能否分别消融。

## 7. Closest-work 分析（条件式）

- 若 StateTune 的核心是**schedule/action history 作为 cost-model 输入**，TLP 是早期强重叠，2026 compiler world model 是更直接的动态重叠：后者显式学习 `(state, action, next-state)` latent dynamics，而非仅将动作序列静态编码。[evidence:artifact-75f99d3b095ac874fec0c8b0c3a3c8e2] [evidence:artifact-712d7ae5e2914abdcc085e52ebeceb0b]
- 若核心是**结构化当前程序状态**，CDMPP 的 compact AST、Tiramisu 的 polyhedral AST/computation vectors、PERFOGRAPH 的 attributed graph 是最近邻；三者分别强调规则化 AST、变换/访问矩阵和显式控制/数据/数值关系。[evidence:artifact-1aedb0873efc36ec0723e1b14a3edfe7] [evidence:artifact-e34c3b048a6e337f57ce1ca4a7a51b3e] [evidence:artifact-550ccf1322c14134a802cad41102ec2b]
- 若核心是**agent 与 compiler feedback 的闭环**，Compiler-R1 在 pass-tool interaction 上最接近；CompilerGPT 在会话历史、编译、测试和运行反馈上最接近，但其 action 是源码重写而非 learned cost prediction。[evidence:artifact-232c5e25f7b3756c0de12d6e585ab7b1] [evidence:artifact-4bc5b1d0c8b4da91e0fc89f9e558e8ec]

这些是按单维度得到的 conditional nearest neighbors，不是对完整 StateTune 的绝对 closest-work 断言。

## 8. Novelty boundary

### 已有先例支持

以下不能作为 StateTune 的基础 novelty：用 learned model 预测 compiler/tensor performance；用 schedule primitive sequence 表示变换；用 AST/polyhedral vectors/attributed graph 表示程序；跨硬件迁移或蒸馏；利用 compiler feedback 迭代优化；用 RL 训练 tool-using pass agent；以及用 action-conditioned latent dynamics 表示 compiler-state trajectory。[evidence:artifact-75f99d3b095ac874fec0c8b0c3a3c8e2] [evidence:artifact-550ccf1322c14134a802cad41102ec2b] [evidence:artifact-232c5e25f7b3756c0de12d6e585ab7b1] [evidence:artifact-712d7ae5e2914abdcc085e52ebeceb0b]

### 可能新颖但尚未支持

只有在本地定义与实验显示新的组合边界时，以下才可能成立：统一并可审计地维护多粒度 compiler state；把状态生命周期/provenance 与 cost-model/search 接口共同形式化；在严格预算匹配下证明 state update/forgetting/history selection 本身带来收益；或在跨 compiler/hardware 上保持同一状态协议。当前没有 StateTune artifact，均不得表述为已证实 novelty。

### 不支持的强断言

“首次 structured compiler state”“首次使用变换历史”“首次 learned cost model”“首次 agentic compiler optimizer”“唯一可跨硬件”及“SOTA”均不受本证据集支持。本文检索是 bounded discovery，未发现不能证明不存在。

## 9. 有效性威胁

- **构念**：instruction count、binary size、predicted latency 与真实 wall-clock performance 不等价；serialized IR 不等于显式 structured state；一次性 LLM proposer 不等于 agent。[evidence:artifact-a9643286b943561123068bf5288b5784]
- **内部**：表示、模型、采样、搜索器与预算常同时变化；Pruner 同时改变 draft mechanism、features、cost model 和 adaptation，TCL 同时改变 sampler、Mamba 与 distillation，因果贡献需消融。[evidence:artifact-49313e19fc6fec640aacddfa07cb61b2] [evidence:artifact-fe88c0b77e97d84f21fe90b34006f3b3]
- **外部**：硬件、compiler version、workload 分布受限；MLIR 工作使用 in-house compiler/accelerator，CompilerGPT 只有五个程序，TCL 明确不做 CPU 与 GPU 间迁移。[evidence:artifact-6bc6f317a298684b1525e4588156f712] [evidence:artifact-4bc5b1d0c8b4da91e0fc89f9e558e8ec] [evidence:artifact-fe88c0b77e97d84f21fe90b34006f3b3]
- **测量/统计**：最大 speedup 会掩盖平均值和失败率；CompilerGPT 表中多个配置无收益，LLM 2023 的训练 oracle 与测试推理预算差异极大。[evidence:artifact-4bc5b1d0c8b4da91e0fc89f9e558e8ec] [evidence:artifact-a9643286b943561123068bf5288b5784]
- **预算公平性**：Compiler-R1 的 SFT baseline 取 N=40 最优，而 interactive model 走单轨工具闭环；反馈论文也显示 sampling 数量本身会改变排序，必须按 compiler calls、measurements、tokens、时间与费用匹配。[evidence:artifact-232c5e25f7b3756c0de12d6e585ab7b1] [evidence:artifact-5c0b8cdccdf40affdd471aedd95a06cd]
- **泄漏**：预训练代码、autotuning labels、同源 IR 及 benchmark 派生函数可能重叠；需要按源项目/程序族而非随机函数切分。
- **复现**：部分论文仅给 URL，当前仅 CDMPP、Pruner、CompilerGPT 和 TCL 数据仓库获得固定 commit 元数据；固定 commit 证明可定位性，不证明论文全部实验可重现。[evidence:artifact-225b1a435ebf06ecfb215c2eac194311] [evidence:artifact-9c1203bf2f09baef5593be2132384285] [evidence:artifact-642e5dca0df6cac7137dba66d1e3e59f] [evidence:artifact-7949c5dc23efd93f6294d46e18a27f6b]
- **时间截点**：2026 工作多为首次 arXiv 版本，尚不能等同于完成同行评审或稳定代码发布。

## 10. 实验含义（仅设计，不执行）

1. **状态消融**：当前 IR-only、action-only、IR+flat history、显式 state-action trajectory、StateTune 完整状态；保持模型参数、训练样本和 search budget 相同。
2. **结构消融**：同信息量下比较 token serialization、compact AST、attributed graph、polyhedral vectors；另做边/位置/数值/历史打乱测试，判断收益来自结构还是信息量。
3. **模型消融**：固定状态，比较 XGBoost/MLP、Transformer、Mamba、latent transition ranker；固定模型，再替换状态。
4. **闭环与预算**：one-shot、sampling、compiler-feedback iteration、tool-agent、传统 autotuner；统一 compiler invocations、真实 measurements、token/API cost 和 wall time。反馈论文表明 iteration 不必然胜过 sampling，因此这是必要对照。[evidence:artifact-5c0b8cdccdf40affdd471aedd95a06cd]
5. **泛化**：按项目级去重，测试 unseen program family、input size、compiler version、CPU/GPU/accelerator；分别报告 within-device、few-shot transfer 与 zero-shot transfer。
6. **测量**：同时报告预测误差、排序质量、search regret、最终 runtime、编译/推理开销、失败率和 per-program 分布；不得只报最大 speedup。
7. **可复现包**：固定 compiler、数据 hash、仓库 commit、随机种子、硬件/驱动、所有失败 trial 和预算；代码可用性与结果复现分开声明。

## 11. 证据缺口、失败路径与置信度

**缺口**：StateTune 本地定义未进入 Research Store；TLP、MLIR、PERFOGRAPH、Tiramisu pretrain、world model 和 LLM pass optimizer 的官方仓库未固定；多数 2026 方法尚缺固定实现证据；本报告不是穷尽综述。

**失败路径**：早期通用 Web search 请求失败；若干 PDF page-range 解析请求无返回；Compiler-R1 commit API 获取失败。这些只记录为获取失败，未被当作“没有论文/代码”的证据。一个 GitHub 关键词搜索返回零结果也没有被用于否定 TLP 官方仓库存在，因为论文全文明确给出仓库 URL。[evidence:artifact-75f99d3b095ac874fec0c8b0c3a3c8e2]

**置信度**：对 TLP、CDMPP、PERFOGRAPH、Pruner、Tiramisu pretraining、TCL、world model 及五项 LLM/agentic 工作的方法分类为**中高**；对论文自报结果为**中等**（未复现）；对代码可复现性为**低至中**；对 StateTune closest work 与 novelty 为**低/不可判定**，直至本地 claim ledger 被不可变保存。

## 12. 结论

证据表明，在本时间窗内，“learned cost model + structured representation + search/feedback loop”的各组成部分均已有多种先例；尤其 2026 compiler world model 已把中间 compiler state、动作轨迹和 ranking cost model显式结合。[evidence:artifact-712d7ae5e2914abdcc085e52ebeceb0b] 因此 StateTune 若要形成可辩护贡献，不能只主张“用了结构化状态”“保留 transformation history”或“接入 learned cost model/agent”。其可能边界必须落在尚未由本地证据定义的具体状态 schema、生命周期、provenance、接口组合及经预算匹配消融证明的增量效果上。
