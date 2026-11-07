# 福酱群聊 Agent 路线图

> 仓库：https://github.com/KazeMae/FujiangBot  
> 看板：https://github.com/users/KazeMae/projects/2  
> 总追踪：#19  
> 更新：2026-11-07 提交前本地检查点。计数见后续协调者报告。已接线：标准 `newuidmap`/`newgidmap`（钉住 04755、`fd:N`）、核对过的 user namespace FD、bwrap `--userns`、`--cap-drop ALL` 之后五个命名空间 capability（CHOWN、DAC_OVERRIDE、FOWNER、SETUID、SETGID）。生产引导要求 `setgroups=allow` 与两段映射 `0→宿主 euid/egid`、`1 100000 65536`。嵌套命名空间改由子进程 seccomp 拒绝 `unshare`/`setns`/带新命名空间标志的 `clone`，`clone3` 返回 ENOSYS；与 `--userns` 不能同时用的 `--unshare-user`/`--disable-userns` 已去掉。本机没有这两个辅助程序，所以没有真实完整映射，也没有 apt。先前干净代码 `d0cb317` 的 CI `42375477574`（21:11:31 Asia/Shanghai 全绿）是这一切片之前的历史；更早 `8d7eda1` / `42374926956` 与 `aaa7291` / `42370884522` 同样只属于历史。新的全量计数和推送后的 CI 由协调者另报，这里不报新覆盖率，也不把本段写成已经变绿。生产仍关闭。

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
| 知识库 / RAG | 有 | **不做**（#15 取消） | — |
| 长期记忆 | 有 | 会话摘要有；分层记忆进行中 #46 | M3 |
| Web / 管理 | 有 | 8787 管理口，agent 配置偏 toml | M3 |
| 插件当 tool | 插件生态 | 仅 contest/rank 部分方法 | **插件工具化波次** |
| 工具发现 / 展示 | 常见 | `.agent tools` 列常驻工具。技能包经 `skills.list` 与 `skills.open` 懒加载。`qq.read`、`schedule`、`fun`、`contest` 已是包 | #53。管理页仍不单列 |
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

| Issue | 内容 | 状态 | 说明 |
|---|---|---|---|
| #16 | 会话摘要 fold | Done | `.agent memory` / `forget`。生产路径已不再做摘录折叠 |
| #17 | 管理页用量 / 人格 | Done | `GET /api/agent/usage`、personas UI |
| #46 | Codex 风格分层记忆 | In Progress | 账号 / 群 / 个人（**群内发言也写个人记忆**）；见 `docs/layered-memory.md` |
| #15 | 托管 RAG | 取消 | 不做 |
| #18 | MCP | 延期 | 默认不做 |
| #53 | 技能包懒加载、QQ 权限、定时任务 | Done | T0 #57、T1 #58、T2 #56、T3 #60。方案 `docs/agent-skills.md` |
| #54 T0 | `skills.list` / `skills.open` 与占位包 `ping` | Done | #57 |
| #55 T1 | `qq.read`，按当前发言人只读 | Done | #58。只读他所属的群和他自己的私聊 |
| #56 T2 | 技能包 `schedule` 与 `.agent schedule` | Done | 到点带创建者 QQ，按 `qq.read` 同一成员边界重验。私聊只发给创建者 |
| #60 T3 | 技能包 `fun` 与 `contest` | Done | 不进常驻。打开后仍走 ServiceHub。`fun.learn` / `fun.search` 仍由宿主写入当前群 |
| 群发言人展示 | 群提示里的当前与历史 QQ、昵称、名片、引用 ID | Done | 当前与历史昵称、名片来自真实 `MessageEvent.sender` 快照，值仍按 untrusted 清理控制字符和双向控制符。`host_reply.message_id` 只是事件所带引用 ID，不取正文、不推断作者。旧行 NULL 不回填。真实 ID 不改变权限。私聊不包装。不改 Session / ToolLoop 状态机 |
| 上下文预算与 LLM 交接摘要 | 500000、90% 摘要、模型上限、CAS、官方看图、摘要用量 | 已实现，待 PR 验收 | 有效预算是配置与已核实上限的较小值。官方 `https://api.deepseek.com` 的 `deepseek-flash` 按 2026-11-07 定价页为 100 万，看图页每图 1024 且多图相加。只适用于该主机名和该模型名，不是 OpenAI、Anthropic 或其它模型的 tile/detail 上界。预算含投影 JSON、工具 schema、已核实图像上界和发出的 `max_output_tokens` 4096。usage 按字段保留有效非负整数；缺字段记 `unknown_calls`，不把缺的 0 当成测得值。旧行迁移后标未确认。记账失败仍走 CompactFail |
| 工作区绑定与独立 owner | 每次真实 resolve；个人/群目录；bash/code 规格默认不广告 | 生产关闭。本切片是本地代码，全量计数未知。历史干净 CI 是 `d0cb317` / `42375477574` | 代码已接标准 uidmap 辅助程序、钉住的 namespace FD、bwrap `--userns` 和五个命名空间 capability。辅助程序未安装，没有真实完整内核映射，也没有官方 rootfs 或 apt。单 ID 探测和 `setgroups=deny` 只是否定覆盖。导入 writer 租约仍不 `LOCK_UN`。不报新覆盖率。见 `docs/workspace-rootfs.md` |
| 工作区离线验证入口 | `python3 scripts/verify-workspace-bundle.py /绝对source /私有parent/新snapshot` | 入口已实现。当前代码 CI 是 `8d7eda1` 的 run `42374926956`。先前基线是 `aaa7291` / `42370884522` | 核对私有快照、固定主指纹 `843938DF228D22F7B3742BC0D94AA3F0EFE21092` 与精确 SHA。无 `--unpack` 仍只验证。`92a7b15` / `42367869558`、`a37ac01` / `42362577354`、`32427e4` / `42364745250` 是更早的头。本机真实 gpgv 14 条仍是明确 SKIP。 |
| `--unpack SOURCE NEWTARGET` | 签名检查后的私有解包与树提案 | receipt 带 `tree_sha256` 与 v2 `tree_policy`，`execution_accepted` 恒 false。v1 批准拒绝。没有 `acceptance.json` | 运行时不设 `RLIMIT_FSIZE`。`/tmp` 持久且 `--chmod 1777`。`/run` 与 `/sys` 仍是 64MiB tmpfs。本切片已把标准 uidmap、`fd:N`、`setgroups=allow`、bwrap `--userns` 和五个命名空间 capability 接进 runner。缺已安装的 04755 辅助程序、真实完整映射和官方 rootfs 验收，所以还不是 apt。`box:100000:65536` 仍是显式部署范围。不重试 DNS、改源或 CI 下载。 |

前置：agent 热路径改动优先落在 `command.rs` / `plan.rs` / 新模块；单文件勿超 1000 行。

### 版本与部署

- 功能合入 `main` 不自动部署。
- 生产包 **v0.3.0**：M2 工具化 + M3 记忆/管理页第一刀；部署须用户确认。
- 部署清单仍按 `docs/BRANCHING.md`：备份、只重启 fujiang、不碰 NapCat。

## 并行策略

1. **轨道 D（记忆）**：#46 分层记忆 T0（schema + tools）；群聊按发言者 QQ 更新个人记忆。  
2. **轨道 E**：管理页浏览分层记忆（后切片）。  
3. **RAG / #15**：取消。  
4. **#18**：保持延期。  
5. **轨道 F（#53）**：T0–T3 已完成。#54 已由 #57 合入，#55 已由 #58 合入。#56 的定时任务在技能包 `schedule`。#60 把 `fun` 和 `contest` 挪进技能包。这四件都不进常驻工具。

每个 PR：自测 fmt/clippy/test（+ 相关 llvm-cov）→ 中文草稿 → 自审 → merge commit。不擅自部署。`lib.rs` 顶格时先拆文件。

## 形式化验证范围

| 模型 | 性质（简述） | 优先级 |
|---|---|---|
| `spec/tla/ToolLoop.tla` | 轮数硬顶；工具错误不离开降级态；无 tools 时单次 complete | P0 |
| `spec/tla/Session.tla` | reset 清空消息和摘要并保留人设；forget 只清空摘要；fold 不改人设 | P1 |
| `spec/tla/Runner.tla` | 抽象顺序：阻塞、pin init pidfd、释放 payload、确认 init 死亡并回收 wrapper、join、成功。拒绝时零 spawn。正常finish解除看护前确认init和wrapper清理；Drop仅尽力清理，不证明未知错误下最终无残留。成功前进程清理和 reader 回收都完成。未知清理不能标成功。不证明 OS 隔离或真实 apt | P1 |
| `spec/tla/Bundle.tla` | 抽象顺序：刚验证通过、密封归档、worker 写私有 stage、成功回收、再发布 rootfs 和 receipt。失败先 kill/reap 再 cleanup，且不发布。`executionAccepted` 始终 FALSE。发布前须 verified/hash、sealed、workerReaped。失败没有可用 rootfs。不证明密码、OS、CPU 或权限 | P1 |
| `spec/tla/OwnerLayer.tla` | 生产 `prod` 恒假、base 不变。两 owner 的 inode 彼此不同且不同于 base。policy 为假时零字节、无 metadata、无 partial、锁空闲。staging 持锁且 partial；running 要求 metadata 正常且无 partial。不证明 OS 隔离、配额或官方 rootfs | P1 |

实现须对照模型；改 loop、session、runner、owner 层或 bundle 解包顺序时先改 TLA+ 或同步改。

## 覆盖率范围

1. 先对 `fujiang-plugin-agent` 做到行覆盖 ≥ 80%。  
2. 再对提供 Service 的插件 crate 补齐。  
3. 报告产物与本地命令写入 `docs/engineering.md`（T0 交付）。

## 明确不做

- 为接 AstrBot 改反向 WS 或双开同一 QQ。  
- 234 上拉海外 Docker 跑 AstrBot。  
- Key 进 git 或世界可读示例。  
- 默认开启主动回复。  
- 自建重型向量库。
- 托管 RAG（#15 已取消）。  
- 依赖 cdylib 热插跑生产 agent。
- 把 NapCat 4.18.42 的全部 action 挂成常驻工具（#53，改为技能包懒加载）。
- 默认打开踢人、禁言、退群、cookies、重启类 QQ 写操作。
