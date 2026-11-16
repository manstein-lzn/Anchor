# 飞书组织管理员 Bot

管理员负责 SeeMine、Anchor、Codex Manager 三个项目的目标、优先级、依赖与汇总，不替代项目经理或单个 Codex 会话。组织蓝图见[三级组织图解](feishu-agent-organization.md)，渠道实现仍以[飞书助手](feishu-assistant.md)为准。

## 当前交付与验收边界

- 独立 Graph：`feishu-organization-manager`，最初定义见[管理员示例](../examples/graphs/feishu-organization-manager.json)。应用绑定已由操作员配置并启用，监听为 `authenticated`，已收到真实私聊；初始停用状态仅是部署历史（本机原件，未入库：`.local/organization-manager-20261112/provisioning.json`）。
- 已实现通用 `feishu_cli`，经理可按需发现并使用官方文档、Wiki、Base 和群管理命令，无需逐 API 开发专用工具。渠道须单独开启应用工具授权，调用使用本 Bot 已发布的应用权限与资源 ACL；原私聊/群对话授权不自动扩展。上线及真实租户验收状态见[统一台账](pilot-development-plan.md)。
- 2026-11-12 首轮在模型调用前中断：当时发布的 Host 目录遗漏 `anchor-sandbox-network`，联网 MCP 无法在隔离环境启动。现已补入与已验证产物摘要一致的 helper，验证隔离出网、宿主回环不可达和实际 network MCP 初始化；未重启 Host，未改 Graph、凭据或渠道权限。修复证据（本机原件，未入库：`.local/organization-manager-recovery-20261112/helper-repair.json`）、网络边界（本机原件，未入库：`.local/organization-manager-recovery-20261112/network-boundary-probe.json`）、MCP 握手（本机原件，未入库：`.local/organization-manager-recovery-20261112/network-mcp-probe.json`）。
- 2026-11-12 后续真消息暴露第二个故障：恢复时旧轮的中断已经被 Kernel 接受，但工作区 owner 未释放，下一轮因 `RunConflict` 挂起。现已部署精确修复：同 Run / Graph / 节点的旧 invocation 必须有已接受的中断记录、无待核查原生工具效果，并取得独占写入锁，才释放旧 owner；不清空目录，不放宽共享授权。部署与验证（本机原件，未入库：`.local/feishu-workspace-recovery-20261112/deployment-result.json`）。
- 此次 Host 更新保留渠道、凭据、项目目录和原 Session。2026-11-12 用户已确认「飞书 bot 都可以正常对话了」，私聊故障按用户反馈验收通过；回读管理员与 Anchor 项目经理均无待恢复状态及未决投递，新消息分别于上海时间 17:45:23、17:46:12 confirmed，见新消息验收（本机原件，未入库：`.local/feishu-organization-next-20261112/private-chat-acceptance.json`）。此前旧 rejected 入站仍不补发、不重置去重账本；先前新消息检查（本机原件，未入库：`.local/feishu-workspace-recovery-20261112/new-message-acceptance.json`）记录的是用户确认前的时点，不补写为平台 confirmed 证据。
- 已通过 Host 单测 1,132 项（另 3 项忽略）、固定 Goose + 模拟飞书的双轮收发回归、Clippy；这些不代替真实飞书回执。附件与完整组织协作仍待各自验收。

## 本次先开放的权限

在独立的企业自建应用中添加机器人能力。以下权限均选**应用身份**；基础文本对话只需前两项，文件与图片协作再加后两项。

| 权限标识 | 用途 | 官方依据 |
| --- | --- | --- |
| `im:message.p2p_msg:readonly` | 接收用户给机器人的私聊 | [接收消息][receive] |
| `im:message:send_as_bot` | 以机器人身份回复消息与卡片 | [发送消息][send] |
| `im:message:readonly` | 读取消息、下载用户发送的文件或图片 | [消息资源][resource] |
| `im:resource:upload` | 上传待发送的附件 | [上传文件][upload] |

如需一次配置文本与附件，可在飞书「权限管理 → 批量导入」使用以下内容；仅文本验收可删去后两项。

```json
{
  "scopes": {
    "tenant": [
      "im:message.p2p_msg:readonly",
      "im:message:send_as_bot",
      "im:message:readonly",
      "im:resource:upload"
    ],
    "user": []
  }
}
```

不要为基础上线额外开放全员通讯录、全群消息、应用管理或整个云盘写权限。

## 绑定并验收

1. 在飞书开发者后台创建独立企业自建应用，名称可用「组织总管」，添加机器人能力。不要复用 Anchor 项目经理已绑定的应用。
2. 按前表开权限，将自己的账号加入应用可用范围。获取**这个新应用**下自己的 `open_id`；不同应用的 open_id 不通用，不能复制旧项目经理的名单。[发送消息][send]
3. 打开 Anchor 的 `feishu-organization-manager`，进入 `manager` 节点所挂 `feishu` Plugin 的「此 Graph 的机器人」。接收、回复名单均填新应用下自己的 open_id，不填 `*`。
4. 在同页的专用凭据控件填写 `FEISHU_APP_ID`、`FEISHU_APP_SECRET` 并启用。App Secret 不放进聊天、Graph JSON 或仓库文件。
5. 监听显示 `authenticated` 后，在飞书「事件与回调 → 事件配置」选择长连接，添加 `im.message.receive_v1`；创建并发布包含所需权限和订阅的版本。先有在线长连接才能保存该接收方式；`authenticated` 本身不代表已收到真实消息。步骤沿用[现有配置流程](feishu-assistant.md#配置与首次单聊)。
6. 私聊发送「请列出你负责的三个项目、当前能力和未完成的接入」。确认客户端收到回复、原 Session 有真实 Turn/投递回执，才记作私聊上线通过。不要用伪造入站或人工启动 Run 代替真实飞书验收。

## 群内对话验收

2026-11-12 已部署获准群内的人 @ Manager 文本入口、共享群历史、实时进度和原群最终回复；它不是 Bot 之间的自动委派。最终发布字节的四项原生回归和上线只读核验通过，AnchorWork 两个 Manager 的真实群收发也已验收，见[统一台账](pilot-development-plan.md)。原私聊绑定和成员名单不自动变化，也不因 Bot 被拉进群就授权整个群。

在飞书应用开放 `im:message.group_at_msg:readonly` 并发布版本；随后由操作员指定目标群的 `chat_id`，按[群配置流程](feishu-assistant.md#获准群内的人--bot)保存明确群清单。普通 @ 文本、其他 Bot、未授权群或成员均不唤醒 Manager。群中对话对该群可见，与同一 Manager 的私聊历史隔离。

用户已指定 `AnchorWork` 群。2026-11-12 已通过飞书查询唯一匹配群，群主与管理员既有私聊身份一致，当时为 1 位用户、2 个 Bot；`AnchorManager` 与 `AnchorDevManager` 已配置该群，并将各自接收/回复名单从 `*` 收窄为既有私聊用户的应用内 open_id。通过原有停止/退役/保存路径更新两份角色的群文本边界，原 Session、私聊历史、项目配置、凭据和计划保留。两个 Gateway 已认证，进程群/成员清单与保存值一致，未重启 Host，见启用记录（本机原件，未入库：`.local/anchorwork-activation-20261112/README.md`）。随后两条真实群来信分别创建独立群 Session / 常驻 Run，用户确认「成功得到了两个agent的回复」；后台回读总管 22:18:19、Anchor 项目经理 22:19:46（上海）的 `channel.delivery.confirmed`，均已完成本轮并回到 `wait_input`，无恢复阻塞或未决投递，见真实群验收（本机原件，未入库：`.local/anchorwork-activation-20261112/group-acceptance.json`）。角色更新后的私聊续接尚未复验。

现在可在群里分别真实 @ `AnchorManager` 与 `AnchorDevManager` 对话。私聊与群聊历史隔离；原私聊监控授权不包含群定时通知，不因安装 CLI 自动建计划。新增群成员不会自动获得调用权限；即使只授权一个人发起任务，群内回复仍对该群成员可见。

## 通用工具与协作配置

两位经理各自使用自己的 `feishu_cli` 应用身份，不共享登录或切换 Bot。停用渠道后开启 `cli_enabled`，在明确的 `groups` 下配置对方的 `peer_bots` Bot open ID，再启用；其他 Bot 不自动获准。双方发布 `im:message.group_at_msg.include_bot:readonly` 后，经理回复实际人类群消息并使用结构化 @ 发起委派，对方按同群人类根及每有向 peer 对一次的预算准入。发出消息不证明对方接收或完成；确切边界归[飞书助手](feishu-assistant.md#群内-bot-协作)。

自动查询应用已发布权限需 `application:application.app_version:readonly`。本租户首次只读检查三个应用均返回 `99991672 app_scope_not_applied`；该项缺失不妨碍已有业务权限的调用，只使权限清单未知。云文档、Wiki 与 Base 仍须授权具体资源；首次知识空间由用户建立并共享，不能假装应用身份拥有用户专属能力。

## 后续管理员功能的增量权限

下表是使用通用 CLI 时按实际业务补充的应用权限，不是基础对话的依赖。**建议逐项开放并发布版本，不预先给项目 Bot 同等全局权限。**无需再为每项业务实现新的 Anchor 工具；平台 scope、资源 ACL 和操作结果仍分别核对。

| 后续能力 | 应用身份权限 | 官方依据与约束 |
| --- | --- | --- |
| 群内接收人或其他 Bot 的 @ | `im:message.group_at_msg.include_bot:readonly` | [接收消息][receive]；该权限包含人和 Bot 的提及，不需要另开全群消息读取 |
| 发现自己已发布的权限 | `application:application.app_version:readonly` | CLI `capabilities`；只看已发布版本的 tenant scope，不把草稿或用户身份权限当作授权 |
| 创建项目群 | `im:chat:create` | [创建群][chat-create]；群成员和机器人的加入仍受平台约束 |
| 读取、更新群信息 | `im:chat:read`、`im:chat:update` | [读取群][chat-get]、[更新群][chat-update]；更新还需满足群内角色或群设置 |
| 邀请成员及项目 Bot | `im:chat.members:write_only` | [添加群成员][chat-members]；按 App ID 邀请应用机器人，仍受群管理权限约束 |
| 创建、读取、编辑云文档 | `docx:document` | [创建文档][doc-create]、[读取内容][doc-read]、[编辑内容][doc-edit]；也可按具体 API 拆成 create / readonly / write_only |
| 管理获准知识库的项目目录 | `wiki:wiki` | [知识节点创建][wiki-node]；只管理已授权空间，不代表能以应用身份新建整个空间 |
| 建立与维护多维表格 | `bitable:app` | [创建表格][base-create]、[更新记录][base-update]；需要实际 Base 的编辑权限；可随工具收窄为 base 系列细分权限 |

文档、知识库和 Base 必须分别授权具体资源，应用 scope 不等于全企业内容访问权。应用需在自己的授权范围内操作；后续各项目 Agent 不因身处同一群就自动取得其他项目的编辑权。[文档授权][permission]

首次知识空间由用户先创建并授权即可；[创建知识空间][wiki-space]当前只支持用户身份。不要为了绕过这一步默认申请全局管理权或接管用户账号。`im:chat:operate_as_owner` 也不是基础必开项；优先由用户当群主、明确把总管 Bot 设为群管理员，再按实际接口需要补充。

接口清单于 2026-11-12 核对官方文档；具体租户是否批准权限及真实调用是否成功，仍须上线验收。

[receive]: https://open.feishu.cn/document/server-docs/im-v1/message/events/receive.md
[send]: https://open.feishu.cn/document/server-docs/im-v1/message/create.md
[resource]: https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/message-resource/get.md
[upload]: https://open.feishu.cn/document/server-docs/im-v1/file/create.md
[chat-create]: https://open.feishu.cn/document/server-docs/group/chat/create.md
[chat-get]: https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/chat/get.md
[chat-update]: https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/chat/update.md
[chat-members]: https://open.feishu.cn/document/server-docs/group/chat-member/create.md
[doc-create]: https://open.feishu.cn/document/server-docs/docs/docs/docx-v1/document/create.md
[doc-read]: https://open.feishu.cn/document/server-docs/docs/docs/docx-v1/document/raw_content.md
[doc-edit]: https://open.feishu.cn/document/server-docs/docs/docs/docx-v1/document-block/batch_update.md
[wiki-node]: https://open.feishu.cn/document/server-docs/docs/wiki-v2/space-node/create.md
[wiki-space]: https://open.feishu.cn/document/server-docs/docs/wiki-v2/space/create.md
[base-create]: https://open.feishu.cn/document/server-docs/docs/bitable-v1/app/create.md
[base-update]: https://open.feishu.cn/document/server-docs/docs/bitable-v1/app-table-record/update.md
[permission]: https://open.feishu.cn/document/server-docs/docs/permission/permission-member/create.md
