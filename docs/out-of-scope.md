# 本期不做

本期把 Python 福酱重写成 Rust + SQLite。这里记录**明确不做**的项，避免口头范围事后丢失。要改范围，改这份文档并提交。

| 项 | 现状 | 不做原因 | 以后若做 |
|---|---|---|---|
| DeepSeek `.ds` 猫娘 | `test.py` 硬编码 API key | 与 ACM 主路无关，密钥裸奔 | 独立 plugin crate，key 只走环境变量 |
| 对分易 Duifene | `dfe.json` 存明文密码 | 安全风险，学校 LMS | 独立插件，密码不落库 |
| YF `.tx` 定时 @ | `YF.json` | 泛用提醒，非核心 | 调度器已有，加 plugin 即可 |
| YTY `.id` / `.analyse` | 已注释下线 | 已下线 | 可并进 Rank 或独立插件 |
| CodeAI 收 cpp | 已注释且入口会崩 | 损坏 | 不恢复 |
| HTTP 反向上报 | 旧 Python 方式 | 本期正向 WS；HTTP 事件上报已作为 LLOneBot adapter | 反向 WS（实现端连 bot）仍不做 |
| 整棵 `src/` 进 git | 大量 jpg | 仓库膨胀 | 图库存 `data/`，gitignore |
| 冷门 NapCat action 业务封装 | — | SDK 只保证 typed `send` | 谁用谁在 sdk 加 thin wrapper |
| 接力旧 Python git 历史 | `FujiangBot/.git` | 新仓库在工作区根，不嵌套 | 旧目录当快照，不迁历史 |

密钥（clist、NapCat token、任何 LLM key）只允许出现在本机 `config.toml` 或环境变量，禁止进仓库。
