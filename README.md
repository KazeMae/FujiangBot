# FujiangBot

西南科技大学 ACM 群机器人「福酱」的 Rust 重写。协议层对齐 [NapCat 4.18.19](https://napneko.github.io/api/4.18.19)，形态参考 [node-napcat-ts](https://github.com/HkTeamX/node-napcat-ts)。业务只依赖中间层，收发器可替换。

- **Language:** Rust 2021, MIT
- **Protocol:** OneBot 11 via NapCat / LLOneBot adapters
- **Runtime:** SQLite store, in-process plugin loader (`.so` / `.dylib`), admin page at `[admin].listen`

## Requirements

- A Rust toolchain that can build edition 2021 workspaces
- [NapCat](https://napneko.github.io/) or [LLOneBot](https://llonebot.github.io/) as the OneBot implementation
- Copy `config.example.toml` → `config.toml` and fill adapter, groups, and secrets before the first run

```bash
cp config.example.toml config.toml
cargo run -p fujiang -- run --config config.toml
```

## 架构

```
插件 (内置 + plugins/*.so 热插)
        │  统一 Event / Segment / BotContext / PluginMeta
        ▼
fujiang-core          分发、ACL、调度、帮助
        │
   store (SQLite)     adapter trait
        │                  │
        │                  ▼
        │            adapter（NapCat WS / LLOneBot WS / LLOneBot HTTP）
        │                  │
        │                  ▼
        │               OneBot 11
```

插件禁止依赖 `napcat-sdk`。换协议只加 adapter。

## 运行

`[adapter].backend` 三选一：

| backend | 实现端怎么开 | 配置 |
|---|---|---|
| `napcat_ws` | NapCat 正向 WebSocket | `ws_url`（默认 `ws://127.0.0.1:3001`） |
| `llonebot_ws` | LLOneBot 正向 WebSocket | 同上，只是名字方便对照文档 |
| `llonebot_http` | LLOneBot HTTP 服务 + 事件上报 | `http_api` 填 LLOneBot HTTP 地址；`event_listen` 填本进程监听，并写进 LLOneBot「事件上报」 |

```bash
cp config.example.toml config.toml
# 填 backend、群号、clist；token 可用 NAPCAT_ACCESS_TOKEN
cargo run -p fujiang -- run --config config.toml
```

HTTP 对接示例：LLOneBot HTTP 监听 `3000`，事件上报 `http://127.0.0.1:5700/`，本进程 `event_listen = "127.0.0.1:5700"`。

从旧 Python 数据导入：

```bash
cargo run -p fujiang -- migrate --from ./FujiangBot --config config.toml
```

关掉内置插件：`[plugins.<name>].enabled = false`，或管理页 Entry 表里停用。动态插件停用写入 `plugins.disabled`，库可以留在内存，不删磁盘文件。写法、分发规则、ABI 和管理页 API 见 [插件说明](docs/plugins.md)。

消息按群分表（`msg_g<群号>`，私聊 `msg_pm`）。超过 `archive_after_days` 的记录按月落到 `archive_dir`（如 `data/archive/msg_g741798363_202608.sqlite`）。`archive_after_days = 0` 关闭归档。

## 配置页

进程起来后打开 `http://127.0.0.1:8787/`（`[admin].listen`）。保存会写回 `config.toml` 并尽量热加载：

| 改什么 | 是否立刻生效 |
|---|---|
| 命令前缀、群白名单、Clist、fun 权限、归档天数/周期 | 是 |
| 插件开关；contest/rank 刷新间隔 | 是（开关热插拔，间隔会重启该插件后台循环） |
| adapter backend / 地址 / token | 是（停旧连接再起新适配器） |
| `store.db` / `image_root` / `archive_dir` / `admin.listen` | 只写入文件，需重启进程 |

密钥框留空或保持 `********` 表示不改；点「清空」才擦掉。`[admin].token` 非空时页面会要口令。保存会重写整个 TOML，原文件里的注释会丢掉。管理页端口不要和 `adapter.event_listen` 相同。

## 插件

群里发 `.help` 会拼出当前已启用插件的说明。前缀默认 `.`，区分大小写。

内置插件按 `PluginMeta` 登记（名字、版本、简介、命令）。额外插件编译成 `.so` / `.dylib` 放到 `plugins/`（`[plugins].dir`），进程会扫描并在 `watch = true` 时热加载。完整约定见 [插件说明](docs/plugins.md)。示例：

```bash
cargo build -p fujiang-plugin-echo    # 最小：.ping → pong
cargo build -p fujiang-plugin-memo    # 完整：私有 SQLite 备忘
cp target/debug/libfujiang_plugin_echo.dylib plugins/   # Linux 用 .so
cp target/debug/libfujiang_plugin_memo.dylib plugins/
```

群里 `.ping` 应回复 `pong`；`.memo add 明天交题` 写入 `data/plugin-data/memo/plugin.sqlite`，不进主库。管理页可以启用 / 停用 / 加载 / 卸载 / 重载，也能改动态插件的 JSON。`.so` 必须用本仓库同一套 rustc 编（ABI 3），不能跨版本拷贝。重载时新库启动失败会继续用旧实例。

### contest

比赛日历。后台定时拉 Codeforces / 洛谷 / 牛客 / AtCoder / SCPC；开赛前约 1 小时会在已设提醒的群里预告。

| 命令 | 作用 |
|---|---|
| `.contest` | 说明 |
| `.cf` `.lg` `.nc` `.atc` `.scpc` | 对应 OJ 最近一场（未开始优先） |
| `.cfall` `.lgall` `.ncall` `.atcall` `.scpcall` | 该 OJ 缓存里的全部场次 |
| `.day` | 今天的比赛 |
| `.bot` | 数据上次刷新时间 |
| `.remindHH:MM` | 仅群聊。每天这个点推送各 OJ 最近一场，如 `.remind08:30` |
| `.remindoff` | 仅群聊。关掉本群每日提醒 |

### rank

Codeforces 排行。`.rank` 和 `.rk` 相同。`{year*}` 可写多个年级，省略则看全部。后台会定时刷 rating 和近期比赛排名。

| 命令 | 作用 |
|---|---|
| `.rank -h` | 说明 |
| `.rank -a <年级> <姓名> <handle...>` | 添加一人或多人 |
| `.rank -r <handle...>` | 删除 |
| `.rank -l [年级...]` | 列出账号 |
| `.rank -s / -m / -vr / -c / -lc [年级...]` | 当前分 / 最高分 / 有效分 / 过题数 / 近 30 天过题 |
| `.rank -t <年级> <姓名>` | 生涯摘要 |
| `.rank -cs <contest_id>` | 这场比赛里已登记同学的分数变化 |
| `.rank -urk` / `-urt` | 立刻后台刷新比赛排名 / rating |
| `.rank -u` | 上次刷新时间 |

长选项：`--add` `--remove` `--list` `--show` `--max` `--validRating` `--count` `--lastCount` `--totalLife` `--contestStandings` `--updateRank` `--updateRating` `--updateTime`。

### problem

从 Codeforces 题库抽题。`.problem` 不带参数会打出说明。

| 命令 | 作用 |
|---|---|
| `.problem <L> <R> [tags...] [-rt]` | `L` `R` 为 800～3500 的整百 rating。tag 空格写成下划线；`!tag` 排除；`new` / `!new` 限新/旧场。默认抽一道，`-rt` 换抽法 |
| `.cftag` | 可用 tag 列表 |
| `.每日一题` | 当天五档（800–1000 … 1600–2000）。过了 `plugins.problem.daily_reset_hour`（默认 4 点）才换新的一天 |

### fun

学话、收藏夹、图库。改学习/图库默认要在 `plugins.fun.admins` 里（名单空=谁都能改）。

**学话** 是原话触发：本群有条目只用本群的，没有则用导入/私聊里的全局词库。

| 命令 | 作用 |
|---|---|
| `.learn add <触发词> <回复>` | 写入当前群或私聊 |
| `.learn list [触发词]` | 列出本群/私聊里的句子 |
| `.learn del <触发词> [n]` | 删全部；带序号只删第 n 条 |
| `.star` | 列出收藏 |
| `.star add\|set\|del <名> [url]` | 新增（重名拒绝）/ 覆盖 / 删除 |
| `.tag list` | 所有 tag、条数、别名 |
| `.tag add <tag>` | 只建空 tag |
| `.tag alias <tag> <别名>` | `来只别名` 也算这个 tag |
| `.tag merge <from> <to>` | 把 from 上的条目也挂到 to，from 还在 |
| `.tag retire <tag>` | 去掉这个 tag（文件还在） |
| `来只<tag>` | 随机一条带该 tag 的图或视频 |
| 回复图/视频 + `.添加<tag>` / `.删除<tag>` / `.标签` | 挂 tag / 只摘这一个 / 列出这条的 tag |

图片和视频共用 tag；摘掉最后一个 tag 也不删文件。

### luck

按日期和 QQ 号生成今天的幸运数字：同一天同一人同一 `N` 结果不变，换天会变。

| 命令 | 作用 |
|---|---|
| `.luck N` | 在 1～N 里抽一个整数，`N ≥ 1` |

## 文档

- [插件说明](docs/plugins.md)（写插件、分发、Scope、配置、动态 `.so`、管理页 API）
- [插件运行时取舍](docs/plugin-lifecycle.md)
- [本期不做](docs/out-of-scope.md)
- [以后再说](docs/later.md)
- [napcat-sdk API 缺口](crates/napcat-sdk/API_GAP.md)

## 开发

```bash
cargo test --workspace
cargo fmt
```
