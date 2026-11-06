# 用 TLC 检查 ToolLoop

在仓库根目录执行 `scripts/run-tlc.sh`。它检查 `spec/tla/ToolLoop.tla`。通过时，输出里有这一句。

```
Model checking completed. No error has been found.
```

模型写的是 `crates/plugins/agent/src/tools/mod.rs` 里 `run_tool_loop` 的完成、降级和轮次上限。这次不改这个函数的行为。

## 准备 Java

TLC 需要 Java 11 或更高版本。先看本机版本。

```
java -version
```

没有 `java` 时，在 Debian 或 Ubuntu 上安装 OpenJDK 21。

```
sudo apt-get install openjdk-21-jre-headless
```

其他系统安装该发行版的 OpenJDK 21。不需要 TLA+ Toolbox。

## 运行检查

在仓库根目录执行。

```
scripts/run-tlc.sh
```

第一次运行会下载 TLA+ 1.7.4 的 `tla2tools.jar`。文件放在 `$XDG_CACHE_HOME/fujiang/`。没有设置 `XDG_CACHE_HOME` 时，目录是 `~/.cache/fujiang/`。脚本用 sha1 `bee4a54f3ee3d4afc347c3240ec2d9e93b075104` 核对这个 jar。

已经有同一份 jar 时，把路径传给脚本。

```
TLA_TOOLS_JAR=/path/to/tla2tools.jar scripts/run-tlc.sh
```

脚本固定 `-workers 1`。TLC 1.7.4 在多个 worker 上检查活性时有过漏报。脚本还带 `-deadlock`。`complete` 和 `degraded` 没有下一步。这是模型停住，不是漏写的转移。

要自己调用 TLC 时，在仓库根目录执行。把 `tla2tools.jar` 换成你机器上的路径。

```
java -cp tla2tools.jar tlc2.TLC -workers 1 -deadlock -config spec/tla/ToolLoop.cfg spec/tla/ToolLoop.tla
```

## 通过时成立的命题

初始状态里，`cap` 取 0、1、2 或 3，`toolsEmpty` 取真或假。TLC 走完这些选择下的每一步。

- 没有工具时只发生 `EmptyComplete`。模型调用次数是 1，阶段是 `complete`，不会变成 `degraded`。
- 有工具时，`ToolOk` 要求 `used < cap`。成功执行的工具轮数不超过 `cap`。
- `ToolErr` 把阶段设成 `degraded`。阶段只有 `running`、`complete`、`degraded`。工具失败不会进入别的状态。
- `used` 已经达到 `cap`，模型仍要调用工具时，只发生 `CapHit`。阶段变成 `degraded`，`used` 不增加。
- 阶段变成 `degraded` 或 `complete` 之后保持不变。

模型不含语言模型的传输失败，也不含响应 JSON 的解码失败。实现里这两类返回 `Err(LlmError)`。调用方另回一句说明，不使用 `ToolReply::Degraded`。未知工具、参数不是 JSON，或 `ToolHub::invoke` 返回错误，才是 `Degraded`。

改 `run_tool_loop` 的完成、降级或轮次上限时，先改 `ToolLoop.tla`，再跑 `scripts/run-tlc.sh`。
