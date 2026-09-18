# 插件运行时取舍

完整用法见 [plugins.md](plugins.md)。这里只记「为什么长这样」以及从 Cordis 借了什么。

## 从 Cordis 借的五件事（已落地）

| 点 | Cordis | 福酱 |
|---|---|---|
| Effect | Fiber 上登记的 disposable | `PluginScope`：`spawn` / `sleep`，卸载 `dispose` |
| 订阅而不是广播 | `ctx.on('message')` | `commands()` + `Interest`；默认只收自己的命令 |
| 每插件配置 | `plugin.Config` + `fiber.update` | 内置 typed TOML；动态 `[plugins.configs.<name>]` |
| Loader 条目 | YAML 插件树 | 一张 Entry 表；停用 = dispose，不删文件 |
| HMR 回滚 | 新模块失败则恢复旧 cache | 先 `on_start` 新 `.so`，失败保留旧实例 |

## 故意没搬

- Proxy `Context`、`isolate` / realm、装饰器 `@Inject`
- isolate / realm 命名空间（多实例用显式 id，不是 isolate 符号）
- 热更 `fujiang-core` 本身（Rust cdylib 做不到安全跨 ABI）

服务图和事件总线是薄版：具名 JSON 调用 + `emit`/`parallel`/`serial`/`waterfall`，绑在 `PluginScope` 上。没有 epoch 字符串、没有跨插件 `Any` 强转。

插件作者面对的是显式的 `Plugin` trait 和 `BotContext`，不是魔术 Context。

存储也一样拆开：宿主主库只放消息；每个插件（含内置 contest / rank / problem / fun）用 `data/plugin-data/<name>/plugin.sqlite`。动态插件自己 `open_plugin_db` 建表。不把第三方 schema 焊进 `fujiang.db`。

## 运行时对象

```
Dispatcher
  └─ PluginSlot { plugin: Arc<dyn Plugin>, scope: Arc<PluginScope> }

PluginHub
  └─ LoadedSo { path, Library, plugin: Arc<dyn Plugin>, … }
       enabled?  → 是否也在 Dispatcher 里
       disabled  → 仅动态名，持久化在 plugins.disabled
```

`replace`：对新实例 `on_start`，成功才停旧的。`insert` 在名字已存在时是空操作；热重载必须走 `replace`。

停插件的顺序是 **先 `scope.dispose()`（cancel + 等后台任务退出，超时再 abort），再 `on_stop()`**，这样备忘录这类插件可以先停循环再关 SQLite。

`start_all` 不会因为一个插件 `on_start` 失败而退出进程：失败的插件被移出分发器，错误写进 Dispatcher / 管理页 `last_error`。

宿主还认每个插件 JSON 里的 `groups` / `allow_private` / `priority`（见 [plugins.md](plugins.md) §3、§7）。命令匹配必须走 `fujiang_core::command`，不要在 handle 里写死 `.luck`。

ABI：改 `Plugin` / `BotContext` 字段或导出符号就 bump `PLUGIN_ABI`（当前 5）。给 `BotContext` 加 inherent 方法、或只改 Dispatcher 行为，不必 bump。

多实例：Dispatcher 按 **instance id** 存槽；`Plugin::name()` 仍是类型。`PluginScope::instance_id()` / `plugin_kind()` / `is_primary()`。
