# 福酱群聊 Agent 路线图

> 仓库：https://github.com/KazeMae/FujiangBot  
> 看板：https://github.com/users/KazeMae/projects/2  
> 总追踪：#19  
> 更新：2026-11-06（M1 完成；M2 适配层与 rank/contest 内置工具已合入）

本文档是规划源。Issue / Milestone / Project 与此对齐。冲突时以已合并 Issue 的验收为准，并回写本文档。

## 目标

在 fujiang 上做出接近 AstrBot 体验的群聊 agent：能对话、能调工具、能看图、能受控、能量化用量。现有业务插件（contest / rank / problem / fun / luck / memo）要能被 agent 当 tool 用，命令与展示也要对人友好。

不用 AstrBot 本体。不共用同一 QQ。不把 Key 写进仓库。生产 agent 是 builtin（见 #24）。部署到 234 前须用户确认。

## 工程约束（全程）

| 项 | 要求 |
|---|---|
| 分支 | 主干 `main`；短分支 + PR；**merge commit**；不直接推 `main` |
| 作者 | `KazeMae <kazex@qq.com>` |
| PR 文案 | **标题与正文中文** |
| 标签 | 部署相关变更走 bump + tag `vX.Y.Z`；日常功能可不立刻打 tag |
| 形式化 | 关键状态机用 **TLA+**（优先 tool loop、session）；模型放 `spec/tla/`，CI 或脚本跑 TLC |
| 覆盖率 | 行覆盖 **≥ 80%**（先 `fujiang-plugin-agent`，再扩到提供 Service 的插件）；用 `cargo llvm-cov`，结果进 CI |
| 文件长度 | 单文件尽量 **≤ 1000 行**；超了就拆模块，不堆测试到同一文件里凑合 |
| 注释 | **不写注释**；把约束写成类型、测试、文档或 lint |
| Lint 债 | 每次 debug 查到的问题模式，写成 clippy/`deny`、自定义检查或 CI 脚本，防止回潮 |
| 生产 | 不重启 NapCat；部署前备份 DB/`plugin-data`；不删 233；密钥只走环境变量 |
| 开发 | Grok Build + poteto-mode / ponytail / technical-writing |

## 与 AstrBot 的差距（对照）

| 能力 | AstrBot 常见形态 | 福酱现状（2026-11-06） | 计划落点 |
|---|---|---|---|
| 群聊多轮 | 有 | 有（整群 session、唤醒/@/回复） | 维持；M3 再加深记忆 |
| Function calling | 有 | 有（#10 适配层；#11 rank/contest） | 扩到全部业务插件 |
| 人格 / 人设 | 有 | 有（`.agent persona`） | M3 管理页增强 |
| 看图 | 有 | 无 | #12 |
| 限流 / 安全 | 有 | 基础每小时限额 | #13 |
| 用量 | 有 | 仅限额事件表 | #14 |
| 知识库 / RAG | 有 | 无 | M3 |
| 长期记忆 | 有 | 无 | M3 |
| Web / 管理 | 有 | 8787 管理口，agent 配置偏 toml | M3 |
| 插件当 tool | 插件生态 | 仅 contest/rank 部分方法 | **插件工具化波次** |
| 工具发现 / 展示 | 常见 | `.agent tools` 列逻辑名。`inject_tool_list` 默认关。启动日志有同一份名字 | 管理页仍不单列 |
| MCP | 可选 | 不做（#18 按需） | 默认不做 |

## 现状

### 已完成

| 项 | 说明 |
|---|---|
| M1 #5–#9 | 出网、客户端、插件骨架、命令文档、234 冒烟；**v0.2.1** 在 234 |
| #10 | Responses tools ↔ ServiceHub，硬顶 loop |
| #11 / #26 | 内置 `rank.list` / `contest.upcoming` / `contest.today`（wire 下划线） |
| #24 | agent **builtin**（cdylib 双 tokio 会 abort，勿再依赖 `.so` 热插） |

### 插件 Service 盘点

| 插件 | `provides` | 已有方法 | 适合先做的 tool | 展示 / 命令侧 |
|---|---|---|---|---|
| contest | 有 | `upcoming`, `today` | 已接 agent | `.contest` 等保持 |
| rank | 有 | `list`, `get` | `get` 待接 | `.rank` 保持 |
| problem | 无 | — | 先 `provide` 再 tool | 每日题命令 |
| fun | 无 | — | 只读：list/search；写操作慎开 | 图库命令 |
| luck | 无 | — | 只读查询类 | 运势命令 |
| memo | 无 Service（自有库） | 命令 add/list/del | `provide` + tool | `.memo` |
| echo | 示例 | — | 非生产 | — |
| agent | builtin | tools loop | 工具目录自省 | `.agent` |

## 阶段计划

### M2 / M2-T（已完成）

| Issue | 内容 | 状态 |
|---|---|---|
| #12 | 看图 | Done |
| #13 | 限流 / 关键词 / 管理员旁路 | Done |
| #14 | `usage_daily` | Done |
| #28 T0 | TLA+ ToolLoop、覆盖率、工程约定 | Done |
| #29 T1 | `rank.get` + `problem` | Done |
| #34 T2 | `memo` 工具 | Done |
| #35 T3 | `fun` 只读工具 | Done |
| #30 T5 | `.agent tools` / 清单注入 | Done |
| T4 `luck` | 未建 Issue；需要时再开 | 候补 |

### M3（进行中）

| Issue | 内容 | 状态 | 第一刀 |
|---|---|---|---|
| #16 | 长期记忆与上下文压缩 | In Progress | 会话摘要 fold + `.agent memory` / `forget`；无 LLM 摘要；ICL 默认关（后切片） |
| #15 | 托管知识库 / RAG | Todo | 需单独确认出网与密钥后再开工 |
| #17 | 管理界面 / 人格与用量报表 | Todo | 跟在 #16 后；先只读用量 API |
| #18 | MCP | 延期 | 默认不做；有真实需求文档再开 |

前置：`crates/plugins/agent/src/lib.rs` 已顶格 1000 行。任何触及该文件的改动必须先拆 `command` / `plan` 再加功能。

### 版本与部署

- 功能合入 `main` 不自动部署。
- 下一生产包建议：M3 记忆切片可合后 bump **v0.3.0**，再等用户说「部署」。
- 部署清单仍按 `docs/BRANCHING.md`：备份、只重启 fujiang、不碰 NapCat。

## 并行策略

1. **轨道 D（记忆）**：先拆 `lib.rs`，再 #16 摘要 fold；LLM 压缩与 ICL 另 PR。  
2. **轨道 E（管理）**：#17 只读用量 / 人格编辑，尽量不碰 agent 热路径。  
3. **轨道 F（RAG）**：#15 等密钥与出网确认；接口先 mock。  
4. **#18**：保持延期。

每个 PR：自测 fmt/clippy/test（+ 相关 llvm-cov）→ 中文草稿 → 自审 → merge commit。不擅自部署。`lib.rs` 顶格时先拆文件。

## 形式化验证范围

| 模型 | 性质（简述） | 优先级 |
|---|---|---|
| `spec/tla/ToolLoop.tla` | 轮数硬顶；工具错误不离开降级态；无 tools 时单次 complete | P0 |
| `spec/tla/Session.tla` | reset 清空消息和摘要并保留人设；forget 只清空摘要；fold 不改人设 | P1 |

实现须对照模型；改 loop/session 行为时先改 TLA+ 或同步改。

## 覆盖率范围

1. 先对 `fujiang-plugin-agent` 做到行覆盖 ≥ 80%。  
2. 再对提供 Service 的插件 crate 补齐。  
3. 报告产物与本地命令写入 `docs/engineering.md`（T0 交付）。

## 明确不做

- 为接 AstrBot 改反向 WS 或双开同一 QQ。  
- 234 上拉海外 Docker 跑 AstrBot。  
- Key 进 git 或世界可读示例。  
- 默认开启主动回复。  
- 自建重型向量库（优先托管 API）。  
- 依赖 cdylib 热插跑生产 agent。
