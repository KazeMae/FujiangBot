# FujiangBot

西南科技大学 ACM 群机器人「福酱」的 Rust 重写。协议层对齐 [NapCat 4.18.19](https://napneko.github.io/api/4.18.19)，形态参考 [node-napcat-ts](https://github.com/HkTeamX/node-napcat-ts)。业务只依赖中间层，收发器可替换。

## 架构

```
插件 (contest / rank / problem / fun / luck)
        │  统一 Event / Segment / BotContext
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

关掉某个插件：在 `config.toml` 里设 `[plugins.<name>].enabled = false`，或打开管理页开关。

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

| 插件 | 命令 |
|---|---|
| contest | `.contest` `.cf` `.lg` `.nc` `.atc` `.scpc` `*all` `.day` `.bot` `.remindHH:MM` `.remindoff` |
| rank | `.rank` / `.rk` |
| problem | `.problem` `.cftag` `.每日一题` |
| fun | `.learn`（按群隔离） `.star` `.tag` `来只xx` 回图挂/摘 tag |
| luck | `.luck N` |

## 文档

- [本期不做](docs/out-of-scope.md)
- [以后再说](docs/later.md)
- [napcat-sdk API 缺口](crates/napcat-sdk/API_GAP.md)

## 开发

```bash
cargo test --workspace
cargo fmt
```
