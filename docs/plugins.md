# 福酱插件说明

内置插件（contest / rank / problem / fun / luck）和 `plugins/` 里热插的 `.so` / `.dylib` / `.dll` 走同一套 `fujiang_core::Plugin`。业务只依赖中间层，**禁止**依赖 `napcat-sdk`。换 OneBot 实现端只加 adapter，插件不用改。

最小模板：`crates/plugins/echo`（`.ping` → pong）。
完整模板：`crates/plugins/memo`（自己的 SQLite、配置、后台清理）。约定与当前代码一致（`PLUGIN_ABI = 5`）。

| 读者 | 从哪读 |
|---|---|
| 写插件 | §3–§8、§10、§14 |
| 运维 / 改配置 | §9、§11、§12，以及仓库根 [README](../README.md) |
| 改宿主 | §2、§5、§11、§16；取舍见 [plugin-lifecycle.md](plugin-lifecycle.md) |

---

## 1. 系统怎么串起来

```
OneBot 实现端（NapCat / LLOneBot）
        │  事件
        ▼
adapter  →  fujiang-core::Event / Segment
        │
        ▼
Dispatcher
  ├─ 群白名单 / 私聊开关
  ├─ 落库（消息分表）
  ├─ `.help` 拼菜单
  └─ 按 commands / Interest 调用插件
        │
        ▼
Plugin（内置 crate 或动态 .so）
  只用 BotContext：发消息、读配置、HTTP、Store
```

运行时还有：

- **PluginScope**：每个运行中的插件一把。后台任务挂在上面，卸载时 cancel + abort。
- **PluginHub**：扫 `plugins.dir`，`dlopen` 动态库，启用/停用/重载。
- **管理页**（默认 `http://127.0.0.1:8787/`）：改全局配置、插件 Entry、动态库 JSON。

Crate 边界：

| crate | 职责 |
|---|---|
| `fujiang-core` | `Plugin` / `BotContext` / `Event` / `Dispatcher` / `PluginScope` |
| `fujiang-store` | 主库消息；各插件自己的 SQLite |
| `fujiang-adapter` | OneBot ↔ 中间层 |
| `fujiang` | 进程、配置、管理页、PluginHub |
| `napcat-sdk` | 仅 adapter 使用 |
| `crates/plugins/*` | 具体插件。内置链进 `fujiang`；echo / memo 额外编成 cdylib |

---

## 2. 必须实现什么

实现 `fujiang_core::Plugin`。`name` / `help` / `handle` 必填，其余有默认实现。动态 `.so` 还要 `#[derive(Default)]` 并调用 `declare_plugin!`。

| 方法 | 必填 | 作用 |
|---|---|---|
| `name()` | 是 | 唯一短名，如 `"echo"`。不能和内置名冲突 |
| `help()` | 是 | `.help` 和插件入口命令展示的说明 |
| `handle(ctx, ev, scope)` | 是 | 处理事件。`Flow::Stop` 截胡后面的插件；`Continue` 继续 |
| `meta()` | 否 | 名字、版本、简介、命令。默认 `name()` + 空简介，版本用 `CARGO_PKG_VERSION` |
| `commands()` | 否 | 命令清单。分发层按**首词精确匹配**交给这个插件 |
| `command_prefixes()` | 否 | 无空格入口，如 `.remind08:30`、`.添加猫` |
| `interest()` | 否 | 默认 `Commands`。`Messages` 收全部消息；`All` 还收通知/请求 |
| `on_start(ctx, scope)` | 否 | 启动或热启用。后台循环必须 `scope.spawn` |
| `on_stop()` | 否 | 卸载时在 `dispose` 之后调用。关库、释放资源放这里 |

宿主顺序：

```
on_start(ctx, scope)
    → 多条 handle(ctx, ev, 同一把 scope)
    → scope.dispose()     // cancel，等后台任务最多 ~400ms，超时 abort
    → on_stop()           // 这时再关 SQLite / 释放资源
    → （动态库）等 Arc 唯一后 dlclose
```

`on_start` 失败：这把新 scope 立刻 `dispose`，插件不会进分发器；**其它插件照常跑**，管理页 `last_error` 会显示原因。进程不会因为一个插件启动失败而退出。

群白名单和 `.help` 在分发层先处理，插件不必再做 ACL。指令前缀以配置为准：`ctx.bot_config().await.command_prefix`（默认 `.`）。`commands()` 里仍写成 `.ping` 这种点号形式，分发时会换成实际前缀。

---

## 3. 分发规则

对每条**已通过白名单**的消息：

1. 落库。
2. 整行等于 `{prefix}help` → 总菜单；`{prefix}help 名字` → 该插件说明。不再交给插件。
3. **命令主人**：先看各插件 `commands()` 是否与**第一个空白分隔词**相等；没有再在 `command_prefixes()` 里取**最长前缀**。命中则先调用该插件。
4. `Flow::Stop` → 结束。`Continue` 或没有主人 → 再把消息交给 `Interest::Messages` 和 `Interest::All`（命令主人不跑第二遍）。
5. 非消息事件（`Notice` / `Request` / `Meta`）只给 `Interest::All`。目前没有内置插件声明 `All`。

因此：

- 只响应自己命令的插件保持默认 `Commands`（contest / rank / problem / luck / echo）。
- 还要接「来只」「学话」这种非命令文本的，声明 `Messages`（fun）。
- `.remind08:30` 这类没有空格的入口，除了可写进 `commands()` 外，用 `command_prefixes()`（contest 的 `.remind`，fun 的 `.添加` / `.删除`）。

路由前会：

- 按插件 JSON 的 `priority`（默认 0，越大越先）排序；命令碰撞时也是高优先级赢，并打 warn。
- 若配置了 `groups`，该插件只看到这些群；`allow_private = false` 则不处理私聊。
- `{prefix}help` 出总菜单；`{prefix}help 插件名` 只出那一项（说明里的 `.cmd` 会换成当前前缀）。

`Flow`：插件内部处理完应 `Stop`，避免后面的 `Messages` 插件再吃同一条。学话命中后 fun 返回 `Continue`，是有意让其它逻辑仍能看到这条。

### 服务图与事件总线

跨 `.so` 只走 JSON，不要 `Any` downcast。ABI 现在是 **5**。旧动态库要重编。

### 多实例

同一插件可以跑多份。默认实例的 id 等于类型名（`rank`）。额外实例写：

```toml
[[plugins.instances]]
id = "rank-fresh"
plugin = "rank"

[plugins.configs.rank-fresh]
groups = [555973858]
share_data = true
```

管理页「再开一份」。命令声明相同，靠 `groups` 分流；同一群里两份则 `priority` 高的先处理。

- 默认实例 `ctx.provide(scope, "rank", …)` 注册服务名 `rank`
- 额外实例注册 `rank#rank-fresh`
- 动态插件（memo）默认每实例一份 sqlite（`plugin-data/memo__rank-fresh/` 这种 key）；`share_data = true` 则和类型共用
- 内置 contest/rank/fun 的 Store API 仍按类型共用数据；多实例主要用来分群
- `.help rank-fresh` 看那一份；`.help rank` 在只有一份时也可以

插件读配置用 `ctx.instance_config(scope)`，开库用 `ctx.open_instance_db(scope)`。

```rust
fn inject(&self) -> &'static [&'static str] { &["rank"] }     // 没有这些服务就先 pending
fn provides(&self) -> &'static [&'static str] { &["memo"] }   // 声明会 provide 的名字

async fn on_start(&self, ctx: &BotContext, scope: &PluginScope) -> anyhow::Result<()> {
    ctx.provide(scope, "memo", Arc::new(MemoSvc { /* ... */ }))?;
    ctx.listen_fn(scope, "rank.updated", |payload| async move {
        Ok(fujiang_core::EventResult::Continue(None))
    });
    Ok(())
}

// 别处
ctx.call("rank", "list", json!({})).await?;
ctx.emit("memo.changed", json!({ "id": 1 }));
```

- `provide` / `listen` 随 `scope.dispose` 自动撤销。热重载同一 owner 替换服务时，旧 generation 的 revoke 是空操作。
- `inject` 未满足 → 插件进 pending，管理页状态 `pending`；提供者起来后自动 `on_start`。提供者被停 → 依赖者 demote 回 pending。
- `inject` 成环 → 启动失败，不挂死。
- JSON 里也可写 `"inject": ["rank"]`、`"provides": ["foo"]`，和 trait 声明合并。
- 事件：`emit`（不等待）/ `events.parallel` / `serial`（`Stop` 中断）/ `waterfall`（`Continue(Some(v))` 改 payload）。
- 内置：`rank` 服务 `list` / `get`；`contest` 服务 `upcoming` / `today`。刷新后发 `rank.updated`、`contest.updated`。宿主还会发 `plugin.started` / `plugin.stopped`。

handle 里**不要**写死 `.luck` / `.ping`。声明仍用点号，匹配用 `fujiang_core::command`：

```rust
let prefix = ctx.bot_config().await.command_prefix;
if let Some(rest) = command::strip_token(&msg.command_line(), ".luck", &prefix) {
    // rest 是参数
}
if command::matches_token(&line, ".ping", &prefix) { /* ... */ }
if let Some(tag) = command::strip_prefix_cmd(&line, ".添加", &prefix) { /* 来只那种粘连命令 */ }
ctx.reply_text(msg, command::plugin_help(self, &prefix)).await?;
```

---

## 4. 生命周期与 `PluginScope`

每个在 Dispatcher 里的插件对应一个 `PluginSlot { plugin, scope }`。

```rust
scope.spawn(async move { /* 后台循环 */ });
scope.sleep(Duration::from_secs(60)).await;   // dispose 时立刻返回
if scope.is_cancelled() { return; }
let stop = scope.stop_token();                // 可传入更里层的 select
```

规则：

- **不要** `tokio::spawn` 脱离 scope 的任务。卸载时那些任务不会停，动态库也可能卸不干净。
- `handle` 里临时刷新（如 `.rank -urk`）同样 `scope.spawn`。
- 热重载 `.so`：先对新库 `on_start`。失败则丢掉新库，**旧实例继续跑**，管理页 `last_error` 会显示原因。成功才停旧的。

进程退出：`stop_all` → 每个插件 `on_stop` + `dispose`，再停 gateway。

---

## 5. 宿主 API：`BotContext`

`handle` / `on_start` 都能拿到 `&BotContext`（可 `clone`，内部是 `Arc`）。

### 5.1 发消息

```rust
ctx.reply_text(msg, "文本").await?;
ctx.send_text(msg.source, "文本").await?;
ctx.send_image(msg.source, "/abs/or/relative/path.jpg").await?;
ctx.send_video(msg.source, "/abs/or/relative/path.mp4").await?;
ctx.send_segments(target, vec![
    Segment::text("hi "),
    Segment::At { target: AtTarget::User(qq) },
]).await?;
```

本地路径在 adapter 里转成 `file://` 绝对 URI 再交给 OneBot。相对路径按进程 cwd 解析。

### 5.2 读配置

全局（热加载后立即是新值）：

```rust
let cfg = ctx.bot_config().await;
cfg.command_prefix
cfg.groups / cfg.allow_private / cfg.allowed(source)
cfg.clist_username / clist_api_key / clist_limit
cfg.contest_update_minutes / rank_update_minutes / daily_reset_hour
cfg.fun_allow_mutate / fun_admins / cfg.is_fun_admin(qq) / fun_delete_files
```

本插件的 JSON（动态插件用这个；内置仍读上面的 typed 字段）：

```rust
let v = ctx.plugin_config(self.name()).await;
// 对应 config.toml 的 [plugins.configs.<name>]
let prefix = v.get("prefix").and_then(|x| x.as_str()).unwrap_or(".");
```

管理页保存动态插件 JSON 后，只重启这一个插件（`on_stop` → 新 scope `on_start`）。

### 5.3 插件自己的数据库

**主库 `fujiang.db` 只给宿主。** 消息分表、归档参数、`settings` 里宿主自己的键（如 `messages_archived_at`）在这里。

**每个插件一份 SQLite。** 路径：`{data}/plugin-data/{name}/plugin.sqlite`。内置插件的表也在各自的文件里：

| 插件 | 文件 | 内容 |
|---|---|---|
| contest | `plugin-data/contest/plugin.sqlite` | 比赛日历、群提醒、`contest_*` / `preremind:*` |
| rank | `plugin-data/rank/plugin.sqlite` | CF 用户、比赛榜、`rank_*` |
| problem | `plugin-data/problem/plugin.sqlite` | 每日一题 |
| fun | `plugin-data/fun/plugin.sqlite` | 学话、收藏、图库元数据（图片/视频文件仍在 `image_root`） |
| memo 等动态插件 | `plugin-data/<name>/plugin.sqlite` | 自己的 schema |

从旧主库升级时，启动会把上述表拷进插件库再从 `fujiang.db` 删掉，只做一次。

动态插件在 `on_start` 里自己 `CREATE TABLE IF NOT EXISTS`（不要用宿主的 `sqlx::migrate!`）：

```rust
// 目录：{data}/plugin-data/{name}/
let dir = ctx.plugin_data_dir(self.name())?;
// 文件：{dir}/plugin.sqlite
let pool = ctx.open_plugin_db(self.name()).await?;
```

`name` 只能是 `[A-Za-z0-9_-]`。停用 / 卸载插件**不会**删这个目录，避免误删用户数据。要清数据就手动删 `data/plugin-data/<name>/`。

完整例子见 `crates/plugins/memo`。

### 5.4 协议层（尽量走 5.1 的封装）

```rust
let m = ctx.messenger().await;
m.send(source, &segs).await?;
m.get_message(id).await?;   // 被回复的那条
m.get_image(file).await?;   // OneBot 缓存文件名，不要传 http URL
m.get_file(file_id).await?;
```

拉回复里的图：

```rust
let bytes = ctx.fetch_media_bytes(media).await?;
```

`http(s)` 直接下载；本地 `Path` 读文件；缓存 id / 非 http 的 `Url` 才走 `get_image`。

### 5.5 HTTP

`ctx.http` 是 `reqwest::Client`（UA `fujiang-bot/0.1`），给 Clist / Codeforces 等用。

### 5.6 存储 `ctx.store`

宿主主库只放消息。内置插件的方法仍挂在 `Store` 上，但读写的是 `plugin-data/<name>/plugin.sqlite`。图文件根目录：`ctx.store.image_root`。

| 域 | 方法 | 库 |
|---|---|---|
| 宿主键值 | `get_setting` / `set_setting` | 主库 |
| 插件键值 | `plugin_get` / `plugin_set` | 对应插件库 |
| 比赛 | `replace_contests` / `contests_by_oj` / `contests_all` / `contests_today` | contest |
| 提醒 | `set_remind` / `delete_remind` / `remind_groups` | contest |
| CF 用户 | `upsert_cf_user` / `delete_cf_user` / `cf_users` / `cf_users_by_year` / `cf_users_by_name` / `handle_exists` | rank |
| 比赛榜 | `replace_standings` / `upsert_standings` / `standings` | rank |
| 每日一题 | `daily_for_date` / `save_daily` | problem |
| 学话 | `learn_add` / `learn_list` / `learn_del` / `learn_random`（本群没有则回落到 `group_id IS NULL`） | fun |
| 收藏 | `star_list` / `star_add` / `star_set` / `star_del` | fun |
| 图库 | `tag_ensure` / `tag_by_name` / `tag_by_alias` / `tag_list` / `tag_alias` / `tag_merge` / `tag_retire` / `image_upsert` / `image_attach` / `image_detach` / `image_tags` / `random_by_tag` | fun（文件在 `image_root`） |
| 消息 | `insert_message`（分发层已自动记，插件一般不用） | 主库 |

学话范围：写入时带当前 `group_id`（私聊为 `NULL`）。群里触发先查本群，没有再用全局（导入的旧 `learn.json` 是 `NULL`）。

图库是「一条媒体（图或视频）多个 tag」，不是旧版专辑。摘掉最后一个 tag 也不删文件。`来只<tag>` 从该 tag 下随机抽一张图或一条视频。

---

## 6. 事件

```rust
let Some(msg) = ev.as_message() else {
    return Ok(Flow::Continue);
};

msg.id / msg.time / msg.self_id
msg.user_id()              // 发送者 QQ
msg.source                 // Group { id } 或 Friend { id }
msg.source.id()            // 群号或 QQ
msg.source.is_group()
msg.source.group_id()      // 群号；私聊 None
msg.command_line()         // raw_text.trim()
msg.raw_text / msg.segments
msg.reply_id()             // 回复了哪条
msg.first_image()          // 这条里第一张图
msg.first_video()          // 这条里第一条视频
msg.first_visual()         // 图或视频，`(media, is_video)`
msg.sender.nickname / card
```

| `Event` | 字段 |
|---|---|
| `Message` | 见上 |
| `Notice` | `kind` / `group_id` / `user_id` / `raw` |
| `Request` | `kind` / `flag` / `user_id` / `group_id` / `comment` |
| `Meta { kind }` | 心跳等 |

`Segment`：`Text` / `At { target: User(qq) \| All }` / `Image { src, summary }` / `Video { src, name }` / `Reply { id }` / `File { src, name }` / `Face { id }` / `Unknown { ty, raw }`。

`Media`：`Url` / `Path` / `Base64` / `FileId`。

---

## 7. 配置

`config.toml`（环境变量 `FUJIANG_*`，嵌套用 `__`）。密钥也可用 `NAPCAT_ACCESS_TOKEN`、`FUJIANG_CLIST_KEY`。

### 7.1 插件相关

```toml
[plugins]
dir = "plugins"          # 动态库目录
watch = true             # 每 2 秒扫增删改
disabled = []            # 动态插件：库可留在内存，不进分发器

[plugins.contest]
enabled = true
update_minutes = 60

[plugins.rank]
enabled = true
update_minutes = 60

[plugins.problem]
enabled = true
daily_reset_hour = 4     # 0..=23，每日一题换天的小时

[plugins.fun]
enabled = true
allow_mutate = true
admins = []              # 空 = 谁都能改学话/图库
delete_files_on_album_del = false

[plugins.luck]
enabled = true

# 动态插件自己的对象。管理页 JSON 编辑器写的就是这里
# [plugins.configs.echo]
# prefix = "."
#
# 宿主认的字段，内置 / 动态都能写（内置 typed 项仍走上面的表）：
# [plugins.configs.luck]
# groups = [741798363]   # 空或不写 = 所有已放行的群
# allow_private = true
# priority = 0
```

内置开关是 `plugins.<name>.enabled`。动态插件的停用名单是 `plugins.disabled`。两者不要混用。

整份配置保存时，若请求里没带 `plugins.configs` / `plugins.disabled`，宿主会保留文件里的旧值，避免管理页表单把它们抹掉。

### 7.2 热加载范围（全局保存）

| 改什么 | 立刻生效？ |
|---|---|
| 命令前缀、群白名单、Clist、fun 权限、归档天数/周期 | 是 |
| 内置插件开关；contest/rank 刷新间隔 | 是（关则 dispose；改间隔会重启该插件） |
| 单插件 JSON `PUT /api/plugins/{name}/config` | 是，只重启这一个 |
| 启用/停用 Entry | 是（dispose 或 insert） |
| adapter backend / 地址 / token | 是（重启 gateway） |
| `store.db` / `image_root` / `archive_dir` / `admin.listen` / `plugins.dir` / `plugins.watch` | 只写入文件，需重启进程 |

保存会重写整个 TOML，原注释会丢掉。密钥框留空或 `********` 表示不改；`__clear__` 或页面「清空」才擦掉。

---

## 8. 动态 `.so`

### 8.1 构建

```bash
cargo build -p fujiang-plugin-echo
mkdir -p plugins
# macOS
cp target/debug/libfujiang_plugin_echo.dylib plugins/
# Linux
# cp target/debug/libfujiang_plugin_echo.so plugins/
# Windows
# copy target\debug\fujiang_plugin_echo.dll plugins\
```

进程启动扫描 `plugins.dir`。`watch = true` 时每 2 秒看一次 mtime：新文件加载，消失则卸载，内容变了则重载。

### 8.2 Cargo.toml

```toml
[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
anyhow.workspace = true
async-trait.workspace = true
fujiang-core = { path = "../../fujiang-core" }
# 需要存数据再加：
# fujiang-store = { path = "../../fujiang-store" }
```

`declare_plugin!` 导出：

- `fujiang_plugin_abi() -> u32` 必须等于宿主的 `PLUGIN_ABI`（现在是 **3**）
- `fujiang_create_plugin() -> *mut Box<dyn Plugin>` 薄指针，类型必须 `Default`

### 8.3 硬限制

- **必须用本仓库、同一套 rustc 编译**，不能从别的机器或旧 commit 拷 `.so`。改 `Plugin` / `BotContext` / 导出符号会 bump ABI，旧库会加载失败。当前 `PLUGIN_ABI = 5`。
- 内置名 `contest` / `rank` / `problem` / `fun` / `luck` 不能被 so 覆盖。
- 不能热更 `fujiang-core` 本身，只能热插同 ABI 的插件。
- `plugins/*.so` 等已 gitignore，不要把编好的库提交进仓库。

---

## 9. Entry：启用、停用、重载

管理页和 `GET /api/plugins` 把内置 + 动态合成一张表。

| 字段 | 含义 |
|---|---|
| `name` | 插件短名 |
| `kind` | `builtin` 或 `dynamic` |
| `path` | 动态库路径；内置为 null |
| `enabled` | 是否在 Dispatcher 里跑 |
| `state` | `active` 运行中；内置关掉是 `disabled`；动态关掉但库还在是 `loaded` |
| `config` | 该插件当前配置对象 |
| `commands` | `meta().commands` |
| `last_error` | 最近一次加载/重载失败原因（仅动态） |

语义：

| 操作 | 内置 | 动态 |
|---|---|---|
| 停用 | `plugins.<name>.enabled = false`，dispose | 名字写入 `plugins.disabled`，dispose，**Library 仍打开** |
| 启用 | `enabled = true`，`make` + `on_start` | 从 disabled 去掉，已打开的实例 `insert` |
| 卸载 | 没有这个按钮 | dispose + `dlclose`，文件还在磁盘 |
| 重载 | 改间隔等走全局保存 | 先 `on_start` 新库；失败保留旧实例 |
| 扫到新 `.so` | — | 默认启用；若名字已在 `disabled` 里则只加载不启动 |

停用 ≠ 删文件。从目录拿走 `.so` 才会从 Hub 卸掉。

---

## 10. 管理页 HTTP API

默认 `GET http://127.0.0.1:8787/`。`[admin].token` 非空时，除页面本身外所有 `/api/*` 要带请求头 `X-Admin-Token` 或 `Authorization: Bearer …`。

| 方法 | 路径 | 作用 |
|---|---|---|
| `GET` | `/api/config` | 脱敏后的整份配置 + 状态。密钥为 `********` |
| `PUT` | `/api/config` | 保存并热加载。body 为 `AppConfig` JSON |
| `GET` | `/api/status` | 运行中插件名、backend、配置路径 |
| `GET` | `/api/plugins` | Entry 表（`entries`；也保留 `builtin` / `dynamic`） |
| `POST` | `/api/plugins/scan` | 立即扫目录 |
| `POST` | `/api/plugins/load` | `{ "path": "plugins/lib….dylib" }` |
| `POST` | `/api/plugins/unload` | `{ "name": "echo" }` 卸库 |
| `POST` | `/api/plugins/reload` | `{ "name": "echo" }` |
| `PUT` | `/api/plugins/{name}/config` | `{ "config": { … } }` 写配置并重启该插件 |
| `POST` | `/api/plugins/{name}/enable` | 启用 |
| `POST` | `/api/plugins/{name}/disable` | 停用（dispose，动态库保留） |

`GET /api/plugins` 形状（节选）：

```json
{
  "dir": "plugins",
  "watch": true,
  "disabled": ["echo"],
  "entries": [
    {
      "name": "contest",
      "kind": "builtin",
      "enabled": true,
      "state": "active",
      "path": null,
      "config": { "enabled": true, "update_minutes": 60 },
      "commands": [".contest", ".cf"]
    },
    {
      "name": "echo",
      "kind": "dynamic",
      "enabled": false,
      "state": "loaded",
      "path": "…/plugins/libfujiang_plugin_echo.dylib",
      "config": { "prefix": "." },
      "last_error": null
    }
  ]
}
```

管理页 `listen` 不要和 `adapter.event_listen` 相同。非本机监听且没有 token 时日志会警告。

---

## 11. 内置插件

群里 `.help` 拼的是各插件 `help()`。前缀默认 `.`，区分大小写。命令细节以 README 和各插件 `help()` 为准。

| 名字 | Interest | 额外前缀 | 后台任务 | 配置 |
|---|---|---|---|---|
| contest | Commands | `.remind` | 拉比赛、开赛前提醒、每日提醒 | `update_minutes` |
| rank | Commands | — | 刷 rating / 榜；`.rank -urk/-urt` 也走 scope | `update_minutes` |
| problem | Commands | — | 无 | `daily_reset_hour` |
| fun | **Messages** | `.添加` `.删除` | 无 | `allow_mutate` / `admins` / `delete_files_on_album_del` |
| luck | Commands | — | 无 | 仅开关 |

fun 仍声明 `.learn` `.star` `.tag` 等命令，这些走命令主人；「来只」「学话触发」走 `Messages`。

---

## 12. 示例

最小：`crates/plugins/echo`（`.ping` → pong）。

完整：`crates/plugins/memo`，覆盖这些点：

| 能力 | memo 怎么做 |
|---|---|
| 命令 | `.memo` / `add` / `list` / `del` / `count`，按实际 `command_prefix` 匹配 |
| 私有库 | `ctx.open_plugin_db("memo")` → `data/plugin-data/memo/plugin.sqlite` |
| schema | `on_start` 里 `CREATE TABLE IF NOT EXISTS` |
| 配置 | `ctx.plugin_config("memo")`：`max_per_source`、`ttl_days` |
| 后台 | `ttl_days > 0` 时 `scope.spawn` 每小时删过期行 |
| 卸载 | `on_stop` 关闭 pool；数据文件保留 |

```bash
cargo build -p fujiang-plugin-memo
cp target/debug/libfujiang_plugin_memo.dylib plugins/   # Linux 用 .so
```

```toml
[plugins.configs.memo]
max_per_source = 50
ttl_days = 0
```

群里 `.memo add 明天交题`，再 `.memo list`。数据不进 `fujiang.db`。

---

## 13. 限制与常见问题

| 现象 | 原因 / 处理 |
|---|---|
| 加载报 ABI 不符 | 用当前仓库重编 `.so`。ABI 现在是 5 |
| 改了 `command_prefix` 命令没反应 | handle 里写死了 `.xxx`。改用 `command::strip_token` / `matches_token` |
| 某个插件启动失败，其它还在 | 正常。看管理页 `last_error` 或日志 |
| `.help luck` 没有这个插件 | 插件没在跑（关掉了或启动失败） |
| 重载失败但机器人还在响应旧命令 | 正常。新库 `on_start` 失败会回滚，看 Entry 的 `last_error` |
| 停用后文件还在、watch 又启用了 | 名字在 `plugins.disabled` 里就不会自动启动 |
| 卸载很慢或警告 still in use | 还有 `handle` 没返回，或任务没走 `scope.spawn` |
| 发图 `识别URL失败` | 传相对路径时 adapter 会转绝对 `file://`；不要自己拼半截 URL |
| `get_image` file not found | 把 http URL 传给了 `get_image`。用 `fetch_media_bytes` |
| 「活着？」不学话 | 导入词库是全局 `group_id NULL`，已做回落；确认 fun 已启用且 `interest` 为 Messages |
| 想在主库建插件表 | 不要。用 `ctx.open_plugin_db(self.name())`，见 memo |
| 指令区分大小写 | `.Help` 不是 `.help` |
| 管理页保存丢注释 | TOML 整文件重写，注释不会保留 |

密钥（Clist、NapCat token、管理口令、任何 LLM key）只允许本机 `config.toml` 或环境变量，禁止进 git。`config.toml` 已 gitignore。

---

## 14. 源码地图

| 路径 | 内容 |
|---|---|
| `crates/fujiang-core/src/services.rs` | 具名服务图 `ServiceHub` |
| `crates/fujiang-core/src/events.rs` | 事件总线 `emit` / `parallel` / `serial` / `waterfall` |
| `crates/fujiang-core/src/command.rs` | 前缀感知的命令匹配、`plugin_help`、`groups`/`priority` ACL |
| `crates/fujiang-core/src/plugin.rs` | `Plugin` / `BotContext` / `Interest` / `PLUGIN_ABI` / `declare_plugin!` |
| `crates/fujiang-core/src/scope.rs` | `PluginScope` |
| `crates/fujiang-core/src/dispatch.rs` | 路由、帮助、insert/remove/replace |
| `crates/fujiang-core/src/event.rs` | `Event` / `Segment` / `Media` |
| `crates/fujiang/src/dynload.rs` | PluginHub、扫目录、回滚重载 |
| `crates/fujiang/src/plugins.rs` | 内置工厂、typed 配置 ↔ JSON |
| `crates/fujiang/src/admin.rs` | 管理页 API |
| `crates/fujiang/web/index.html` | 配置页 |
| `crates/plugins/echo` | 最小 cdylib |
| `crates/plugins/memo` | 完整 cdylib：私有库 + 配置 + 后台清理 |
| `config.example.toml` | 配置样例 |
