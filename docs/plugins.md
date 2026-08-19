# 插件约定与动态加载

内置插件（contest / rank / problem / fun / luck）和 `plugins/` 里的 `.so` / `.dylib` 走同一套 `Plugin` 接口。只依赖 `fujiang-core`（按需再加 `fujiang-store`），**禁止**依赖 `napcat-sdk`。事件已经是中间层类型，收发走 `BotContext`。

可运行模板：`crates/plugins/echo`。

## 必须实现什么

实现 `fujiang_core::Plugin`。`handle` 和 `name` / `help` 必填，其余有默认实现。动态 `.so` 还要 `#[derive(Default)]` 并调用 `declare_plugin!`。

| 方法 | 必填 | 作用 |
|---|---|---|
| `name()` | 是 | 唯一短名，如 `"echo"`。内置名不能用 so 覆盖 |
| `help()` | 是 | `.help` 和插件入口命令展示的说明 |
| `handle(ctx, ev, scope)` | 是 | 处理事件。`Flow::Stop` 截胡后面的插件；`Continue` 继续 |
| `meta()` | 否 | 名字、版本、简介、命令列表。默认用 `name()` + 空简介 |
| `commands()` | 否 | 命令清单。分发层按**首词精确匹配**交给这个插件 |
| `command_prefixes()` | 否 | 无空格入口，如 `.remind08:30`、`.添加猫` |
| `interest()` | 否 | 默认 `Commands`（只收自己的命令）。`Messages` 收全部消息；`All` 还收通知/请求 |
| `on_start(ctx, scope)` | 否 | 启动或热加载时。后台循环用 `scope.spawn` / `scope.sleep` |
| `on_stop()` | 否 | 卸载前额外清理。scope 会在这之后 `dispose`，不必自己 cancel |

宿主调用顺序：`on_start` → 多条 `handle` → `on_stop` → `scope.dispose()`。群白名单和 `.help` 在分发层先处理，插件里不必再做 ACL。后台任务必须 `scope.spawn`，卸载时才会被 cancel/abort。

分发：先把匹配 `commands()` / `command_prefixes()` 的插件叫一遍（`Stop` 则结束）。没截胡再把消息交给 `Interest::Messages` / `All`（命令主人不跑第二遍）。学话、来只这类非命令入口把 `interest()` 设成 `Messages`。非消息事件只给 `All`。

`handle` 里自己解析 `msg.command_line()`（已 `trim`）。前缀默认 `.`，以实际配置为准：`ctx.bot_config().await.command_prefix`。

```rust
use fujiang_core::{declare_plugin, BotContext, Event, Flow, Plugin, PluginMeta, PluginScope};

#[derive(Default)]
pub struct EchoPlugin;

declare_plugin!(EchoPlugin); // 只在 cdylib 热插时需要

#[async_trait::async_trait]
impl Plugin for EchoPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta::new("echo", "示例热插插件：.ping → pong", self.commands())
    }
    fn name(&self) -> &'static str {
        "echo"
    }
    fn help(&self) -> &'static str {
        ".ping     回复 pong"
    }
    fn commands(&self) -> &'static [&'static str] {
        &[".ping"]
    }
    async fn handle(&self, ctx: &BotContext, ev: &Event, _scope: &PluginScope) -> anyhow::Result<Flow> {
        let Some(msg) = ev.as_message() else {
            return Ok(Flow::Continue);
        };
        if msg.command_line() == ".ping" {
            ctx.reply_text(msg, "pong").await?;
            return Ok(Flow::Stop);
        }
        Ok(Flow::Continue)
    }
}
```

## 宿主提供的 API：`BotContext`

每个 `handle` / `on_start` 都能拿到 `&BotContext`。

### 发消息

```rust
ctx.reply_text(msg, "文本").await?;
ctx.send_text(msg.source, "文本").await?;
ctx.send_image(msg.source, "/abs/or/relative/path.jpg").await?;
ctx.send_segments(target, vec![
    Segment::text("hi "),
    Segment::At { target: AtTarget::User(qq) },
]).await?;
```

本地图会在适配器里转成 `file://` 绝对路径再交给 OneBot。

### 读配置

```rust
let cfg = ctx.bot_config().await;
cfg.command_prefix
cfg.groups / cfg.allow_private / cfg.allowed(source)
cfg.clist_username / clist_api_key / clist_limit
cfg.contest_update_minutes / rank_update_minutes / daily_reset_hour
cfg.fun_allow_mutate / fun_admins / cfg.is_fun_admin(qq) / fun_delete_files
```

热加载后这里读到的是新值。

动态插件自己的 JSON 配置：

```rust
let v = ctx.plugin_config(self.name()).await;
// 对应 config.toml 里 [plugins.configs.<name>]
```

管理页可以对每个动态插件改这份 JSON；保存后只重启这一个插件。内置插件仍用上面的 typed 字段。

### 协议（尽量走上面的封装）

```rust
let m = ctx.messenger().await;
m.send(source, &segs).await?;
m.get_message(id).await?;   // 回复的那条
m.get_image(file).await?;   // OneBot 缓存名，不要传 http URL
m.get_file(file_id).await?;
```

拉回复里的图用：

```rust
let bytes = ctx.fetch_media_bytes(media).await?;
```

`http(s)` 会直接下载，缓存 id 才走 `get_image`。

### HTTP

`ctx.http` 是 `reqwest::Client`，给 Clist / CF API 用。

### 存储 `ctx.store`

SQLite。图库根目录：`ctx.store.image_root`。

| 域 | 方法 |
|---|---|
| 键值 | `get_setting` / `set_setting` |
| 比赛 | `replace_contests` / `contests_by_oj` / `contests_all` / `contests_today` |
| 提醒 | `set_remind` / `delete_remind` / `remind_groups` |
| CF 用户 | `upsert_cf_user` / `delete_cf_user` / `cf_users` / `cf_users_by_year` / `cf_users_by_name` / `handle_exists` |
| 比赛榜 | `replace_standings` / `standings` |
| 每日一题 | `daily_for_date` / `save_daily` |
| 学话 | `learn_add` / `learn_list` / `learn_del` / `learn_random`（群里无条目会回落到 `group_id = NULL`） |
| 收藏 | `star_list` / `star_add` / `star_set` / `star_del` |
| 图库 | `tag_*` / `image_upsert` / `image_attach` / `image_detach` / `image_tags` / `random_by_tag` |
| 消息 | `insert_message`（分发层已自动记，插件一般不用） |

## 事件里能读到什么

```rust
let Some(msg) = ev.as_message() else { return Ok(Flow::Continue) };

msg.id / msg.time / msg.self_id
msg.user_id()              // 发送者 QQ
msg.source                 // Group { id } 或 Friend { id }
msg.source.group_id()      // 群号；私聊是 None
msg.command_line()         // 纯文本 trim
msg.raw_text / msg.segments
msg.reply_id()             // 回复了哪条
msg.first_image()          // 这条消息里第一张图
msg.sender.nickname / card
```

`Event` 还有 `Notice` / `Request` / `Meta`，目前内置插件基本只处理 `Message`。

`Segment`：`Text` / `At` / `Image` / `Reply` / `File` / `Face` / `Unknown`。

## 生命周期（热加载）

| 时机 | 调用 |
|---|---|
| 进程启动或热启用 | `on_start(ctx, scope)` |
| 消息 | `handle(ctx, ev, scope)`（同一把 scope） |
| 热禁用 / 卸载 / 进程退出 | `on_stop`，然后 `scope.dispose()` |

卸载时会先 `on_stop`，再 cancel/abort scope 里的任务，再等 in-flight `handle` 结束才 `dlclose`。

## 动态 .so

ABI 号 `fujiang_plugin_abi() == 2`（`PLUGIN_ABI`）。`.so` **必须用本仓库、同一套 rustc 编译**，不能跨版本乱拷。内置插件名不能用 so 覆盖。

```bash
cargo build -p fujiang-plugin-echo
mkdir -p plugins
# macOS
cp target/debug/libfujiang_plugin_echo.dylib plugins/
# Linux
# cp target/debug/libfujiang_plugin_echo.so plugins/
```

进程启动会扫描 `plugins.dir`（默认 `plugins/`）。`plugins.watch = true` 时每 2 秒看一次增删改。管理页也可以手动加载 / 卸载 / 重载。

导出符号（`declare_plugin!` 已生成）：

- `fujiang_plugin_abi() -> u32`
- `fujiang_create_plugin() -> *mut Box<dyn Plugin>`

`Cargo.toml`：

```toml
[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
anyhow.workspace = true
async-trait.workspace = true
fujiang-core = { path = "../../fujiang-core" }
```
