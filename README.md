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
        │            napcat-sdk  ← 外部协议，正向 WebSocket
        │                  │
        │                  ▼
        │               NapCat
```

插件禁止依赖 `napcat-sdk`。换协议只加 adapter。

## 运行

本机先开 NapCat，启用**正向 WebSocket**（默认示例 `ws://127.0.0.1:3001`）。

```bash
cp config.example.toml config.toml
# 填 ws_url、群号、clist；token 可用环境变量 NAPCAT_ACCESS_TOKEN
cargo run -p fujiang -- run --config config.toml
```

从旧 Python 数据导入：

```bash
cargo run -p fujiang -- migrate --from ./FujiangBot --config config.toml
```

关掉某个插件：在 `config.toml` 里设 `[plugins.<name>].enabled = false`。

消息按群分表（`msg_g<群号>`，私聊 `msg_pm`）。超过 `archive_after_days` 的记录按月落到 `archive_dir`（如 `data/archive/msg_g741798363_202608.sqlite`）。`archive_after_days = 0` 关闭归档。

## 插件

| 插件 | 命令 |
|---|---|
| contest | `.contest` `.cf` `.lg` `.nc` `.atc` `.scpc` `*all` `.day` `.bot` `.remindHH:MM` `.remindoff` |
| rank | `.rank` / `.rk` |
| problem | `.problem` `.tag` `.每日一题` |
| fun | `.learn`（按群隔离） `.star` `.idea` `来只xx` 回图挂/摘 idea |
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
