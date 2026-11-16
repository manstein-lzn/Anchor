# 本地源码整理与推送准备

2026-11-16。本轮把现有本地工作整理为可正常快进推送的提交，不重做产品架构，不恢复 Dreaming。

## 整理结果

- 保留本地订阅提交 `11b7e45`，合入远端 `f087ff8` 的详情、共享历史监听、调度、任务结果和子 Graph 管理修复；不改写两边历史。
- 既有代码按读取原语、Codex 执行、Runtime/Library、Facts/Hub/Eval/发行、渠道、Host、Web 和示例分批提交。Rust 验收对应的完整源码在 `8c92bed` 已齐备，后续只收口文档与本机原件。
- Dreaming 专用编排保持退役；保留普通执行、结构化事实、Hub 分析与独立质量工具，不建设替代控制器。
- 仅选入 21 份必要历史摘要/结果；未发布的原日志、旧实验现场和两份草稿原样归入本机 `.local/master-preparation-20261116/unpublished/`。旧整理长记录也保留原件，不把历史失败改成通过。
- Kernel 写入/中断创建恢复及 Eval 回退共八个路径单独保存在本地分支 `codex/held-kernel-creation-20261116`，不混入准备推送的 `master`；仍需独立确认必要性、授权范围及调用方中断验收。

## 本次实际验收

| 检查 | 结果与边界 |
| --- | --- |
| 文件租约与 Kernel 定向回归 | 10 个 target，81 通过，0 失败/忽略/显式排除；包括读取游标及远端共享监听规模用例 |
| 完整 Rust workspace | all-features/all-targets，共 165 个 target；**2,752 通过、0 失败、158 默认忽略、0 显式排除** |
| 原生确定性场景 | 显式执行默认忽略集中 7 项，全部通过；6 项任务输入场景覆盖订阅、委派、结果、打断及自建 Graph 删除，另 1 项项目直连；不是其余 151 项通过 |
| 严格 Clippy / 格式 | workspace/all-features/all-targets `-D warnings` 及 workspace `fmt --check` 均通过 |
| Web 单元 / 构建 | 51 文件、407 项通过；TypeScript 与 Vite 构建通过，保留大 chunk 告警 |
| Web mock 浏览器 | 详情加载 1、图目录 4、消息管理 4，共 9 项通过；不是实际 Host/渠道业务验收 |
| 历史 P7 fixture | 8 通过、2 因没有本机 Hub/数据 fixture 跳过；Node/Bash 语法、工程措辞及空白检查通过 |

原生验收直接运行本轮 workspace 生成的测试产物，没有在采证中切换构建特性；Host、relay、Sandbox helper 和 Gateway 二进制前后摘要相同。七份回执均为 `real_model_requests: 0`、`production_data_used: false`。Web 源码与锁文件前后摘要一致，临时依赖链接已移除；共享依赖的 `.vite-temp` 目录时间发生变化，不称缓存零触碰。

实际命令：

```sh
scripts/test.sh --workspace --all-features --all-targets --no-fail-fast --offline --locked -- --test-threads=2
cargo +stable clippy --manifest-path rust/Cargo.toml --workspace --all-features --all-targets --offline --locked -- -D warnings
cargo +stable fmt --manifest-path rust/Cargo.toml --all -- --check
npm --prefix apps/web test
npm --prefix apps/web run build
```

浏览器、原生直接产物命令及实际环境见本机 `.local/master-preparation-20261116/logs/`。复用既有 `rust/target`，限制四个构建 worker，关闭调试符号和 incremental；测试现场使用短 `/tmp` 路径，不另建巨大构建缓存。

## writer 启动修复的准确边界

空部署原先可在首次 Run Kernel 初始化前响应健康检查，后台 retention 再创建基础资源；此时强制退出可能留下半建目录。改为在既有、被等待的 Host 启动步骤完成原 owner 初始化，不加状态字段、Kernel 恢复 API 或目录容错。

新测试在修前稳定失败、修后通过；并检查半建资源仍报错且原字节保留。原 `writer_lease` 五项在本轮完整 workspace 中均通过。早前 `validator/effect` 已存在的失败日志仍保留：这次证明可达的启动缺陷及其修复，不证明早前 SIGKILL 的实际落点，也不承诺初始化完成前遭强制中断后的自动恢复。

## 未扩大的范围

本轮没有推送、部署、重启正式服务、修改定时计划/权限、调用真实模型或重跑业务 Graph。没有新增完整发行安装、全部原生忽略集、真实渠道或长期运行验收。源码准备就绪与现网版本更新是两件事。

完整原工作区快照、逐文件摘要、合并决策、负控/正控、失败编译、最终日志及原生回执保存在本机 `.local/master-preparation-20261116/`。这些私有产物不随克隆交付，不能把文件名当成公开可获取的证据；必要结论与验证边界由本文保留。
