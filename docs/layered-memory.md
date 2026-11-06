# 分层记忆（Codex 风格）

对照 [openai/codex `codex-rs/memories`](https://github.com/openai/codex/blob/main/codex-rs/memories/README.md)。福酱不做托管 RAG（#15 已取消）。记忆由 **agent 工具显式读写**，落在 agent 自己的 sqlite（`plugin-data/agent/`），与会话摘要 `sessions.summary`（#16）并存：摘要管「窗口外对话压缩」，本设计管「跨会话长期事实」。

## 从 Codex 学到什么

| Codex | 福酱对应 |
|---|---|
| Phase 1：从单次 rollout 抽出结构化记忆 | 第一刀不做后台抽记忆；改由模型经 **tool** 决定写什么（可控、可审计） |
| Phase 2：全局合并 / 去重 / 修剪 | 后续切片：可选 consolidation；第一刀只做 CRUD |
| `raw_memory` + 紧凑摘要 | 每条记忆：`key` + `value`（短文本）+ `updated_at`；可选 `source` |
| 读写分路径、引用与用量 | `memory.get` / `memory.list` 读；`memory.upsert` / `memory.forget` 写；后续再加引用计数 |
| 密钥脱敏 | upsert 时拒绝明显密钥形态（后续 lint）；第一刀文档约定不写密钥 |
| 非 ephemeral、功能开关 | `layered_memory.enabled`（默认 true） |

第一刀刻意 **小于** Codex：没有后台 Phase1/2、没有 git workspace、没有子代理 consolidation。形状对齐「分层 + 工具读写 + 可遗忘」。

## 三层主体

| 层 | scope 键 | 谁的记忆 | 典型内容 |
|---|---|---|---|
| `account` | 固定 `account` | 福酱账号全局 | 口吻、禁区、运营约定 |
| `group` | `group:{群号}` | 某个群 | 群规、常用梗、活动 |
| `user` | `user:{QQ}` | **某个 QQ 号** | 称呼、偏好、做过的事 |

### 个人记忆与群聊（硬性）

- **私聊**：发言者 QQ → 更新/读取该 `user:{QQ}`。
- **群聊**：发言者 QQ **同样**可更新/读取该 `user:{QQ}`。群内发言会影响个人记忆，不只私聊。
- 群记忆用 `group:{群号}`，与个人记忆分离。一条群消息可以同时：读群记忆 + 读/写发言者个人记忆。
- 工具入参里的 `user_id`：宿主在调用时注入**当前发言者** QQ，模型不可伪造他人 QQ 写个人记忆（与 memo 的 chat scope 同思路）。若需「记别人」，第一刀不做；后续若开放须管理员旁路。

## 数据形状（第一刀）

表 `layered_memories`（名可微调）：

- `scope_kind`：`account` | `group` | `user`
- `scope_id`：空（account）/ 群号 / QQ 号
- `key`：短键，同 scope 内唯一
- `value`：文本，长度上限（如 500 字）
- `updated_at`：unix 秒
- `source`：可选，`group:{id}` / `private` 等，便于审计「这条个人记忆来自哪次群聊」

主键或唯一索引：`(scope_kind, scope_id, key)`。

与现有 `sessions.summary`：**不合并**。摘要仍按会话折叠；分层记忆跨会话。

## Agent 工具（第一刀）

逻辑名（wire 下划线）：

| 工具 | 作用 |
|---|---|
| `memory.list` | 按层列出 key（可限 scope）；群会话默认可见 account + 本群 + 发言者 user |
| `memory.get` | 取一条 |
| `memory.upsert` | 写入/更新；user 层强制当前发言者 QQ |
| `memory.forget` | 删一条或按 key 前缀清（谨慎） |

开关：`enable_memory_tools` 默认 true；写工具可另用 `enable_memory_write_tools` 默认 true（或与 memo 一样写默认开——群助场景要能记，默认开写，forget 保留）。

注入：组 prompt 时 **不**自动塞全文（避免爆窗）。模型先 `list`/`get`。可选后续：把「本层 key 清单」短注入，第一刀不做。

## 与现有命令

- `.agent memory` / `forget`：继续只管 **会话摘要**（#16），文案可注明「会话摘要，不是分层记忆」。
- 分层记忆只走 tools（或后续再加 `.agent note`）。

## 切片

1. **T0（本波实现）**：schema + 工具 + 宿主注入发言者 QQ；群/私聊单测；覆盖率 ≥80%。
2. **T1**：后台 consolidation（类 Phase2）、去重、用量/引用。
3. **T2**：管理页只读浏览三层记忆。

## 明确不做

- 托管知识库 / 向量 RAG（#15）
- 模型静默改记忆（无 tool call 不写库）
- 第一刀跨用户写个人记忆

## 参考

- https://github.com/openai/codex/blob/main/codex-rs/memories/README.md
- 本仓库 `docs/agent.md`（会话摘要、tools）
- Issue #46
