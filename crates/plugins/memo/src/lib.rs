//! Full example of a hot-pluggable plugin.
//!
//! Demonstrates: PluginMeta, commands, plugin_config, private SQLite,
//! PluginScope background work, and prefix-aware command parsing.
//!
//! ```bash
//! cargo build -p fujiang-plugin-memo
//! cp target/debug/libfujiang_plugin_memo.dylib plugins/   # Linux: .so
//! ```
//!
//! Optional `config.toml`:
//!
//! ```toml
//! [plugins.configs.memo]
//! max_per_source = 50
//! ttl_days = 0
//! ```

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fujiang_core::{
    command, declare_plugin, BotContext, Event, Flow, MessageEvent, Plugin, PluginMeta,
    PluginScope, Service, Source,
};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::warn;

const HELP: &str = "示例插件：给当前群/私聊记备忘，数据在插件自己的 SQLite，不进主库。\n\
.memo              本说明\n\
.memo add <文本>   记一条\n\
.memo list         列出本对话的备忘\n\
.memo del <id>     删一条（id 来自 list）\n\
.memo count        本对话有多少条\n\
配置 [plugins.configs.memo]：max_per_source（默认 50）、ttl_days（0=不自动删）";

pub struct MemoPlugin {
    pool: Mutex<Option<SqlitePool>>,
}

impl Default for MemoPlugin {
    fn default() -> Self {
        Self {
            pool: Mutex::new(None),
        }
    }
}

declare_plugin!(MemoPlugin);

struct MemoCfg {
    max_per_source: i64,
    ttl_days: i64,
}

impl MemoCfg {
    fn from_value(v: &serde_json::Value) -> Self {
        Self {
            max_per_source: v
                .get("max_per_source")
                .and_then(|x| x.as_i64())
                .unwrap_or(50)
                .clamp(1, 500),
            ttl_days: v
                .get("ttl_days")
                .and_then(|x| x.as_i64())
                .unwrap_or(0)
                .max(0),
        }
    }
}

#[async_trait]
impl Plugin for MemoPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta::new("memo", "示例：每聊天一份备忘，私有 SQLite", self.commands())
    }

    fn name(&self) -> &'static str {
        "memo"
    }

    fn help(&self) -> &'static str {
        HELP
    }

    fn commands(&self) -> &'static [&'static str] {
        &[".memo"]
    }

    fn provides(&self) -> &'static [&'static str] {
        &["memo"]
    }

    async fn on_start(&self, ctx: &BotContext, scope: &PluginScope) -> anyhow::Result<()> {
        let pool = ctx.open_instance_db(scope).await?;
        ensure_schema(&pool).await?;

        *self.pool.lock().await = Some(pool.clone());

        let cfg = MemoCfg::from_value(&ctx.instance_config(scope).await);
        ctx.provide(
            scope,
            "memo",
            Arc::new(MemoService {
                pool: pool.clone(),
                max_per_source: cfg.max_per_source,
            }),
        )?;
        if cfg.ttl_days > 0 {
            let ttl = cfg.ttl_days;
            let stop = scope.stop_token();
            scope.spawn(async move {
                loop {
                    if stop.is_cancelled() {
                        break;
                    }
                    let cutoff = chrono_now() - ttl * 86400;
                    if let Err(e) = sqlx::query("DELETE FROM memos WHERE created_at < ?")
                        .bind(cutoff)
                        .execute(&pool)
                        .await
                    {
                        warn!(error = %e, "memo ttl cleanup");
                    }
                    tokio::select! {
                        _ = stop.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(3600)) => {}
                    }
                }
            });
        }
        Ok(())
    }

    async fn on_stop(&self) -> anyhow::Result<()> {
        if let Some(pool) = self.pool.lock().await.take() {
            pool.close().await;
        }
        Ok(())
    }

    async fn handle(
        &self,
        ctx: &BotContext,
        ev: &Event,
        scope: &PluginScope,
    ) -> anyhow::Result<Flow> {
        let Some(msg) = ev.as_message() else {
            return Ok(Flow::Continue);
        };
        let prefix = ctx.bot_config().await.command_prefix;
        let line = msg.command_line();
        let Some(rest) = command::strip_token(&line, ".memo", &prefix) else {
            return Ok(Flow::Continue);
        };
        let pool = {
            let g = self.pool.lock().await;
            g.clone()
                .ok_or_else(|| anyhow::anyhow!("memo db not open"))?
        };
        let cfg = MemoCfg::from_value(&ctx.instance_config(scope).await);
        dispatch_cmd(ctx, msg, &pool, &cfg, rest.trim()).await?;
        Ok(Flow::Stop)
    }
}

fn source_key(src: Source) -> (&'static str, i64) {
    match src {
        Source::Group { id } => ("group", id),
        Source::Friend { id } => ("friend", id),
    }
}

async fn ensure_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS memos (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_kind TEXT NOT NULL,
            source_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            body TEXT NOT NULL,
            created_at INTEGER NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS memos_source ON memos(source_kind, source_id, id)")
        .execute(pool)
        .await?;
    Ok(())
}

fn scope_from_args(args: &Value) -> anyhow::Result<(&'static str, i64)> {
    let kind = args.get("source_kind").and_then(Value::as_str);
    let id = args.get("source_id").and_then(Value::as_i64);
    if let (Some(kind), Some(id)) = (kind, id) {
        return kind_name(kind).map(|kind| (kind, id));
    }
    if let Some(session) = args.get("session").and_then(Value::as_str) {
        return scope_from_session(session);
    }
    anyhow::bail!("memo 需要 source_kind 与 source_id，或 session")
}

fn scope_from_session(session: &str) -> anyhow::Result<(&'static str, i64)> {
    let (kind, id) = session
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("memo 不认识 session `{session}`"))?;
    let id: i64 = id
        .parse()
        .map_err(|_| anyhow::anyhow!("memo 不认识 session `{session}`"))?;
    kind_name(kind).map(|kind| (kind, id))
}

fn kind_name(kind: &str) -> anyhow::Result<&'static str> {
    match kind {
        "group" => Ok("group"),
        "friend" | "private" => Ok("friend"),
        _ => anyhow::bail!("memo 不认识来源 `{kind}`"),
    }
}

struct MemoService {
    pool: SqlitePool,
    max_per_source: i64,
}

#[async_trait]
impl Service for MemoService {
    async fn call(&self, method: &str, args: Value) -> anyhow::Result<Value> {
        match method {
            "list" => {
                let (kind, sid) = scope_from_args(&args)?;
                let rows = select_memos(&self.pool, kind, sid).await?;
                Ok(Value::Array(
                    rows.into_iter()
                        .map(|(id, user_id, body)| json!({"id": id, "user_id": user_id, "body": body}))
                        .collect(),
                ))
            }
            "add" => {
                let (kind, sid) = scope_from_args(&args)?;
                let body = args
                    .get("body")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .trim();
                let user_id = args
                    .get("user_id")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0);
                match insert_memo(&self.pool, self.max_per_source, kind, sid, user_id, body).await?
                {
                    AddOutcome::Saved(id) => Ok(json!({"id": id})),
                    AddOutcome::Empty => anyhow::bail!("memo.add 需要 body"),
                    AddOutcome::Full { n, max } => {
                        anyhow::bail!("memo.add 已有 {n} 条，上限 {max}")
                    }
                }
            }
            other => anyhow::bail!("memo 没有方法 `{other}`（list / add）"),
        }
    }
}

enum AddOutcome {
    Saved(i64),
    Empty,
    Full { n: i64, max: i64 },
}

async fn select_memos(
    pool: &SqlitePool,
    kind: &str,
    sid: i64,
) -> anyhow::Result<Vec<(i64, i64, String)>> {
    let rows = sqlx::query_as(
        "SELECT id, user_id, body FROM memos
         WHERE source_kind = ? AND source_id = ?
         ORDER BY id DESC LIMIT 20",
    )
    .bind(kind)
    .bind(sid)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

async fn count_for(pool: &SqlitePool, kind: &str, sid: i64) -> anyhow::Result<i64> {
    let n =
        sqlx::query_scalar("SELECT COUNT(*) FROM memos WHERE source_kind = ? AND source_id = ?")
            .bind(kind)
            .bind(sid)
            .fetch_one(pool)
            .await?;
    Ok(n)
}

async fn insert_memo(
    pool: &SqlitePool,
    max_per_source: i64,
    kind: &str,
    sid: i64,
    user_id: i64,
    body: &str,
) -> anyhow::Result<AddOutcome> {
    if body.is_empty() {
        return Ok(AddOutcome::Empty);
    }
    let n = count_for(pool, kind, sid).await?;
    if n >= max_per_source {
        return Ok(AddOutcome::Full {
            n,
            max: max_per_source,
        });
    }
    let r = sqlx::query(
        "INSERT INTO memos(source_kind, source_id, user_id, body, created_at)
         VALUES(?, ?, ?, ?, ?)",
    )
    .bind(kind)
    .bind(sid)
    .bind(user_id)
    .bind(body)
    .bind(chrono_now())
    .execute(pool)
    .await?;
    Ok(AddOutcome::Saved(r.last_insert_rowid()))
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn dispatch_cmd(
    ctx: &BotContext,
    msg: &MessageEvent,
    pool: &SqlitePool,
    cfg: &MemoCfg,
    rest: &str,
) -> anyhow::Result<()> {
    let mut parts = rest.splitn(2, char::is_whitespace);
    let verb = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();
    match verb {
        "" | "help" | "-h" | "--help" => {
            let prefix = ctx.bot_config().await.command_prefix;
            ctx.reply_text(msg, command::rewrite_help(HELP, &[".memo"], &prefix))
                .await?;
        }
        "add" => add_memo(ctx, msg, pool, cfg, arg).await?,
        "list" => list_memos(ctx, msg, pool).await?,
        "del" | "rm" => del_memo(ctx, msg, pool, arg).await?,
        "count" => count_memos(ctx, msg, pool).await?,
        _ => {
            let prefix = ctx.bot_config().await.command_prefix;
            ctx.reply_text(msg, format!("不认识这个子命令，发 {prefix}memo 看说明"))
                .await?;
        }
    }
    Ok(())
}

async fn add_memo(
    ctx: &BotContext,
    msg: &MessageEvent,
    pool: &SqlitePool,
    cfg: &MemoCfg,
    body: &str,
) -> anyhow::Result<()> {
    let (kind, sid) = source_key(msg.source);
    match insert_memo(pool, cfg.max_per_source, kind, sid, msg.user_id(), body).await? {
        AddOutcome::Empty => {
            ctx.reply_text(msg, "格式：.memo add <文本>").await?;
        }
        AddOutcome::Full { n, max } => {
            ctx.reply_text(msg, format!("本对话已有 {n} 条，上限 {max}。先 .memo del"))
                .await?;
        }
        AddOutcome::Saved(id) => {
            ctx.reply_text(msg, format!("已记下 #{id}")).await?;
        }
    }
    Ok(())
}

async fn list_memos(ctx: &BotContext, msg: &MessageEvent, pool: &SqlitePool) -> anyhow::Result<()> {
    let (kind, sid) = source_key(msg.source);
    let rows = select_memos(pool, kind, sid).await?;
    if rows.is_empty() {
        ctx.reply_text(msg, "这个对话还没有备忘").await?;
        return Ok(());
    }
    let mut s = String::from("最近备忘：");
    for (id, uid, body) in rows {
        s.push_str(&format!("\n#{id} ({uid}) {body}"));
    }
    ctx.reply_text(msg, s).await?;
    Ok(())
}

async fn del_memo(
    ctx: &BotContext,
    msg: &MessageEvent,
    pool: &SqlitePool,
    arg: &str,
) -> anyhow::Result<()> {
    let Ok(id) = arg.parse::<i64>() else {
        ctx.reply_text(msg, "格式：.memo del <id>").await?;
        return Ok(());
    };
    let (kind, sid) = source_key(msg.source);
    let n = sqlx::query("DELETE FROM memos WHERE id = ? AND source_kind = ? AND source_id = ?")
        .bind(id)
        .bind(kind)
        .bind(sid)
        .execute(pool)
        .await?
        .rows_affected();
    ctx.reply_text(
        msg,
        if n == 0 {
            format!("没有 #{id}，或不是这个对话的")
        } else {
            format!("已删 #{id}")
        },
    )
    .await?;
    Ok(())
}

async fn count_memos(
    ctx: &BotContext,
    msg: &MessageEvent,
    pool: &SqlitePool,
) -> anyhow::Result<()> {
    let (kind, sid) = source_key(msg.source);
    let n = count_for(pool, kind, sid).await?;
    ctx.reply_text(msg, format!("这个对话有 {n} 条备忘"))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_respects_prefix() {
        assert_eq!(command::strip_token(".memo", ".memo", "."), Some(""));
        assert_eq!(
            command::strip_token(".memo add hi", ".memo", "."),
            Some("add hi")
        );
        assert_eq!(
            command::strip_token("!memo list", ".memo", "!"),
            Some("list")
        );
        assert_eq!(command::strip_token(".memo", ".memo", "!"), None);
        assert_eq!(command::strip_token(".memory", ".memo", "."), None);
    }

    #[test]
    fn config_defaults_and_clamp() {
        let d = MemoCfg::from_value(&serde_json::json!({}));
        assert_eq!(d.max_per_source, 50);
        assert_eq!(d.ttl_days, 0);
        let c = MemoCfg::from_value(&serde_json::json!({
            "max_per_source": 9999,
            "ttl_days": -3
        }));
        assert_eq!(c.max_per_source, 500);
        assert_eq!(c.ttl_days, 0);
    }

    #[test]
    fn plugin_provides_memo() {
        let plugin = MemoPlugin::default();
        assert_eq!(plugin.provides(), ["memo"]);
    }

    #[tokio::test]
    async fn service_lists_one_source_and_accepts_a_session_key() {
        let pool = test_pool().await;
        let svc = MemoService {
            pool: pool.clone(),
            max_per_source: 50,
        };
        let saved = svc
            .call(
                "add",
                json!({
                    "source_kind": "group",
                    "source_id": 7,
                    "user_id": 3,
                    "body": "交题"
                }),
            )
            .await
            .unwrap();
        let id = saved["id"].as_i64().unwrap();
        let rows = svc
            .call("list", json!({"session": "group:7"}))
            .await
            .unwrap();
        assert_eq!(rows, json!([{"id": id, "user_id": 3, "body": "交题"}]));
        let other = svc
            .call("list", json!({"session": "private:7"}))
            .await
            .unwrap();
        assert_eq!(other, json!([]));
        let again = svc
            .call(
                "add",
                json!({"session": "private:4", "user_id": 9, "body": "私聊"}),
            )
            .await
            .unwrap();
        let friend = svc
            .call("list", json!({"source_kind": "friend", "source_id": 4}))
            .await
            .unwrap();
        assert_eq!(friend[0]["id"], again["id"]);
        assert_eq!(friend[0]["user_id"], 9);
        assert_eq!(friend[0]["body"], "私聊");
    }

    #[tokio::test]
    async fn service_lists_newest_twenty_and_rejects_bad_calls() {
        let pool = test_pool().await;
        let svc = MemoService {
            pool,
            max_per_source: 1,
        };
        assert!(svc.call("list", json!({})).await.is_err());
        assert!(svc
            .call(
                "add",
                json!({"source_kind": "group", "source_id": 1, "body": "  "}),
            )
            .await
            .is_err());
        assert!(svc
            .call("add", json!({"session": "nope", "body": "x"}))
            .await
            .is_err());
        svc.call(
            "add",
            json!({"source_kind": "group", "source_id": 1, "body": "第一条"}),
        )
        .await
        .unwrap();
        assert!(svc
            .call(
                "add",
                json!({"source_kind": "group", "source_id": 1, "body": "第二条"}),
            )
            .await
            .is_err());
        assert!(svc.call("drop", json!({})).await.is_err());

        let wide = MemoService {
            pool: test_pool().await,
            max_per_source: 30,
        };
        for i in 0..21 {
            wide.call(
                "add",
                json!({"source_kind": "group", "source_id": 2, "body": format!("m{i}")}),
            )
            .await
            .unwrap();
        }
        let rows = wide
            .call("list", json!({"source_kind": "group", "source_id": 2}))
            .await
            .unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 20);
        assert_eq!(rows[0]["body"], "m20");
        assert!(rows.iter().all(|row| row["body"] != "m0"));
    }

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        ensure_schema(&pool).await.unwrap();
        pool
    }
}
