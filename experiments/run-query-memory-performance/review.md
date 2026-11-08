# 独立正确性审查

基线：`6cbfdaee0303aea1d244d6c0a6be48b54ff12bcb`。2026-11-08 由独立 Agent 只读审查本次 Kernel、Runtime、Host 读取改动；未发现可确认的正确性回归。

- 借用入口复用原资源租约、删除收尾和历史缓存刷新，回调期间不持有缓存 mutex；当前回调只做纯恢复投影，没有存储重入。
- frontier 仍由原有 prepare 在临时记录上计算，预览不改变读取到的事实。
- 恢复保留 accepted、principal、stop、Run、digest、node、invocation 全部过滤。
- 通用 project_run 保留旧快照刷新；HTTP 详情内部投影复用当前视图。父子 Run、metadata、历史和外部文件不构成全局原子快照。
- 列表仍使用既有 ID 枚举与 read_run；缺失记录跳过，损坏记录向上传播错误。

审查限于静态代码，不替代运行验证。回调不得重入同一资源的约束有文档说明，当前调用方满足。Kernel 改动只涉及只读访问方式，没有新的持久字段、裁决或恢复算法。

主轨收尾删除了已无生产调用的 `run_updated` 包装器，相关测试改用既有 `read_run`/`view.updated()`；该小改动由主轨复核，最终源码重建、普通 Host 全量及围栏结果见 README，不冒充独立审查的二次全量结论。
