# 用 TLC 检查 ToolLoop、Session 和 Workspace

在仓库根目录执行 `scripts/run-tlc.sh`。它先检查 `spec/tla/ToolLoop.tla`，再检查 `spec/tla/Session.tla`，最后检查 `spec/tla/Workspace.tla`。通过时，每一段输出里都有这一句。

```
Model checking completed. No error has been found.
```

模型写的是 `crates/plugins/agent/src/tools/mod.rs` 里 `run_tool_loop` 的完成、降级、轮次上限，以及上下文预算在发请求前拒绝且不增加 provider 调用次数。

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
java -cp tla2tools.jar tlc2.TLC -workers 1 -deadlock -config spec/tla/Session.cfg spec/tla/Session.tla
```

## 通过时成立的命题

初始状态里，`cap` 取 0、1、2 或 3，`toolsEmpty` 取真或假。TLC 走完这些选择下的每一步。

- 没有工具时只发生 `EmptyComplete`。模型调用次数是 1，阶段是 `complete`，不会变成 `degraded`。
- 有工具时，`ToolOk` 要求 `used < cap`。成功执行的工具轮数不超过 `cap`。
- `ToolErr` 把阶段设成 `degraded`。阶段只有 `running`、`complete`、`degraded`。工具失败不会进入别的状态。
- `used` 已经达到 `cap`，模型仍要调用工具时，只发生 `CapHit`。阶段变成 `degraded`，`used` 不增加。
- 阶段变成 `degraded`、`complete` 或 `rejected` 之后保持不变。
- `overBudget` 为真时只发生 `RejectBudget`。阶段变成 `rejected`，`modelCalls` 仍是 0。这对应上下文未确认、超预算、图片没有核实上界，或图片张数和体积超限时，工具循环在发请求前返回，不把这次算成 provider 调用。已核实上界的图片仍是一次普通模型调用，不单独占状态。

模型不含语言模型的传输失败，也不含响应 JSON 的解码失败。实现里这两类返回 `Err(LlmError)`，并且会计一次 provider 调用。调用方另回一句说明，不使用 `ToolReply::Degraded`。未知工具、参数不是 JSON，或 `ToolHub::invoke` 返回错误，才是 `Degraded`。

改 `run_tool_loop` 的完成、降级或轮次上限时，先改 `ToolLoop.tla`，再跑 `scripts/run-tlc.sh`。

## Session

`Session.tla` 是有限抽象。`Window` 是 2。`messages` 从 0 到 4。`rev` 从 0 到 2，到顶后不再增加，所以单靠 revision 不能证明饱和之后的变更。真正拦住过期提交的是 `fresh` 和 `ticket`。`summary` 是真或假。`persona` 是 `default` 或 `strict`。`layered` 恒为真。一次只跟踪一个快照。`messages` 为 3 时可以 `snap`，随后 `speak` 加到 4，这次追加把 `fresh` 置假。模型不覆盖更长的历史。

`snap` 在空闲且消息数大于 `Window` 时把 `fresh` 设为真，`ticket` 设为 `open`。`Validate` 只在这时进入 `ready`，并把 `ticket` 设为 `checked`。它抽象「摘要非空、折叠后变小且不超过预算」，不改历史。`commit` 只在 `ready`、`fresh` 且 `ticket = checked` 时把消息收到 `Window`。它不比较当前的 revision、人设、摘要或消息数是否又等于快照。`reset`、`forget`、`speak`、`persona` 无条件把 `fresh` 设为假、`ticket` 设为 `blocked`，即使 revision 已经饱和、摘要本来就是假、或人设来回切换后看起来和快照一样。`fail` 可以从 `snapped` 或 `ready` 退出，不改历史。

`StaleCannotCommit` 要求 `blocked` 或 `fresh = FALSE` 的快照阶段里 `op` 不是 `commit`。`CommitOnlyAfterCheck` 要求成功提交的前一状态是 `ready` 且 `ticket` 曾是 `checked`。这不是把 `commit` 的守卫再写一遍：提交看的是票据，不是字段是否重新相等。

模型不含每小时次数，也不含真正的模型调用。用量记账失败归已有的 `Fail`（`op = "fail"`，也叫 CompactFail），不另建状态：不改 `messages` 和 `summary`。昵称、名片、引用 ID 这类 metadata 快照不进入 ToolLoop 或 Session，也不改变权限状态。改 `.agent reset`、`.agent forget` 或交接摘要时，先改 `Session.tla`，再跑 `scripts/run-tlc.sh`。

## Workspace

`Workspace.tla` 检查群里的本人选择只属于当前群和当前 actor。换 actor、换群、换私聊 peer，或在群和私聊之间切换时，先清掉已解析的绑定，再按当前事件重算。解析出的 `ownerId` 只能是当前 actor 或当前群。私聊在 peer 和 actor 不一致时拒绝。`payload` 保持 0。这个模型只覆盖路由，不证明沙箱、bwrap 或文件隔离。改 `.agent workspace` 的选择规则时，先改这个模型，再跑 `scripts/run-tlc.sh`。
