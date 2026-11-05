# FujiangBot 分支管理手册（草稿）

> 适用仓库：https://github.com/KazeMae/FujiangBot ｜ 默认分支：`main`
> 现状数据采集于 2026-11-05（北京时间），来源：只读 clone + GitHub API。

## 0. 现状（实际查到的）

| 项目 | 现状 |
|---|---|
| 远程分支 | 只有 `main`（最新 `01350a9`，2026-09-18）。PR 分支合并后都已删除 |
| 提交数 | `main` 上共 21 个提交 |
| 标签 / Release | 无 |
| PR | 3 个（#1 feat/dynamic-plugins、#2 feat/plugin-owned-sqlite、#3 feat/lai-zhi-video），全部用 **merge commit** 合并（2026-08-20 ~ 08-21） |
| 直接推送到 main | 有：`b74fc49`（docs，README）、`01350a9`（feat: plugin services, events, and multiple instances）没有走 PR |
| 提交信息 | 基本是 Conventional Commits（`feat:` / `fix:` / `refactor:` / `docs:`），有 1 条例外：`343c702 来只 bug` |
| CI | 无 `.github/workflows` |
| CODEOWNERS | 无 |
| 分支保护 | `main` 未开启保护（API 返回 "Branch not protected"） |
| 生产部署 | 内网 234 服务器上手动部署；之前部署的是 `f71083e`，正在更新到 main 最新版。没有自动部署，也没有监控 |

## 1. 分支模型：轻量主干开发

- `main` 是唯一的长期分支，**始终可编译、可部署**。
- 所有改动都在短生命周期分支上做，通过 PR 合回 `main`，分支存活尽量不超过 1 周。
- 不设 `develop` / `release` 分支：只有一个生产环境，发布节奏是手动、按需，不需要 Git Flow。

## 2. 分支命名

格式：`<类型>/<简短英文描述>`，小写，单词用 `-` 连接（沿用现有 `feat/lai-zhi-video` 风格，中文概念用拼音）。

| 前缀 | 用途 | 例子 |
|---|---|---|
| `feat/` | 新功能、新插件 | `feat/memo-search` |
| `fix/` | 普通 bug 修复 | `fix/reply-image-timeout` |
| `chore/` | 依赖、构建、脚本、文档 | `chore/bump-tokio` |
| `refactor/` | 不改行为的重构 | `refactor/plugin-scope` |
| `hotfix/` | 线上（234）紧急修复，见第 6 节 | `hotfix/napcat-reconnect` |

## 3. 提交信息规范

沿用现有的 Conventional Commits：

```
<type>(<可选范围>): <祈使句简述，英文或中文均可，≤72 字符>

<可选正文：为什么改、影响哪些插件/数据>
```

- type：`feat` `fix` `refactor` `docs` `chore` `perf` `test`。
- 涉及数据库结构或数据迁移时，正文里写一行 `DB: <改了什么>`，部署时据此先备份。
- 破坏性改动（插件 ABI、配置项改名）加 `!`，如 `feat(plugin)!: ...`，并在正文写 `BREAKING CHANGE:`。
- 不再写 `来只 bug` 这种无类型的提交。

## 4. PR 流程

1. 从最新 `main` 拉分支，改完先开 **草稿 PR**。
2. PR 描述必须写：做了什么、怎么测的（本地 `cargo build --release` / `cargo test` / 连 NapCat 实测）、是否改数据库、是否改 `config.toml` 格式、是否需要重启 NapCat（默认应为“否”）。
3. 自测通过后转为 Ready for review。欢迎他人评审，但不强制批准。
4. **合并条件**：本地 `cargo build --release` 和 `cargo test` 通过（加上 CI 后以 CI 检查为准）+ 分支已与 `main` 同步。不要求批准。
5. **合并方式**：使用 **merge commit**（Create a merge commit），保持现有 PR #1–#3 的做法，保留分支上的完整提交历史。代码回退时 revert 对应的合并提交（`git revert -m 1 <merge-commit>`）。
6. 合并后删除源分支（开启仓库的 “Automatically delete head branches”）。
7. 禁止直接推送 `main`（包括文档改动），统一走 PR。

## 5. 版本与标签（语义化版本 vX.Y.Z）

- 版本号以根 `Cargo.toml` 的 `[workspace.package] version` 为准（各 crate 用 `version.workspace = true` 继承）。当前值为 `0.1.0`，`f71083e` 和 `01350a9` 上也都是 `0.1.0`。
- 何时升级：
  - **MAJOR**：不兼容改动，如插件 ABI 变化导致旧 `.so` 插件无法加载、`config.toml` 字段改名或删除、数据库需不可逆迁移。1.0.0 之前，这类改动升 MINOR。
  - **MINOR**：向后兼容的新功能，如新插件、新命令、新配置项（有默认值）。
  - **PATCH**：只修 bug、性能或文档，不改行为接口。
- 每次部署到 234 的流程：
  1. 在 PR 里把 `Cargo.toml` 的 version 改成新版本（与 `Cargo.lock` 一起提交），合并进 `main`。
  2. 用这个 commit 构建并部署。
  3. 在**被部署的 commit** 上打 annotated tag：`git tag -a vX.Y.Z -m "deploy to 234: <摘要>; prev: <旧版本>; DB: <是否改动>"`，再 `git push origin vX.Y.Z`。tag 名必须与 `Cargo.toml` 的 version 一致。
  4. 可选：同时发 GitHub Release，写变更说明。
- 234 上的 `~/FujiangBot/DEPLOYED` 记录 commit 和 tag，与 Git 保持一致。
- 历史版本补标签建议（仅建议，尚未执行）：
  - `f71083e`（之前 233/234 运行的版本，Cargo.toml 为 0.1.0）→ 补 `v0.1.0`。
  - 当前 `main`（`01350a9`，新增插件服务、事件、多实例，属于 MINOR）的 Cargo.toml 仍是 0.1.0。部署前应先通过 PR 把 version 升到 `0.2.0`，再在那个 commit 上打 `v0.2.0`。

## 6. 热修复流程

1. 从**当前线上的版本标签**（`vX.Y.Z`）拉 `hotfix/...`（若 `main` 自该标签后无未部署的改动，也可直接从 `main` 拉）。
2. 只做最小修复，开 PR，可简化评审但仍需 1 人过目。
3. 合入 `main`；若是从旧标签拉的分支，部署产物用该 hotfix 分支的 commit 构建，PATCH 加 1 并打对应 `vX.Y.Z` 标签，随后确保修复已进 `main`。
4. 部署步骤同第 7 节，只重启 fujiang，**不重启 NapCat/QQ**。

## 7. 部署检查单（与分支管理配套）

1. 在 box 上 checkout 目标 commit/标签，`cargo build --release`。
2. 234 上备份：旧二进制 → `target/release/fujiang.<旧commit>`；`data/fujiang.db` 连同 `-wal` / `-shm` 一起备份（以及插件自己的 sqlite 文件）。
3. 传二进制，停 fujiang（SIGINT/SIGTERM），启动新版。
4. 健康检查：`127.0.0.1:3001`、`127.0.0.1:8787` 在监听，日志出现 `napcat websocket open`。
5. 打 `vX.Y.Z` annotated 标签并推送，更新 `DEPLOYED` 文件。

## 8. 回退流程

按影响由小到大：

1. **二进制回退**（首选）：停 fujiang → 换回 `fujiang.<旧commit>` 备份 → 启动。若新版改过数据库，同时恢复部署前备份的 db（含 -wal/-shm）。
2. **代码回退**：在 `main` 上 revert 出问题的合并提交（`git revert -m 1 <merge-commit>`），走 PR，重新部署并打新标签。不对 `main` 做 force push。
3. **整机回退到 233**：仅在 234 无法恢复时使用。先彻底停掉 234 上的 fujiang 和 QQ（同一 QQ 号不能两边同时在线），再在 233 上启动，可能需要用户本人扫码。233 上的数据和程序不得删除。

## 9. 清理已合并分支

- 合并后由 GitHub 自动删除源分支；本地定期 `git fetch --prune`。
- 每月检查一次：超过 30 天没有提交、且未合并的分支，询问作者后关闭或删除。
- 标签不删除（它们是回退依据）。

### 现有分支清理清单

| 分支 | 最近提交 | 已合并进 main | 处理 |
|---|---|---|---|
| `main` | `01350a9` 2026-09-18 | — | 保留 |
| `feat/dynamic-plugins`（PR #1） | — | 是 | 远程已不存在，无需处理 |
| `feat/plugin-owned-sqlite`（PR #2） | — | 是 | 远程已不存在，无需处理 |
| `feat/lai-zhi-video`（PR #3） | — | 是 | 远程已不存在，无需处理 |

结论：目前没有需要清理的远程分支。各开发者本地分支情况未确认。

## 10. 分支保护（main）

- 必须通过 PR 合并（Require a pull request before merging），**不要求批准数**。
- 禁止直接推送 `main`，禁止 force push 和删除 `main`。
- 等 CI 加上后（`cargo fmt --check`、`cargo clippy`、`cargo build --release`、`cargo test`），再开启“检查通过才能合并”。
- 合并方式允许 merge commit；开启 Automatically delete head branches。
- 可选：用 Tag ruleset 保护 `v*` 标签不被删除或改写。

## 11. 已定决定

1. 合并方式：保持 merge commit。
2. 版本标签：语义化版本 `vX.Y.Z`，与 Cargo.toml 对齐，每次部署打 annotated tag。
3. 分支保护：不强制批准，只要求必须走 PR、禁止直推和 force push，CI 加上后再要求检查通过。
