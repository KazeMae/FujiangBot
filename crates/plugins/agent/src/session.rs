use anyhow::Context;
use fujiang_core::Source;
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
    ensure_message_user_id(pool).await?;
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
        "SELECT role, content, user_id FROM messages
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
        let role: String = sqlx::Row::try_get(&row, 0)?;
        let content: String = sqlx::Row::try_get(&row, 1)?;
        let user_id: Option<i64> = sqlx::Row::try_get(&row, 2)?;
        let Some(role) = parse_role(&role) else {
            continue;
        };
        out.push(StoredTurn {
            role,
            content,
            user_id,
        });
    }
    Ok(out)
}

async fn ensure_message_user_id(pool: &SqlitePool) -> anyhow::Result<()> {
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('messages')")
        .fetch_all(pool)
        .await
        .context("messages columns")?;
    if names.iter().any(|name| name == "user_id") {
        return Ok(());
    }
    sqlx::query("ALTER TABLE messages ADD COLUMN user_id INTEGER")
        .execute(pool)
        .await
        .context("add messages.user_id")?;
    Ok(())
}

pub async fn save_exchange(
    pool: &SqlitePool,
    session_key: &str,
    user_id: i64,
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
        "INSERT INTO messages (session_key, role, content, user_id, created_at)
         VALUES (?1, 'user', ?2, ?3, ?4)",
    )
    .bind(session_key)
    .bind(user_text)
    .bind(user_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO messages (session_key, role, content, user_id, created_at)
         VALUES (?1, 'assistant', ?2, NULL, ?3)",
    )
    .bind(session_key)
    .bind(assistant_text)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
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
        "SELECT id, role, content, user_id FROM messages
         WHERE session_key = ?1 AND role IN ('user', 'assistant')
         ORDER BY id ASC",
    )
    .bind(session_key)
    .fetch_all(&mut *tx)
    .await
    .context("load fold rows")?;
    let mut rows = Vec::with_capacity(loaded.len());
    for row in loaded {
        rows.push(FoldRow {
            id: sqlx::Row::try_get(&row, 0)?,
            role: sqlx::Row::try_get(&row, 1)?,
            content: sqlx::Row::try_get(&row, 2)?,
            user_id: sqlx::Row::try_get(&row, 3)?,
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
        "SELECT role, content, user_id FROM messages
         WHERE session_key = ?1 AND role IN ('user', 'assistant')
         ORDER BY id ASC",
    )
    .bind(session_key)
    .fetch_all(pool)
    .await
    .context("load active history")?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let role: String = sqlx::Row::try_get(&row, 0)?;
        let content: String = sqlx::Row::try_get(&row, 1)?;
        let user_id: Option<i64> = sqlx::Row::try_get(&row, 2)?;
        let Some(role) = parse_role(&role) else {
            continue;
        };
        out.push(StoredTurn {
            role,
            content,
            user_id,
        });
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
mod tests {
    use super::*;

    async fn mem() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        ensure_schema(&pool).await.unwrap();
        ensure_schema(&pool).await.unwrap();
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master
             WHERE name IN ('sessions', 'messages', 'messages_session', 'usage_events', 'usage_daily')
             ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            names,
            vec![
                "messages".to_string(),
                "messages_session".to_string(),
                "sessions".to_string(),
                "usage_daily".to_string(),
                "usage_events".to_string(),
            ]
        );
        let cols: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('sessions') ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            cols,
            vec![
                "fold_rev".to_string(),
                "persona".to_string(),
                "session_key".to_string(),
                "summary".to_string(),
                "updated_at".to_string(),
            ]
        );
        pool
    }

    #[test]
    fn group_is_one_key_and_private_is_per_friend() {
        assert_eq!(session_key(Source::Group { id: 7 }), "group:7");
        assert_ne!(
            session_key(Source::Group { id: 7 }),
            session_key(Source::Group { id: 8 })
        );
        assert_eq!(session_key(Source::Friend { id: 3 }), "private:3");
        assert_ne!(
            session_key(Source::Friend { id: 3 }),
            session_key(Source::Friend { id: 4 })
        );
        assert_ne!(
            session_key(Source::Group { id: 3 }),
            session_key(Source::Friend { id: 3 })
        );
    }

    #[tokio::test]
    async fn history_keeps_the_latest_pairs_for_one_session() {
        let pool = mem().await;
        let group = session_key(Source::Group { id: 7 });
        let other = session_key(Source::Friend { id: 3 });
        save_exchange(&pool, &group, 1, "u1", "a1").await.unwrap();
        save_exchange(&pool, &group, 2, "u2", "a2").await.unwrap();
        save_exchange(&pool, &group, 1, "u3", "a3").await.unwrap();
        save_exchange(&pool, &other, 3, "solo", "ok").await.unwrap();

        let last = recent_turns(&pool, &group, 1).await.unwrap();
        assert_eq!(
            last,
            vec![
                ChatMessage {
                    role: Role::User,
                    content: "u3".into(),
                    images: Vec::new(),
                },
                ChatMessage {
                    role: Role::Assistant,
                    content: "a3".into(),
                    images: Vec::new(),
                },
            ]
        );
        let all = recent_turns(&pool, &group, 20).await.unwrap();
        assert_eq!(all.len(), 6);
        assert_eq!(all[0].content, "u1");
        assert_eq!(all[5].content, "a3");
        assert!(recent_turns(&pool, &group, 0).await.unwrap().is_empty());

        let user_ids: Vec<(String, Option<i64>)> =
            sqlx::query_as("SELECT role, user_id FROM messages WHERE session_key = ?1 ORDER BY id")
                .bind(&group)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            user_ids,
            vec![
                ("user".into(), Some(1)),
                ("assistant".into(), None),
                ("user".into(), Some(2)),
                ("assistant".into(), None),
                ("user".into(), Some(1)),
                ("assistant".into(), None),
            ]
        );

        let sessions: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE session_key = ?1")
                .bind(&group)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(sessions, 1);

        let left = recent_turns(&pool, &other, 20).await.unwrap();
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].content, "solo");
    }

    #[tokio::test]
    async fn old_sessions_table_gains_persona_and_keeps_rows() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE sessions (
                session_key TEXT PRIMARY KEY,
                updated_at INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO sessions (session_key, updated_at) VALUES ('group:1', 5)")
            .execute(&pool)
            .await
            .unwrap();
        ensure_schema(&pool).await.unwrap();
        ensure_schema(&pool).await.unwrap();
        let persona: Option<String> =
            sqlx::query_scalar("SELECT persona FROM sessions WHERE session_key = 'group:1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(persona, None);
        let summary: Option<String> =
            sqlx::query_scalar("SELECT summary FROM sessions WHERE session_key = 'group:1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(summary, None);
        set_persona(&pool, "group:1", "strict", 9).await.unwrap();
        save_exchange(&pool, "group:1", 3, "hi", "yo")
            .await
            .unwrap();
        assert_eq!(
            persona_of(&pool, "group:1").await.unwrap().as_deref(),
            Some("strict")
        );
        let updated: i64 =
            sqlx::query_scalar("SELECT updated_at FROM sessions WHERE session_key = 'group:1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(updated >= 9);
    }

    #[tokio::test]
    async fn daily_rows_add_within_one_utc_day_and_split_on_the_next() {
        let pool = mem().await;
        let day = 86_400 * 10;
        record_daily(&pool, "group:7", 5, day + 10, 1, 4, 2)
            .await
            .unwrap();
        record_daily(&pool, "group:7", 5, day + 20, 1, 0, 0)
            .await
            .unwrap();
        record_daily(&pool, "private:5", 5, day + 30, 1, 10, 1)
            .await
            .unwrap();
        record_daily(&pool, "group:7", 8, day + 40, 1, 7, 8)
            .await
            .unwrap();
        record_daily(&pool, "group:7", 5, day, 0, 0, 0)
            .await
            .unwrap();

        let today = today_usage(&pool, "group:7", 5, day + 50).await.unwrap();
        assert_eq!(
            today.user,
            DayUsage {
                calls: 3,
                in_tokens: 14,
                out_tokens: 3,
            }
        );
        assert_eq!(
            today.session,
            DayUsage {
                calls: 3,
                in_tokens: 11,
                out_tokens: 10,
            }
        );
        let other = today_usage(&pool, "group:8", 9, day + 50).await.unwrap();
        assert_eq!(
            other,
            TodayUsage {
                user: DayUsage::default(),
                session: DayUsage::default(),
            }
        );

        record_daily(&pool, "group:7", 5, day + 86_400, 1, 3, 3)
            .await
            .unwrap();
        let next = today_usage(&pool, "group:7", 5, day + 86_400 + 1)
            .await
            .unwrap();
        assert_eq!(
            next.user,
            DayUsage {
                calls: 1,
                in_tokens: 3,
                out_tokens: 3,
            }
        );
        assert_eq!(next.session, next.user);
        clear_messages(&pool, "group:7").await.unwrap();
        let kept = today_usage(&pool, "group:7", 5, day + 50).await.unwrap();
        assert_eq!(kept.user.calls, 3);
        assert_eq!(kept.session.calls, 3);
    }
}
