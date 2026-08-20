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

use std::time::Duration;

use async_trait::async_trait;
use fujiang_core::{
    declare_plugin, BotContext, Event, Flow, MessageEvent, Plugin, PluginMeta, PluginScope, Source,
};
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

    async fn on_start(&self, ctx: &BotContext, scope: &PluginScope) -> anyhow::Result<()> {
        let pool = ctx.open_plugin_db(self.name()).await?;
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
        .execute(&pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS memos_source ON memos(source_kind, source_id, id)")
            .execute(&pool)
            .await?;

        *self.pool.lock().await = Some(pool.clone());

        let cfg = MemoCfg::from_value(&ctx.plugin_config(self.name()).await);
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
        _scope: &PluginScope,
    ) -> anyhow::Result<Flow> {
        let Some(msg) = ev.as_message() else {
            return Ok(Flow::Continue);
        };
        let prefix = ctx.bot_config().await.command_prefix;
        let line = msg.command_line();
        let Some(rest) = strip_cmd(&line, &prefix, "memo") else {
            return Ok(Flow::Continue);
        };
        let pool = {
            let g = self.pool.lock().await;
            g.clone()
                .ok_or_else(|| anyhow::anyhow!("memo db not open"))?
        };
        let cfg = MemoCfg::from_value(&ctx.plugin_config(self.name()).await);
        dispatch_cmd(ctx, msg, &pool, &cfg, rest.trim()).await?;
        Ok(Flow::Stop)
    }
}

fn strip_cmd<'a>(line: &'a str, prefix: &str, name: &str) -> Option<&'a str> {
    let head = format!("{prefix}{name}");
    if line == head {
        return Some("");
    }
    let with_space = format!("{head} ");
    line.strip_prefix(&with_space)
}

fn source_key(src: Source) -> (&'static str, i64) {
    match src {
        Source::Group { id } => ("group", id),
        Source::Friend { id } => ("friend", id),
    }
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
            ctx.reply_text(msg, HELP).await?;
        }
        "add" => add_memo(ctx, msg, pool, cfg, arg).await?,
        "list" => list_memos(ctx, msg, pool).await?,
        "del" | "rm" => del_memo(ctx, msg, pool, arg).await?,
        "count" => count_memos(ctx, msg, pool).await?,
        _ => {
            ctx.reply_text(msg, "不认识这个子命令，发 .memo 看说明")
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
    if body.is_empty() {
        ctx.reply_text(msg, "格式：.memo add <文本>").await?;
        return Ok(());
    }
    let (kind, sid) = source_key(msg.source);
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM memos WHERE source_kind = ? AND source_id = ?")
            .bind(kind)
            .bind(sid)
            .fetch_one(pool)
            .await?;
    if n >= cfg.max_per_source {
        ctx.reply_text(
            msg,
            format!(
                "本对话已有 {n} 条，上限 {}。先 .memo del",
                cfg.max_per_source
            ),
        )
        .await?;
        return Ok(());
    }
    let r = sqlx::query(
        "INSERT INTO memos(source_kind, source_id, user_id, body, created_at)
         VALUES(?, ?, ?, ?, ?)",
    )
    .bind(kind)
    .bind(sid)
    .bind(msg.user_id())
    .bind(body)
    .bind(chrono_now())
    .execute(pool)
    .await?;
    ctx.reply_text(msg, format!("已记下 #{}", r.last_insert_rowid()))
        .await?;
    Ok(())
}

async fn list_memos(ctx: &BotContext, msg: &MessageEvent, pool: &SqlitePool) -> anyhow::Result<()> {
    let (kind, sid) = source_key(msg.source);
    let rows: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT id, user_id, body FROM memos
         WHERE source_kind = ? AND source_id = ?
         ORDER BY id DESC LIMIT 20",
    )
    .bind(kind)
    .bind(sid)
    .fetch_all(pool)
    .await?;
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
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM memos WHERE source_kind = ? AND source_id = ?")
            .bind(kind)
            .bind(sid)
            .fetch_one(pool)
            .await?;
    ctx.reply_text(msg, format!("这个对话有 {n} 条备忘"))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_respects_prefix() {
        assert_eq!(strip_cmd(".memo", ".", "memo"), Some(""));
        assert_eq!(strip_cmd(".memo add hi", ".", "memo"), Some("add hi"));
        assert_eq!(strip_cmd("!memo list", "!", "memo"), Some("list"));
        assert_eq!(strip_cmd(".memo", "!", "memo"), None);
        assert_eq!(strip_cmd(".memory", ".", "memo"), None);
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
}
