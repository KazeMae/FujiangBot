use anyhow::Context;
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};

pub(crate) const DEFAULT_UNUSED_DAYS: u32 = 90;
pub(crate) const DEFAULT_MAX_ENTRIES: u32 = 200;
const DAY_SECS: i64 = 86_400;

pub(crate) fn unused_secs(days: u32) -> i64 {
    i64::from(days).saturating_mul(DAY_SECS)
}

pub(crate) async fn consolidate_if(
    pool: &SqlitePool,
    enabled: bool,
    now: i64,
    unused_secs: i64,
    max_entries: i64,
) -> anyhow::Result<()> {
    if !enabled {
        return Ok(());
    }
    run_all(pool, now, unused_secs, max_entries).await?;
    Ok(())
}

pub(crate) async fn run(
    pool: &SqlitePool,
    pairs: &[(&str, &str)],
    now: i64,
    unused_secs: i64,
    max_entries: i64,
) -> anyhow::Result<Value> {
    let mut removed = 0u64;
    let mut kept = 0i64;
    for (kind, id) in pairs {
        removed += drop_stale(pool, kind, id, now, unused_secs).await?;
        removed += drop_over_cap(pool, kind, id, max_entries).await?;
        kept += count_scope(pool, kind, id).await?;
    }
    Ok(json!({ "removed": removed, "kept": kept }))
}

async fn run_all(
    pool: &SqlitePool,
    now: i64,
    unused_secs: i64,
    max_entries: i64,
) -> anyhow::Result<Value> {
    let rows = sqlx::query(
        "SELECT DISTINCT scope_kind, scope_id FROM layered_memories
         ORDER BY scope_kind, scope_id",
    )
    .fetch_all(pool)
    .await
    .context("list memory scopes")?;
    let mut owned = Vec::with_capacity(rows.len());
    for row in &rows {
        let kind: String = row.try_get(0)?;
        let id: String = row.try_get(1)?;
        owned.push((kind, id));
    }
    let pairs: Vec<(&str, &str)> = owned
        .iter()
        .map(|(kind, id)| (kind.as_str(), id.as_str()))
        .collect();
    run(pool, &pairs, now, unused_secs, max_entries).await
}

async fn drop_stale(
    pool: &SqlitePool,
    kind: &str,
    id: &str,
    now: i64,
    unused_secs: i64,
) -> anyhow::Result<u64> {
    let cutoff = now.saturating_sub(unused_secs.max(0));
    let done = sqlx::query(
        "DELETE FROM layered_memories
         WHERE scope_kind = ?1 AND scope_id = ?2
           AND (
             (last_used_at IS NOT NULL AND last_used_at < ?3)
             OR (last_used_at IS NULL AND updated_at < ?3)
           )",
    )
    .bind(kind)
    .bind(id)
    .bind(cutoff)
    .execute(pool)
    .await
    .context("drop unused memories")?;
    Ok(done.rows_affected())
}

struct Ranked {
    key: String,
    usage: i64,
    recency: i64,
}

async fn drop_over_cap(
    pool: &SqlitePool,
    kind: &str,
    id: &str,
    max_entries: i64,
) -> anyhow::Result<u64> {
    let cap = max_entries.max(0);
    let rows = sqlx::query(
        "SELECT key, usage_count, updated_at, last_used_at
         FROM layered_memories
         WHERE scope_kind = ?1 AND scope_id = ?2",
    )
    .bind(kind)
    .bind(id)
    .fetch_all(pool)
    .await
    .context("rank memories")?;
    let mut ranked = Vec::with_capacity(rows.len());
    for row in rows {
        let key: String = row.try_get(0)?;
        let usage: i64 = row.try_get(1)?;
        let updated_at: i64 = row.try_get(2)?;
        let last_used_at: Option<i64> = row.try_get(3)?;
        let recency = match last_used_at {
            Some(used) => used.max(updated_at),
            None => updated_at,
        };
        ranked.push(Ranked {
            key,
            usage,
            recency,
        });
    }
    if ranked.len() as i64 <= cap {
        return Ok(0);
    }
    ranked.sort_by(|a, b| {
        b.usage
            .cmp(&a.usage)
            .then(b.recency.cmp(&a.recency))
            .then(a.key.cmp(&b.key))
    });
    let mut removed = 0u64;
    for row in ranked.into_iter().skip(cap as usize) {
        sqlx::query(
            "DELETE FROM layered_memories
             WHERE scope_kind = ?1 AND scope_id = ?2 AND key = ?3",
        )
        .bind(kind)
        .bind(id)
        .bind(&row.key)
        .execute(pool)
        .await
        .context("cap memories")?;
        removed += 1;
    }
    Ok(removed)
}

async fn count_scope(pool: &SqlitePool, kind: &str, id: &str) -> anyhow::Result<i64> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM layered_memories WHERE scope_kind = ?1 AND scope_id = ?2",
    )
    .bind(kind)
    .bind(id)
    .fetch_one(pool)
    .await
    .context("count memories")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layered_memory::{call, call_limited, ensure_schema};
    use serde_json::json;

    const DAY: i64 = 86_400;
    const NOW: i64 = 1_000_000;

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        ensure_schema(&pool).await.unwrap();
        pool
    }

    async fn insert(
        pool: &SqlitePool,
        kind: &str,
        id: &str,
        key: &str,
        updated_at: i64,
        usage: i64,
        last_used_at: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO layered_memories
             (scope_kind, scope_id, key, value, updated_at, source, usage_count, last_used_at)
             VALUES (?1, ?2, ?3, ?3, ?4, NULL, ?5, ?6)",
        )
        .bind(kind)
        .bind(id)
        .bind(key)
        .bind(updated_at)
        .bind(usage)
        .bind(last_used_at)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn keys(pool: &SqlitePool) -> Vec<(String, String, String)> {
        let rows = sqlx::query(
            "SELECT scope_kind, scope_id, key FROM layered_memories
             ORDER BY scope_kind, scope_id, key",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        rows.iter()
            .map(|row| {
                (
                    row.get::<String, _>(0),
                    row.get::<String, _>(1),
                    row.get::<String, _>(2),
                )
            })
            .collect()
    }

    fn speaker(user_id: i64, group_id: Option<i64>) -> Value {
        let mut args = json!({ "user_id": user_id });
        if let Some(id) = group_id {
            args["group_id"] = json!(id);
        }
        args
    }

    #[tokio::test]
    async fn old_table_gains_usage_columns_and_keeps_the_row() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE layered_memories (
                scope_kind TEXT NOT NULL,
                scope_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                source TEXT,
                UNIQUE (scope_kind, scope_id, key)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO layered_memories
             (scope_kind, scope_id, key, value, updated_at, source)
             VALUES ('user', '42', 'nick', '小明', 5, 'group:7')",
        )
        .execute(&pool)
        .await
        .unwrap();
        ensure_schema(&pool).await.unwrap();
        ensure_schema(&pool).await.unwrap();
        let row: (String, i64, Option<i64>) = sqlx::query_as(
            "SELECT value, usage_count, last_used_at FROM layered_memories WHERE key = 'nick'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, ("小明".into(), 0, None));
    }

    #[tokio::test]
    async fn get_and_list_bump_returned_rows_and_upsert_keeps_the_count() {
        let pool = pool().await;
        let who = speaker(42, Some(7));
        let mut nick = who.clone();
        nick["scope"] = json!("user");
        nick["key"] = json!("nick");
        nick["value"] = json!("小明");
        call(&pool, "upsert", &nick, 10).await.unwrap();
        let mut food = nick.clone();
        food["key"] = json!("food");
        food["value"] = json!("面");
        call(&pool, "upsert", &food, 11).await.unwrap();
        let mut other = speaker(99, Some(7));
        other["scope"] = json!("user");
        other["key"] = json!("nick");
        other["value"] = json!("别人");
        call(&pool, "upsert", &other, 12).await.unwrap();

        let mut list_user = who.clone();
        list_user["scope"] = json!("user");
        let listed = call(&pool, "list", &list_user, 20).await.unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 2);

        let mut get_nick = nick.clone();
        get_nick.as_object_mut().unwrap().remove("value");
        let got = call(&pool, "get", &get_nick, 30).await.unwrap();
        assert_eq!(got["value"], "小明");
        assert_eq!(got["updated_at"], 10);

        let usage: (i64, i64) = sqlx::query_as(
            "SELECT usage_count, last_used_at FROM layered_memories
             WHERE scope_id = '42' AND key = 'nick'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(usage, (2, 30));
        let food_usage: (i64, i64) = sqlx::query_as(
            "SELECT usage_count, last_used_at FROM layered_memories
             WHERE scope_id = '42' AND key = 'food'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(food_usage, (1, 20));
        let other_usage: (i64, Option<i64>) = sqlx::query_as(
            "SELECT usage_count, last_used_at FROM layered_memories
             WHERE scope_id = '99' AND key = 'nick'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(other_usage, (0, None));

        let mut missing = get_nick.clone();
        missing["key"] = json!("gone");
        assert!(call(&pool, "get", &missing, 31).await.unwrap().is_null());

        nick["value"] = json!("小红");
        let rewritten = call(&pool, "upsert", &nick, 40).await.unwrap();
        assert_eq!(rewritten["updated_at"], 40);
        let kept: (i64, i64, i64) = sqlx::query_as(
            "SELECT usage_count, last_used_at, updated_at FROM layered_memories
             WHERE scope_id = '42' AND key = 'nick'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(kept, (2, 30, 40));
    }

    #[tokio::test]
    async fn age_drops_stale_used_and_never_used_rows_at_the_cutoff() {
        let pool = pool().await;
        let cutoff = NOW - 10 * DAY;
        insert(&pool, "user", "42", "used_old", NOW, 3, Some(cutoff - 1)).await;
        insert(&pool, "user", "42", "used_edge", 1, 1, Some(cutoff)).await;
        insert(&pool, "user", "42", "used_fresh", 1, 1, Some(NOW)).await;
        insert(&pool, "user", "42", "never_old", cutoff - 1, 0, None).await;
        insert(&pool, "user", "42", "never_edge", cutoff, 0, None).await;
        insert(&pool, "user", "42", "never_fresh", NOW, 0, None).await;
        insert(&pool, "user", "99", "never_old", cutoff - 1, 0, None).await;

        let summary = run(&pool, &[("user", "42")], NOW, 10 * DAY, 200)
            .await
            .unwrap();
        assert_eq!(summary["removed"].as_u64(), Some(2));
        assert_eq!(summary["kept"].as_i64(), Some(4));
        assert_eq!(
            keys(&pool).await,
            vec![
                ("user".into(), "42".into(), "never_edge".into()),
                ("user".into(), "42".into(), "never_fresh".into()),
                ("user".into(), "42".into(), "used_edge".into()),
                ("user".into(), "42".into(), "used_fresh".into()),
                ("user".into(), "99".into(), "never_old".into()),
            ]
        );
    }

    #[tokio::test]
    async fn cap_keeps_highest_usage_then_newer_recency_then_smaller_key() {
        let pool = pool().await;
        insert(&pool, "user", "7", "hot", 100, 5, Some(100)).await;
        insert(&pool, "user", "7", "mid_new", 10, 3, Some(500)).await;
        insert(&pool, "user", "7", "mid_old", 900, 3, Some(400)).await;
        insert(&pool, "user", "7", "low_b", 800, 1, None).await;
        insert(&pool, "user", "7", "low_a", 800, 1, None).await;
        insert(&pool, "group", "7", "rule", 1, 0, None).await;

        let summary = run(&pool, &[("user", "7")], NOW, i64::MAX, 2)
            .await
            .unwrap();
        assert_eq!(summary["removed"].as_u64(), Some(3));
        assert_eq!(summary["kept"].as_i64(), Some(2));
        assert_eq!(
            keys(&pool).await,
            vec![
                ("group".into(), "7".into(), "rule".into()),
                ("user".into(), "7".into(), "hot".into()),
                ("user".into(), "7".into(), "mid_old".into()),
            ]
        );
        insert(&pool, "user", "8", "m", 800, 1, None).await;
        insert(&pool, "user", "8", "b", 800, 1, None).await;
        insert(&pool, "user", "8", "a", 800, 1, None).await;
        let tied = run(&pool, &[("user", "8")], NOW, i64::MAX, 2)
            .await
            .unwrap();
        assert_eq!(tied["removed"].as_u64(), Some(1));
        assert_eq!(tied["kept"].as_i64(), Some(2));
        assert!(keys(&pool)
            .await
            .iter()
            .any(|row| row.0 == "user" && row.1 == "8" && row.2 == "a"));
        assert!(keys(&pool)
            .await
            .iter()
            .any(|row| row.0 == "user" && row.1 == "8" && row.2 == "b"));
        assert!(!keys(&pool)
            .await
            .iter()
            .any(|row| row.0 == "user" && row.1 == "8" && row.2 == "m"));
        let wiped = run(&pool, &[("user", "7")], NOW, i64::MAX, 0)
            .await
            .unwrap();
        assert_eq!(wiped["removed"].as_u64(), Some(2));
        assert_eq!(wiped["kept"].as_i64(), Some(0));
        assert_eq!(
            keys(&pool).await,
            vec![
                ("group".into(), "7".into(), "rule".into()),
                ("user".into(), "8".into(), "a".into()),
                ("user".into(), "8".into(), "b".into()),
            ]
        );
    }

    #[tokio::test]
    async fn tool_consolidate_stays_inside_the_scopes_the_caller_can_see() {
        let pool = pool().await;
        let old = NOW - 11 * DAY;
        insert(&pool, "account", "", "tone", old, 0, None).await;
        insert(&pool, "group", "7", "rule", old, 0, None).await;
        insert(&pool, "user", "42", "nick", old, 0, None).await;
        insert(&pool, "user", "99", "nick", old, 0, None).await;
        insert(&pool, "group", "8", "rule", old, 0, None).await;

        let private = call_limited(&pool, "consolidate", &speaker(42, None), NOW, 10 * DAY, 200)
            .await
            .unwrap();
        assert_eq!(private["removed"].as_u64(), Some(2));
        assert_eq!(private["kept"].as_i64(), Some(0));
        assert_eq!(
            keys(&pool).await,
            vec![
                ("group".into(), "7".into(), "rule".into()),
                ("group".into(), "8".into(), "rule".into()),
                ("user".into(), "99".into(), "nick".into()),
            ]
        );

        let grouped = call_limited(
            &pool,
            "consolidate",
            &speaker(42, Some(7)),
            NOW,
            10 * DAY,
            200,
        )
        .await
        .unwrap();
        assert_eq!(grouped["removed"].as_u64(), Some(1));
        assert_eq!(grouped["kept"].as_i64(), Some(0));
        assert!(
            call_limited(&pool, "consolidate", &speaker(42, None), NOW, 10 * DAY, 200,)
                .await
                .unwrap()["removed"]
                .as_u64()
                == Some(0)
        );

        let mut only_group = speaker(42, Some(7));
        only_group["scope"] = json!("group");
        insert(&pool, "group", "7", "fresh", NOW, 0, None).await;
        insert(&pool, "user", "42", "fresh", old, 0, None).await;
        let one = call_limited(&pool, "consolidate", &only_group, NOW, 10 * DAY, 200)
            .await
            .unwrap();
        assert_eq!(one["removed"].as_u64(), Some(0));
        assert_eq!(one["kept"].as_i64(), Some(1));
        assert!(keys(&pool)
            .await
            .iter()
            .any(|row| row.2 == "fresh" && row.0 == "user"));

        let err = call_limited(
            &pool,
            "consolidate",
            &{
                let mut args = speaker(42, None);
                args["scope"] = json!("group");
                args
            },
            NOW,
            10 * DAY,
            200,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not a group"));
    }

    #[tokio::test]
    async fn start_hook_skips_until_enabled_then_drops_stale_rows_in_every_scope() {
        let db = pool().await;
        let old = NOW - 11 * DAY;
        insert(&db, "user", "1", "a", old, 0, None).await;
        insert(&db, "user", "2", "b", NOW, 1, Some(NOW)).await;
        consolidate_if(&db, false, NOW, 10 * DAY, 200)
            .await
            .unwrap();
        assert_eq!(keys(&db).await.len(), 2);
        consolidate_if(&db, true, NOW, 10 * DAY, 200).await.unwrap();
        assert_eq!(
            keys(&db).await,
            vec![("user".into(), "2".into(), "b".into())]
        );
        let again = run_all(&db, NOW, 10 * DAY, 200).await.unwrap();
        assert_eq!(again["removed"].as_u64(), Some(0));
        assert_eq!(again["kept"].as_i64(), Some(1));

        let bare = pool().await;
        let empty = run_all(&bare, NOW, 10 * DAY, 200).await.unwrap();
        assert_eq!(empty["removed"].as_u64(), Some(0));
        assert_eq!(empty["kept"].as_i64(), Some(0));
    }
}
