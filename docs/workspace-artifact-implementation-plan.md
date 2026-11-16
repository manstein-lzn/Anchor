# 工作区与交付重构：完成后的入口

本计划已退出活跃开发入口。当前实现统一采用 `(Run,node)` 稳定可写 Workspace、显式文件选集和内容去重的不可变 Artifact；不再按 invocation 复制完整现场，也不生成 Git Artifact 投影。

用户决定不保留旧运行数据。本次切换使用新数据根，旧 Artifact/Workspace 导入命令、旧格式读取器及其专用测试已经删除；现有本机旧数据未删除、未迁移。Git Plugin 继续作为显式授权的项目开发能力，与 Artifact 交付无关。

后续开发只依据 [当前架构](architecture.md)、[Graph 作者契约](rust-graph-authoring-contract.md)、[文件 API](rust-frontend-api-contract.md#graphrun-与-artifact) 和 [验收台账](pilot-development-plan.md)。模型会话连续性见 [对应契约与证据](rsi-agent-session-continuity.md)。
