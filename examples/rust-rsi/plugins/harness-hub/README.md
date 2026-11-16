# Harness Hub Plugin

普通 Graph 查询 Hub 的只读 stdio MCP 示例，只封装现有 `anchor-hub`，不提供另一套调查流程、gate 或调度。复杂 [Dreaming](../../../../docs/dreaming.md) 专用系统已从源码退役，不再提供内置调查或自动改进入口，也不表示旧部署已经停用。清单的 `bin/anchor-hub` 是构建产物，安装前必须补齐；仓库不提交二进制。

在仓库根构建，然后将清单与二进制组成候选目录：

```sh
cargo build --manifest-path rust/Cargo.toml -p anchor-hub --release --locked
mkdir -p /tmp/anchor-harness-hub-plugin/bin
cp examples/rust-rsi/plugins/harness-hub/plugin.json /tmp/anchor-harness-hub-plugin/plugin.json
install -m 755 rust/target/release/anchor-hub /tmp/anchor-harness-hub-plugin/bin/anchor-hub
```

通过既有 Plugin 安装入口将该目录安装为 `harness-hub`。Host 的命令授权须含 `anchor-hub`；不要把 `/local-inputs` 文件直接当作进程入口，沙箱要求入口是系统命令或已挂载的 Plugin/tool 路径。

普通 MCP 需要操作员为实际 Graph 的调用节点配置 `local-inputs.json`，显式挂载 `harness-hub-data`；清单只引用沙箱内的 `/local-inputs/harness-hub-data`。例如名为 `inspect` 的节点：

```json
{
  "inspect": {"harness-hub-data": "/srv/anchor-history"}
}
```

这会将整个获准目录只读挂载给节点，shell 可直接读取，MCP 查询也不按信号隔离；`reads` 不是访问控制。原 Dreaming 的宿主安全窗口生成接入已退役；Hub 保留格式与消费能力不等于自动生成安全窗口，本 Plugin 也不提供替代导出或更窄的读取隔离，无法接受全目录读取范围时不应挂载该目录。旧信号 Graph 的 Python gate、源码输入授权与试跑脚本只保留为 [P7 历史实验](../../../../experiments/harness-hub/p7/README.md)，不再作为本 Plugin 的产品流程。
