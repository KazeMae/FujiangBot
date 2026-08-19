# 插件生命周期 1–5

从 Cordis 借五件事，按里程碑落地。每步可独立提交、独立回滚。不引入 Proxy Context、isolate、服务图。

现状（本分支已有）：`Plugin` + `on_start`/`handle`/`on_stop`；`Dispatcher` 广播每条事件；`PluginHub` 热插 `.so`；卸载靠手写 `CancellationToken` 和 `Arc` 引用计数。

## M1 PluginScope

**目标：** 插件后台任务挂在作用域上，卸载时 cancel + abort，不再赌 `on_stop` 写全。

- 新增 `PluginScope`：`stop` token、`spawn`、`sleep`、`dispose`。
- `on_start(ctx, scope)`、`handle(ctx, ev, scope)` 都传入同一把 scope。
- `Dispatcher` 每个插件一个 `PluginSlot { plugin, scope }`。`insert` 先 `on_start`，失败则 `dispose`；`remove` / `stop_all` 先 `on_stop` 再 `dispose`。
- contest / rank 去掉自建 token，循环和 `pre_remind` 子任务、手动刷新都走 `scope.spawn`。
- `PLUGIN_ABI` 升到 2（trait 布局变了）。

不在本步做：命令路由、配置、Entry 表。

## M2 命令表真路由

**目标：** `commands()` 参与分发；学话 / 来只走显式订阅。

- `Interest::{Commands, Messages, All}`。默认 `Commands`。fun 声明 `Messages`。
- `command_prefixes()`：像 `.remind08:30`、`.添加xx` 这种没有空格的入口。
- 分发：先按「首词精确 → 最长前缀」找到命令主人并调用；`Stop` 则结束。未截胡再把消息交给 `Messages`/`All`（命令主人不跑第二遍）。非消息事件只给 `All`。
- `.help`、白名单、落库仍在分发层。

## M3 每插件 config

**目标：** 动态插件有配置通道；改一份配置只重启那一个插件。

- `BotContext::plugin_config(name) -> Value`。
- TOML `[plugins.configs.<name>]` 存动态插件 JSON 对象。内置仍用现有 typed 字段（`plugins.contest.update_minutes` 等），避免改配置文件格式。
- `GET /api/plugins` 每条带 `config`。`PUT /api/plugins/{name}/config` 校验后写入并 `restart` 该插件。
- 管理页：动态插件可编辑 JSON；内置继续用现有表单，保存时只重启变更项（沿用 `needs_restart`）。

## M4 Entry 清单

**目标：** 内置 + `.so` 合成一张表；disable = dispose，不删磁盘文件。

- 统一视图：`name / kind / path / enabled / state / config / commands`。
- `plugins.disabled = ["echo"]` 持久化关闭。扫到新 `.so` 默认启用。
- Hub 可以保留已打开的 `Library`；`enabled` 只决定是否在 Dispatcher 里。
- 管理页一张表：启用 / 停用（内置写回 `plugins.<name>.enabled`，动态写入 `disabled`）。

## M5 reload 回滚

**目标：** 新库 `on_start` 失败则旧实例继续跑。

- 先打开并 `on_start` 新实例（自带新 scope）。失败：`dispose` 新 scope、丢掉新 `Library`，Dispatcher 不动。
- 成功：再 `remove` 旧的、换入新的，等旧 `Arc` 唯一后 `dlclose`。
- Entry 上记录 `last_error`，管理页能看见 FAILED 原因。
