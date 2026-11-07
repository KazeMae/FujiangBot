use anyhow::Context;
use fujiang_core::{MessageEvent, Source};
use sqlx::SqlitePool;

#[cfg(test)]
use crate::client::ChatMessage;
use crate::client::Role;

const WINDOW_SECS: i64 = 3600;
const COUNT_USER: &str = "SELECT COUNT(*) FROM usage_events WHERE user_id = ?1 AND ts > ?2";
const COUNT_SESSION: &str = "SELECT COUNT(*) FROM usage_events WHERE session_key = ?1 AND ts > ?2";

pub fn session_key(source: Source) -> String {
    match source {
        Source::Group { id } => format!("group:{id}"),
        Source::Friend { id } => format!("private:{id}"),
    }
}

pub async fn ensure_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            session_key TEXT PRIMARY KEY,
            updated_at INTEGER NOT NULL,
            persona TEXT,
            summary TEXT,
            fold_rev INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(pool)
    .await
    .context("create sessions")?;
    ensure_persona_column(pool).await?;
    ensure_summary_column(pool).await?;
    ensure_fold_rev(pool).await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_key TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            user_id INTEGER,
            created_at INTEGER NOT NULL
        )",
    )
    .execute(pool)
    .await
    .context("create messages")?;
    sqlx::query("CREATE INDEX IF NOT EXISTS messages_session ON messages(session_key, id)")
        .execute(pool)
        .await
        .context("create messages_session")?;
    ensure_message_columns(pool).await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS usage_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_key TEXT NOT NULL,
            user_id INTEGER NOT NULL,
            ts INTEGER NOT NULL
        )",
    )
    .execute(pool)
    .await
    .context("create usage_events")?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS usage_daily (
            day INTEGER NOT NULL,
            session_key TEXT NOT NULL,
            user_id INTEGER NOT NULL,
            calls INTEGER NOT NULL,
            in_tokens INTEGER NOT NULL,
            out_tokens INTEGER NOT NULL,
            PRIMARY KEY (day, session_key, user_id)
        )",
    )
    .execute(pool)
    .await
    .context("create usage_daily")?;
    crate::layered_memory::ensure_schema(pool).await?;
    crate::schedule::ensure_schema(pool).await?;
    Ok(())
}

async fn ensure_persona_column(pool: &SqlitePool) -> anyhow::Result<()> {
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('sessions')")
        .fetch_all(pool)
        .await
        .context("sessions columns")?;
    if names.iter().any(|name| name == "persona") {
        return Ok(());
    }
    sqlx::query("ALTER TABLE sessions ADD COLUMN persona TEXT")
        .execute(pool)
        .await
        .context("add sessions.persona")?;
    Ok(())
}

async fn ensure_summary_column(pool: &SqlitePool) -> anyhow::Result<()> {
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('sessions')")
        .fetch_all(pool)
        .await
        .context("sessions columns")?;
    if names.iter().any(|name| name == "summary") {
        return Ok(());
    }
    sqlx::query("ALTER TABLE sessions ADD COLUMN summary TEXT")
        .execute(pool)
        .await
        .context("add sessions.summary")?;
    Ok(())
}

pub(crate) fn is_group_key(session_key: &str) -> bool {
    match session_key.split_once(':') {
        Some(("group", id)) => !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()),
        _ => false,
    }
}

pub(crate) struct StoredTurn {
    pub role: Role,
    pub content: String,
    pub user_id: Option<i64>,
    pub nickname: Option<String>,
    pub card: Option<String>,
    pub reply_id: Option<i64>,
}

pub struct UserSnap {
    pub user_id: i64,
    pub nickname: Option<String>,
    pub card: Option<String>,
    pub reply_id: Option<i64>,
}

impl UserSnap {
    #[cfg(test)]
    pub fn bare(user_id: i64) -> Self {
        Self {
            user_id,
            nickname: None,
            card: None,
            reply_id: None,
        }
    }
}

pub fn snap_of(msg: &MessageEvent) -> UserSnap {
    UserSnap {
        user_id: msg.user_id(),
        nickname: msg.sender.nickname.clone(),
        card: msg.sender.card.clone(),
        reply_id: msg.reply_id(),
    }
}

#[cfg(test)]
pub(crate) async fn recent_turns(
    pool: &SqlitePool,
    session_key: &str,
    max_turns: u32,
) -> anyhow::Result<Vec<ChatMessage>> {
    let rows = recent_stored(pool, session_key, max_turns).await?;
    Ok(rows
        .into_iter()
        .map(|row| ChatMessage {
            role: row.role,
            content: row.content,
            images: Vec::new(),
        })
        .collect())
}

#[cfg(test)]
pub(crate) async fn recent_stored(
    pool: &SqlitePool,
    session_key: &str,
    max_turns: u32,
) -> anyhow::Result<Vec<StoredTurn>> {
    let limit = i64::from(max_turns.saturating_mul(2));
    if limit == 0 {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(
        "SELECT role, content, user_id, nickname, card, reply_id FROM messages
         WHERE session_key = ?1 AND role IN ('user', 'assistant')
         ORDER BY id DESC
         LIMIT ?2",
    )
    .bind(session_key)
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("load history")?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.into_iter().rev() {
        if let Some(turn) = stored_turn(&row)? {
            out.push(turn);
        }
    }
    Ok(out)
}

async fn ensure_message_columns(pool: &SqlitePool) -> anyhow::Result<()> {
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('messages')")
        .fetch_all(pool)
        .await
        .context("messages columns")?;
    for (column, ddl) in [
        ("user_id", "ALTER TABLE messages ADD COLUMN user_id INTEGER"),
        ("nickname", "ALTER TABLE messages ADD COLUMN nickname TEXT"),
        ("card", "ALTER TABLE messages ADD COLUMN card TEXT"),
        (
            "reply_id",
            "ALTER TABLE messages ADD COLUMN reply_id INTEGER",
        ),
    ] {
        if names.iter().any(|name| name == column) {
            continue;
        }
        sqlx::query(ddl)
            .execute(pool)
            .await
            .with_context(|| format!("add messages.{column}"))?;
    }
    Ok(())
}

#[cfg(test)]
pub async fn save_exchange(
    pool: &SqlitePool,
    session_key: &str,
    user_id: i64,
    user_text: &str,
    assistant_text: &str,
) -> anyhow::Result<()> {
    save_faced(
        pool,
        session_key,
        &UserSnap::bare(user_id),
        user_text,
        assistant_text,
    )
    .await
}

pub async fn save_faced(
    pool: &SqlitePool,
    session_key: &str,
    snap: &UserSnap,
    user_text: &str,
    assistant_text: &str,
) -> anyhow::Result<()> {
    let now = unix_now();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO sessions (session_key, updated_at, fold_rev) VALUES (?1, ?2, 1)
         ON CONFLICT(session_key) DO UPDATE SET
            updated_at = excluded.updated_at,
            fold_rev = fold_rev + 1",
    )
    .bind(session_key)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO messages
            (session_key, role, content, user_id, nickname, card, reply_id, created_at)
         VALUES (?1, 'user', ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind(session_key)
    .bind(user_text)
    .bind(snap.user_id)
    .bind(&snap.nickname)
    .bind(&snap.card)
    .bind(snap.reply_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO messages
            (session_key, role, content, user_id, nickname, card, reply_id, created_at)
         VALUES (?1, 'assistant', ?2, NULL, NULL, NULL, NULL, ?3)",
    )
    .bind(session_key)
    .bind(assistant_text)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

fn stored_turn(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Option<StoredTurn>> {
    let role: String = sqlx::Row::try_get(row, 0)?;
    let Some(role) = parse_role(&role) else {
        return Ok(None);
    };
    let (nickname, card, reply_id) = read_face(row, 3)?;
    Ok(Some(StoredTurn {
        role,
        content: sqlx::Row::try_get(row, 1)?,
        user_id: sqlx::Row::try_get(row, 2)?,
        nickname,
        card,
        reply_id,
    }))
}

fn read_face(
    row: &sqlx::sqlite::SqliteRow,
    at: usize,
) -> anyhow::Result<(Option<String>, Option<String>, Option<i64>)> {
    Ok((
        sqlx::Row::try_get(row, at)?,
        sqlx::Row::try_get(row, at + 1)?,
        sqlx::Row::try_get(row, at + 2)?,
    ))
}

pub async fn persona_of(pool: &SqlitePool, session_key: &str) -> anyhow::Result<Option<String>> {
    let row = sqlx::query("SELECT persona FROM sessions WHERE session_key = ?1")
        .bind(session_key)
        .fetch_optional(pool)
        .await
        .context("load persona")?;
    let Some(row) = row else {
        return Ok(None);
    };
    let name: Option<String> = sqlx::Row::try_get(&row, 0)?;
    Ok(name.filter(|text| !text.is_empty()))
}

pub async fn set_persona(
    pool: &SqlitePool,
    session_key: &str,
    persona: &str,
    now: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO sessions (session_key, updated_at, persona, fold_rev) VALUES (?1, ?2, ?3, 1)
         ON CONFLICT(session_key) DO UPDATE SET
            updated_at = excluded.updated_at,
            persona = excluded.persona,
            fold_rev = fold_rev + 1",
    )
    .bind(session_key)
    .bind(now)
    .bind(persona)
    .execute(pool)
    .await
    .context("set persona")?;
    Ok(())
}

pub async fn clear_messages(pool: &SqlitePool, session_key: &str) -> anyhow::Result<u64> {
    let mut tx = pool.begin().await.context("clear messages tx")?;
    let result = sqlx::query("DELETE FROM messages WHERE session_key = ?1")
        .bind(session_key)
        .execute(&mut *tx)
        .await
        .context("clear messages")?;
    sqlx::query(
        "UPDATE sessions SET fold_rev = fold_rev + 1, updated_at = ?2 WHERE session_key = ?1",
    )
    .bind(session_key)
    .bind(unix_now())
    .execute(&mut *tx)
    .await
    .context("bump fold_rev")?;
    let n = result.rows_affected();
    tx.commit().await.context("commit clear messages")?;
    Ok(n)
}

pub async fn summary_of(pool: &SqlitePool, session_key: &str) -> anyhow::Result<String> {
    stored_summary(pool, session_key).await
}

pub async fn clear_summary(pool: &SqlitePool, session_key: &str) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await.context("clear summary tx")?;
    let current = stored_summary(&mut *tx, session_key).await?;
    let cleared = !current.is_empty();
    let sql = if cleared {
        "UPDATE sessions SET summary = NULL, fold_rev = fold_rev + 1 WHERE session_key = ?1"
    } else {
        "UPDATE sessions SET fold_rev = fold_rev + 1 WHERE session_key = ?1"
    };
    sqlx::query(sql)
        .bind(session_key)
        .execute(&mut *tx)
        .await
        .context("clear summary")?;
    tx.commit().await.context("commit clear summary")?;
    Ok(cleared)
}

pub(crate) struct FoldHead {
    pub rev: i64,
    pub persona: Option<String>,
    pub summary: Option<String>,
    pub rows: Vec<FoldRow>,
}

pub(crate) struct FoldRow {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub user_id: Option<i64>,
    pub nickname: Option<String>,
    pub card: Option<String>,
    pub reply_id: Option<i64>,
}

pub(crate) async fn load_fold_head(
    pool: &SqlitePool,
    session_key: &str,
) -> anyhow::Result<FoldHead> {
    let mut tx = pool.begin().await.context("fold head tx")?;
    let session =
        sqlx::query("SELECT fold_rev, persona, summary FROM sessions WHERE session_key = ?1")
            .bind(session_key)
            .fetch_optional(&mut *tx)
            .await
            .context("load fold head")?;
    let (rev, persona, summary) = if let Some(row) = session {
        let rev: i64 = sqlx::Row::try_get(&row, 0)?;
        let persona: Option<String> = sqlx::Row::try_get(&row, 1)?;
        let summary: Option<String> = sqlx::Row::try_get(&row, 2)?;
        (rev, persona, summary)
    } else {
        (0, None, None)
    };
    let loaded = sqlx::query(
        "SELECT id, role, content, user_id, nickname, card, reply_id FROM messages
         WHERE session_key = ?1 AND role IN ('user', 'assistant')
         ORDER BY id ASC",
    )
    .bind(session_key)
    .fetch_all(&mut *tx)
    .await
    .context("load fold rows")?;
    let mut rows = Vec::with_capacity(loaded.len());
    for row in loaded {
        let (nickname, card, reply_id) = read_face(&row, 4)?;
        rows.push(FoldRow {
            id: sqlx::Row::try_get(&row, 0)?,
            role: sqlx::Row::try_get(&row, 1)?,
            content: sqlx::Row::try_get(&row, 2)?,
            user_id: sqlx::Row::try_get(&row, 3)?,
            nickname,
            card,
            reply_id,
        });
    }
    tx.commit().await.context("end fold head")?;
    Ok(FoldHead {
        rev,
        persona,
        summary,
        rows,
    })
}

pub(crate) async fn commit_fold(
    pool: &SqlitePool,
    session_key: &str,
    head: &FoldHead,
    new_summary: &str,
    fold: &[FoldRow],
) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await.context("commit fold tx")?;
    let updated = sqlx::query(
        "UPDATE sessions
         SET summary = ?1, updated_at = ?2, fold_rev = fold_rev + 1
         WHERE session_key = ?3 AND fold_rev = ?4
           AND ((?5 IS NULL AND persona IS NULL) OR persona = ?5)
           AND ((?6 IS NULL AND summary IS NULL) OR summary = ?6)",
    )
    .bind(new_summary)
    .bind(unix_now())
    .bind(session_key)
    .bind(head.rev)
    .bind(&head.persona)
    .bind(&head.summary)
    .execute(&mut *tx)
    .await
    .context("cas summary")?;
    if updated.rows_affected() != 1 {
        tx.rollback().await.ok();
        return Ok(false);
    }
    for row in fold {
        let deleted = sqlx::query(
            "DELETE FROM messages
             WHERE id = ?1 AND session_key = ?2 AND role = ?3 AND content = ?4
               AND ((?5 IS NULL AND user_id IS NULL) OR user_id = ?5)",
        )
        .bind(row.id)
        .bind(session_key)
        .bind(&row.role)
        .bind(&row.content)
        .bind(row.user_id)
        .execute(&mut *tx)
        .await
        .context("delete folded row")?;
        if deleted.rows_affected() != 1 {
            tx.rollback().await.ok();
            return Ok(false);
        }
    }
    tx.commit().await.context("commit fold")?;
    Ok(true)
}

pub(crate) async fn active_messages(
    pool: &SqlitePool,
    session_key: &str,
) -> anyhow::Result<Vec<StoredTurn>> {
    let rows = sqlx::query(
        "SELECT role, content, user_id, nickname, card, reply_id FROM messages
         WHERE session_key = ?1 AND role IN ('user', 'assistant')
         ORDER BY id ASC",
    )
    .bind(session_key)
    .fetch_all(pool)
    .await
    .context("load active history")?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(turn) = stored_turn(&row)? {
            out.push(turn);
        }
    }
    Ok(out)
}

async fn ensure_fold_rev(pool: &SqlitePool) -> anyhow::Result<()> {
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('sessions')")
        .fetch_all(pool)
        .await
        .context("sessions columns")?;
    if names.iter().any(|name| name == "fold_rev") {
        return Ok(());
    }
    sqlx::query("ALTER TABLE sessions ADD COLUMN fold_rev INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await
        .context("add sessions.fold_rev")?;
    Ok(())
}

async fn stored_summary<'a, E>(executor: E, session_key: &str) -> anyhow::Result<String>
where
    E: sqlx::Executor<'a, Database = sqlx::Sqlite>,
{
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT summary FROM sessions WHERE session_key = ?1")
            .bind(session_key)
            .fetch_optional(executor)
            .await
            .context("load summary")?;
    Ok(row.flatten().unwrap_or_default().trim().to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageCount {
    pub user: i64,
    pub session: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Taken,
    UserLimited,
    SessionLimited,
}

/// A zero cap is off.
/// `per_group` counts `session_key`, so a private chat is one session.
pub async fn take_slot(
    pool: &SqlitePool,
    session_key: &str,
    user_id: i64,
    per_user: u32,
    per_group: u32,
    now: i64,
) -> anyhow::Result<Slot> {
    let cutoff = now.saturating_sub(WINDOW_SECS);
    let mut tx = pool.begin().await.context("usage tx")?;
    sqlx::query("DELETE FROM usage_events WHERE ts <= ?1")
        .bind(cutoff)
        .execute(&mut *tx)
        .await
        .context("prune usage")?;
    let user_n: i64 = sqlx::query_scalar(COUNT_USER)
        .bind(user_id)
        .bind(cutoff)
        .fetch_one(&mut *tx)
        .await
        .context("count user usage")?;
    let session_n: i64 = sqlx::query_scalar(COUNT_SESSION)
        .bind(session_key)
        .bind(cutoff)
        .fetch_one(&mut *tx)
        .await
        .context("count session usage")?;
    if per_user > 0 && user_n >= i64::from(per_user) {
        tx.commit().await.context("commit usage prune")?;
        return Ok(Slot::UserLimited);
    }
    if per_group > 0 && session_n >= i64::from(per_group) {
        tx.commit().await.context("commit usage prune")?;
        return Ok(Slot::SessionLimited);
    }
    if per_user > 0 || per_group > 0 {
        sqlx::query("INSERT INTO usage_events (session_key, user_id, ts) VALUES (?1, ?2, ?3)")
            .bind(session_key)
            .bind(user_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .context("insert usage")?;
    }
    tx.commit().await.context("commit usage")?;
    Ok(Slot::Taken)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DayUsage {
    pub calls: i64,
    pub in_tokens: i64,
    pub out_tokens: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TodayUsage {
    pub user: DayUsage,
    pub session: DayUsage,
}

const SUM_USER: &str =
    "SELECT COALESCE(SUM(calls), 0), COALESCE(SUM(in_tokens), 0), COALESCE(SUM(out_tokens), 0)
    FROM usage_daily WHERE day = ?1 AND user_id = ?2";
const SUM_SESSION: &str =
    "SELECT COALESCE(SUM(calls), 0), COALESCE(SUM(in_tokens), 0), COALESCE(SUM(out_tokens), 0)
    FROM usage_daily WHERE day = ?1 AND session_key = ?2";

pub async fn record_daily(
    pool: &SqlitePool,
    session_key: &str,
    user_id: i64,
    now: i64,
    calls: i64,
    in_tokens: i64,
    out_tokens: i64,
) -> anyhow::Result<()> {
    if calls <= 0 && in_tokens <= 0 && out_tokens <= 0 {
        return Ok(());
    }
    let day = utc_day(now);
    sqlx::query(
        "INSERT INTO usage_daily (day, session_key, user_id, calls, in_tokens, out_tokens)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(day, session_key, user_id) DO UPDATE SET
            calls = calls + excluded.calls,
            in_tokens = in_tokens + excluded.in_tokens,
            out_tokens = out_tokens + excluded.out_tokens",
    )
    .bind(day)
    .bind(session_key)
    .bind(user_id)
    .bind(calls)
    .bind(in_tokens)
    .bind(out_tokens)
    .execute(pool)
    .await
    .context("record usage_daily")?;
    Ok(())
}

pub async fn today_usage(
    pool: &SqlitePool,
    session_key: &str,
    user_id: i64,
    now: i64,
) -> anyhow::Result<TodayUsage> {
    let day = utc_day(now);
    let user = day_sum_user(pool, day, user_id).await?;
    let session = day_sum_key(pool, day, session_key).await?;
    Ok(TodayUsage { user, session })
}

async fn day_sum_user(pool: &SqlitePool, day: i64, user_id: i64) -> anyhow::Result<DayUsage> {
    let row = sqlx::query(SUM_USER)
        .bind(day)
        .bind(user_id)
        .fetch_one(pool)
        .await
        .context("sum usage_daily")?;
    row_usage(&row)
}

async fn day_sum_key(pool: &SqlitePool, day: i64, session_key: &str) -> anyhow::Result<DayUsage> {
    let row = sqlx::query(SUM_SESSION)
        .bind(day)
        .bind(session_key)
        .fetch_one(pool)
        .await
        .context("sum usage_daily session")?;
    row_usage(&row)
}

fn utc_day(now: i64) -> i64 {
    now.div_euclid(86_400)
}

fn row_usage(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<DayUsage> {
    Ok(DayUsage {
        calls: sqlx::Row::try_get(row, 0)?,
        in_tokens: sqlx::Row::try_get(row, 1)?,
        out_tokens: sqlx::Row::try_get(row, 2)?,
    })
}

pub async fn usage_counts(
    pool: &SqlitePool,
    session_key: &str,
    user_id: i64,
    now: i64,
) -> anyhow::Result<UsageCount> {
    let cutoff = now.saturating_sub(WINDOW_SECS);
    let user: i64 = sqlx::query_scalar(COUNT_USER)
        .bind(user_id)
        .bind(cutoff)
        .fetch_one(pool)
        .await
        .context("count user usage")?;
    let session: i64 = sqlx::query_scalar(COUNT_SESSION)
        .bind(session_key)
        .bind(cutoff)
        .fetch_one(pool)
        .await
        .context("count session usage")?;
    Ok(UsageCount { user, session })
}

fn parse_role(role: &str) -> Option<Role> {
    match role {
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        _ => None,
    }
}

pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
