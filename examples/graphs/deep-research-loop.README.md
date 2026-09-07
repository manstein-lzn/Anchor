# deep-research-loop.bundle.json

Anchor 第一个官方 Graph Bundle：`plan → research → review → check(loop) → report`。

- `review` 拿双份输入：`research` 的 work（含 `addressed` 回应链）+ `plan` 直连的
  questions——循环收敛靠信息流完备，不是靠多跑。
- `check` 是 loop 节点：`revise` 回填 research，`pass` 进 report。
- 评审标准必须与能力对齐（不要索取实时网页、版本号或实验数据），否则门永不开。

导入（需要 compatible 的 capability 配置）：

```bash
curl -H "Authorization: Bearer $(cat .local/api-token)" \
  -H 'Content-Type: application/json' \
  http://127.0.0.1:8090/api/bundles/import \
  -d "{\"bundle\": $(cat examples/graphs/deep-research-loop.bundle.json), \
       \"expected_revision\": 0, \"publish\": true, \"import_triggers\": false}"
```

已验证：hash 验签通过；隔离库四进程 + 真模型首轮 pass terminal（25 事件单调）。
