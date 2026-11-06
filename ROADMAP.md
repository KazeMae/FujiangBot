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
| 工具发现 / 展示 | 常见 | 无用户可见清单 | agent 工具目录 + `.agent tools` |
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

### M2 收尾（已有 Issue）

| Issue | 内容 | 依赖 | 分支建议 | 状态 |
|---|---|---|---|---|
| #12 | 看图：`fetch_media_bytes` → 多模态 | M1；建议 #10 后 | `feat/agent-vision` | Todo |
| #13 | 更细限流、关键词、管理员旁路 | #8 | `feat/agent-safety-limits` | Todo |
| #14 | `usage_daily` 用量统计 | #7 | `feat/agent-usage-stats` | Todo |

### M2-T 插件工具化（新建 Issue，可与 #12–#14 并行）

原则：每个插件先补 `Service` + JSON 友好的方法，再在 `builtin_specs`（或配置）注册 tool；写操作默认关或要管理员。wire 名继续 `service_method`。

| 波次 | 内容 | 验收要点 |
|---|---|---|
| T0 | 工程底座：`spec/tla/ToolLoop.tla`（+ 可选 Session）；`cargo llvm-cov` 门槛；超 1000 行拆分门槛；debug→lint 流程写入 `docs/BRANCHING.md` 或 `docs/engineering.md` | TLC 可跑；agent 包覆盖率报告可出；CI 草稿 |
| T1 | `rank.get` 注册进 agent；`problem` provide + tool | 假 hub / 单测；配置可关 |
| T2 | `memo` provide + tool（list/add 策略写清） | 私有库不进主库 |
| T3 | `fun` 只读 tool；写 tool 默认关 | 不误删图库 |
| T4 | `luck` tool | 降级提示 |
| T5 | 工具展示：`.agent tools`、system 注入可用工具摘要、管理口只读列表 | 人与模型都能看到清单 |

### M3（已有 Issue，按需排期）

#15 RAG · #16 长期记忆 · #17 管理界面 · #18 MCP（默认不做）

### 版本与部署

- 功能合入 `main` 不自动部署。
- 下一生产包建议：M2 看图 + 工具化一波 + 覆盖率底座后 bump **v0.3.0**，再等用户说「部署」。
- 部署清单仍按 `docs/BRANCHING.md`：备份、只重启 fujiang、不碰 NapCat。

## 并行策略

1. **轨道 A（体验）**：#12 识图 ↔ #13 安全（可并行，注意 `client`/`config` 冲突时 rebase）。  
2. **轨道 B（工具）**：T1→T5 按插件拆 PR，互不改同一 Service 时可并行。  
3. **轨道 C（质量）**：T0（TLA+、覆盖率、拆文件、lint）尽早合，给后面 PR 垫底座。  

每个 PR：自测 fmt/clippy/test（+ 相关 llvm-cov）→ 中文草稿 → 自审 → merge commit。不擅自部署。

## 形式化验证范围

| 模型 | 性质（简述） | 优先级 |
|---|---|---|
| `spec/tla/ToolLoop.tla` | 轮数硬顶；工具错误不离开降级态；无 tools 时单次 complete | P0 |
| `spec/tla/Session.tla` | reset 清空对话保留人设；限额与 reset 正交 | P1 |

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
