# 做覆盖率、文件长度和 lint

这三项检查对着 `fujiang-plugin-agent`。规划里的总表在 [路线图](../ROADMAP.md)。

## 量 fujiang-plugin-agent 的行覆盖率

行覆盖的目标是至少 80%。在仓库根目录执行。

```
scripts/llvm-cov-agent.sh
```

脚本发现没有 `llvm-tools` 时，执行 `rustup component add llvm-tools-preview`。发现没有 `cargo-llvm-cov` 时，执行 `cargo install cargo-llvm-cov --locked`。然后它运行下面的命令。

```
cargo llvm-cov -p fujiang-plugin-agent --summary-only
```

看摘要里的 Lines。低于 80% 就补 `fujiang-plugin-agent` 的测试。GitHub Actions 里同一步允许失败。这一步失败不会挡住合并。本地的目标仍是 80%。

要自己敲命令时，先完成上面的 `rustup` 和 `cargo install`，再运行那条 `cargo llvm-cov`。

## 把单个 Rust 文件保持在 1000 行以内

先数 `run_tool_loop` 所在的两个文件。

```
wc -l crates/plugins/agent/src/tools/mod.rs crates/plugins/agent/src/tools/tests.rs
```

`scripts/check-file-length.sh` 检查 `crates/plugins/agent` 下每个 `.rs` 文件。某一文件超过 1000 行时，脚本打印行数和路径，并以状态码 1 退出。

文件到了 900 行，就把测试拆到子模块。`run_tool_loop` 的测试在 `crates/plugins/agent/src/tools/tests.rs`。

`crates/fujiang-core/src/dispatch.rs` 和 `crates/fujiang-store/src/lib.rs` 已经超过 1000 行。这个脚本不检查它们。拆那两个文件另做一次。

## 不在 Rust 里写注释

把约束写进类型、测试、这篇文档，或 lint。不要用源码注释代替类型、测试、文档或 lint。

## 把 debug 里的问题写成检查

调试时抓到一种还会再出现的错误，按这个顺序做。

1. 写一个测试。bug 还在时，这个测试失败。
2. 改实现，让这个测试通过。
3. 同一种错误还能写出来的话，加上 clippy 的 `deny`、一条仓库脚本，或 GitHub Actions 里的一步。
4. 跑格式和 clippy。

```
cargo fmt
cargo clippy -p fujiang-plugin-agent --all-targets --no-deps -- -D warnings
```

`-D warnings` 把警告当成失败。

## 2026-11-07 上下文预算与交接摘要

协调者在本机跑过下面这些命令。这一轮没有测生产环境、NapCat，也没有打真实 provider。字节上界不是精确 tokenizer 验收。工作区沙箱没有实现。下面这组数字记录的是看图恢复和摘要用量入账之前的结果。当时未知视觉上界会拒绝图片，交接摘要也还没写入 `usage_daily`。

下面保留首刀基线，不代表当前补丁验收；当前补丁以 PR 里的最新本地验收记录为准。记账失败走 Session 里已有的 `Fail`（文档里称 CompactFail），不新增状态：历史留下，用户看到忙，摘要不能提交。昵称、名片、引用 ID 的 metadata 快照不改变 ToolLoop 或 Session 的权限状态。工作区和沙箱仍未实现。

在仓库根目录：

```
cargo fmt --all -- --check
cargo clippy -p fujiang-plugin-agent --all-targets --no-deps -- -D warnings
cargo test --workspace
scripts/check-file-length.sh
scripts/run-tlc.sh
cargo llvm-cov -p fujiang-plugin-agent --summary-only
```

当时的结果：`fmt` 检查通过。clippy 在 `-D warnings` 下通过。`cargo test --workspace` 216 项通过，0 失败，0 ignored；其中 `fujiang-plugin-agent` 134 项通过。`scripts/check-file-length.sh` 通过。`scripts/run-tlc.sh` 的 ToolLoop 和 Session 都没有错误；ToolLoop 54 个 distinct states，Session 688 个 distinct states。`cargo llvm-cov -p fujiang-plugin-agent --summary-only` 的行覆盖是 92.62%。`compact.rs` 81.57%，`context_budget.rs` 98.89%，`speaker.rs` 97.88%。

## 2026-11-07 工作区绑定切片

上面的 216 项和 134 项是更早一刀的记录。更早的 224 项 workspace 测试和 142 项 agent 测试，以及任何 namespace 探针，都不是这一批的验收。协调者在仓库根目录跑过：

```
cargo fmt --all -- --check
cargo clippy -p fujiang-plugin-agent --all-targets --no-deps -- -D warnings
cargo test --workspace
scripts/check-file-length.sh
scripts/run-tlc.sh
cargo llvm-cov -p fujiang-plugin-agent --summary-only
python3 scripts/test-workspace-rootfs.py
sh -n scripts/prepare-workspace-rootfs.sh
```

`fmt` 检查通过。clippy 与 CI 相同，在 `-D warnings` 下通过。`scripts/check-file-length.sh` 通过。`cargo test --workspace` 236 项通过，0 失败，0 ignored；其中 `fujiang-plugin-agent` 154 项通过，比此前的 142 项多 12 项。`cargo llvm-cov -p fujiang-plugin-agent --summary-only` 的行覆盖是 92.03%。`compact.rs` 83.21%，`context_budget.rs` 99.40%，`session.rs` 99.16%，`workspace.rs` 91.86%。`scripts/run-tlc.sh` 的 ToolLoop、Session 和 Workspace 都没有错误；distinct states 分别是 54、688 和 640。`python3 scripts/test-workspace-rootfs.py` 的 11 个离线控制流 case 通过。`sh -n scripts/prepare-workspace-rootfs.sh` 通过。这 11 个 case 不是公开工件的加密验签，也不是沙箱端到端证明。

这一批交付了真实宿主工作区绑定，以及默认关闭、不启动进程的 bash/code 路由。真实 bwrap、独立 rootfs、私有写层、网络隔离和 apt 都还没有交付。公开工件的准备说明在 [workspace-rootfs](workspace-rootfs.md)。没有测生产环境、NapCat，也没有打真实 provider。

## 2026-11-07 离线 bundle 验证

在仓库根目录复现：

```
cargo fmt --all -- --check
cargo clippy -p fujiang-plugin-agent --all-targets --no-deps -- -D warnings
cargo test --workspace --offline
cargo llvm-cov -p fujiang-plugin-agent --summary-only
scripts/run-tlc.sh
python3 scripts/test-workspace-rootfs.py
python3 scripts/test-workspace-bundle.py
REQUIRE_GPGV=1 python3 scripts/test-workspace-bundle-gpgv.py
```

已测：`cargo fmt` 通过。与 CI 相同的 agent clippy（`-D warnings`）通过。`cargo test --workspace --offline` 248 项通过。`cargo llvm-cov -p fujiang-plugin-agent --summary-only` 行覆盖 87.04%，只证明 Rust。TLC：ToolLoop 54、Session 688、Workspace 640、Runner 17，无错误。已有 rootfs 准备脚本 11 个 case 通过，不是真实 crypto。11 项正式单元回归通过，包含无 writer FIFO subprocess 截止 3 秒、真实目录 FD、CLI 失败清理。本机 gpgv 9 条 skip。`REQUIRE_GPGV=1` 缺依赖时真实失败仍准确。不声称 CI 已通过。

上面 248 项、87.04% 和 Workspace 640 是更早一头的记录。不能写成当前 owner 头的通过。

## 2026-11-07 独立 owner 写层（当前头）

基线 `32427e4` 的整体 CI 已成功：2026-11-07 18:42:18 Asia/Shanghai，https://github.com/KazeMae/FujiangBot/actions/runs/42364745250 。这只证明该基线。当前 head 没有新的 CI 结论。

在仓库根目录实际跑完：

```
cargo fmt --all -- --check
cargo clippy -p fujiang-plugin-agent --all-targets --no-deps --offline -- -D warnings
cargo test --workspace --offline
cargo llvm-cov -p fujiang-plugin-agent --summary-only
scripts/run-tlc.sh
python3 scripts/test-workspace-bundle.py
python3 scripts/test-workspace-bundle-gpgv.py
python3 scripts/test-workspace-rootfs.py
scripts/check-file-length.sh
git diff --check
```

结果（日志在 `/workspace/agent-tools/fujiang-local-20261107/`）：

- `owner-clippy-final.log`：上述 clippy 通过。
- fmt `--check` 通过。
- `owner-workspace-final.log`：`cargo test --workspace --offline` 261 通过，其中 agent 179；0 失败，0 ignored。
- `owner-coverage-final.log`：`TOTAL` 行覆盖 90.76%（9299 行，859 未覆盖）。同一行的 88.55% 是 region，不是行覆盖。
- `owner-python-bundle-final.log`：24 通过。`owner-python-gpgv-final.log`：14 条因缺 gpgv 明确 SKIP。`owner-python-rootfs-final.log`：旧 stub 11 个控制流通过。SKIP 不是验签通过，stub 不是官方工件。
- `owner-tlc-acceptance.log`：六模型无错误。distinct states：ToolLoop 54、Session 688、Workspace 512、OwnerLayer 346、Runner 17、Bundle 16。
- 文件长度检查与 `git diff --check` 通过。

范围：真实 SQLite 上的 QQ 选择，以及 OS 复制、持久、并发、硬链接、符号链接、metadata、FIFO、清理和界。全局节点预算在预取分配前计入，含清理。ToolLoop 的模拟 LLM 与成功路径的 runner 回调是模拟。`owner-agent-second.log` 里，合成不完整 owner root 上原生 bwrap 对 `/bin/bash` 和 `/usr/bin/python3` 都是 exit 1、`execvp` No such file。这不是成功的 shell、Python 或 Ubuntu。

没有可用的生产已接受 base，provider 未因此打开。模型、配置、环境都不能启用。没有官方 rootfs、apt、网络、挂载或 FD 端到端。没有 NapCat 生产调用，没有从 CI 取回工件，没有宿主根绑定，没有 DNS 重试。`UnacceptedRootfs` 恒拒绝。`VerifiedBase` 只有 `cfg(test)` 工厂。运行时没有作为 Ubuntu 交付。
