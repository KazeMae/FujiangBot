use anyhow::Context;
use fujiang_core::Source;
use sqlx::SqlitePool;

use crate::client::{ChatMessage, Role};

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
            persona TEXT
        )",
    )
    .execute(pool)
    .await
    .context("create sessions")?;
    ensure_persona_column(pool).await?;
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

pub async fn recent_turns(
    pool: &SqlitePool,
    session_key: &str,
    max_turns: u32,
) -> anyhow::Result<Vec<ChatMessage>> {
    let limit = i64::from(max_turns.saturating_mul(2));
    if limit == 0 {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(
        "SELECT role, content FROM messages
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
        let Some(role) = parse_role(&role) else {
            continue;
        };
        out.push(ChatMessage {
            role,
            content,
            images: Vec::new(),
        });
    }
    Ok(out)
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
        "INSERT INTO sessions (session_key, updated_at) VALUES (?1, ?2)
         ON CONFLICT(session_key) DO UPDATE SET updated_at = excluded.updated_at",
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
        "INSERT INTO sessions (session_key, updated_at, persona) VALUES (?1, ?2, ?3)
         ON CONFLICT(session_key) DO UPDATE SET
            updated_at = excluded.updated_at,
            persona = excluded.persona",
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
    let result = sqlx::query("DELETE FROM messages WHERE session_key = ?1")
        .bind(session_key)
        .execute(pool)
        .await
        .context("clear messages")?;
    Ok(result.rows_affected())
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
             WHERE name IN ('sessions', 'messages', 'messages_session', 'usage_events')
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
                "persona".to_string(),
                "session_key".to_string(),
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
}
