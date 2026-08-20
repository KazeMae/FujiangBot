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

- Proxy `Context`、`isolate` / realm、装饰器 inject
- 同一插件多实例、服务依赖图 / epoch
- 热更 `fujiang-core` 本身（Rust cdylib 做不到安全跨 ABI）

插件作者面对的是显式的 `Plugin` trait 和 `BotContext`，不是魔术 Context。

存储也一样拆开：宿主主库给消息和内置业务；动态插件用 `open_plugin_db`，文件在 `data/plugin-data/<name>/`。不把第三方 schema 焊进 `fujiang.db`。

## 运行时对象

```
Dispatcher
  └─ PluginSlot { plugin: Arc<dyn Plugin>, scope: Arc<PluginScope> }

PluginHub
  └─ LoadedSo { path, Library, plugin: Arc<dyn Plugin>, … }
       enabled?  → 是否也在 Dispatcher 里
       disabled  → 仅动态名，持久化在 plugins.disabled
```

`replace`：对新实例 `on_start`，成功才 `on_stop` 旧的。`insert` 在名字已存在时是空操作；热重载必须走 `replace`。

ABI：改 `Plugin` / `BotContext` / 导出符号就 bump `PLUGIN_ABI`（当前 2）。
