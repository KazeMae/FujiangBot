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

协调者在本机跑过下面这些命令。这一轮没有测生产环境、NapCat，也没有打真实 provider。字节上界不是精确 tokenizer 验收。工作区沙箱没有实现。图片在视觉 token 无法核定时 fail closed。交接摘要那一次模型调用的 usage 没有计入 `usage_daily`。

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
