use anyhow::{bail, Context};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};

const KEY_MAX: usize = 64;
const VALUE_MAX: usize = 500;
const LIST_LIMIT: i64 = 100;

pub(crate) async fn ensure_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS layered_memories (
            scope_kind TEXT NOT NULL CHECK (scope_kind IN ('account', 'group', 'user')),
            scope_id TEXT NOT NULL,
            key TEXT NOT NULL,
            value TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            source TEXT,
            UNIQUE (scope_kind, scope_id, key)
        )",
    )
    .execute(pool)
    .await
    .context("create layered_memories")?;
    Ok(())
}

struct Caller {
    user_id: i64,
    group_id: Option<i64>,
    source: Option<String>,
}

pub(crate) async fn call(
    pool: &SqlitePool,
    method: &str,
    args: &Value,
    now: i64,
) -> anyhow::Result<Value> {
    let caller = caller_from(args)?;
    match method {
        "list" => list(pool, &caller, args).await,
        "get" => get(pool, &caller, args).await,
        "upsert" => upsert(pool, &caller, args, now).await,
        "forget" => forget(pool, &caller, args).await,
        _ => bail!("unknown memory method"),
    }
}

fn caller_from(args: &Value) -> anyhow::Result<Caller> {
    let user_id = args
        .get("user_id")
        .and_then(Value::as_i64)
        .context("missing user_id")?;
    let group_id = args.get("group_id").and_then(Value::as_i64);
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    Ok(Caller {
        user_id,
        group_id,
        source,
    })
}

fn scope_arg(args: &Value) -> Option<&str> {
    args.get("scope")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn resolve(caller: &Caller, scope: &str) -> anyhow::Result<(&'static str, String)> {
    match scope {
        "account" => Ok(("account", String::new())),
        "group" => {
            let id = caller.group_id.context("not a group")?;
            Ok(("group", id.to_string()))
        }
        "user" => Ok(("user", caller.user_id.to_string())),
        _ => bail!("bad scope"),
    }
}

fn default_pairs(caller: &Caller) -> Vec<(&'static str, String)> {
    let mut pairs = vec![("account", String::new())];
    if let Some(id) = caller.group_id {
        pairs.push(("group", id.to_string()));
    }
    pairs.push(("user", caller.user_id.to_string()));
    pairs
}

fn required_key(args: &Value) -> anyhow::Result<String> {
    let raw = args
        .get("key")
        .and_then(Value::as_str)
        .context("missing key")?;
    let key = raw.trim();
    let n = key.chars().count();
    if n == 0 || n > KEY_MAX {
        bail!("bad key");
    }
    Ok(key.to_string())
}

fn required_value(args: &Value) -> anyhow::Result<String> {
    let raw = args
        .get("value")
        .and_then(Value::as_str)
        .context("missing value")?;
    let value = raw.trim();
    let n = value.chars().count();
    if n == 0 || n > VALUE_MAX {
        bail!("bad value");
    }
    Ok(value.to_string())
}

async fn list(pool: &SqlitePool, caller: &Caller, args: &Value) -> anyhow::Result<Value> {
    let pairs = match scope_arg(args) {
        Some(scope) => vec![resolve(caller, scope)?],
        None => default_pairs(caller),
    };
    let mut items = Vec::new();
    for (kind, id) in pairs {
        let rows = sqlx::query(
            "SELECT key, updated_at FROM layered_memories
             WHERE scope_kind = ?1 AND scope_id = ?2
             ORDER BY key
             LIMIT ?3",
        )
        .bind(kind)
        .bind(&id)
        .bind(LIST_LIMIT)
        .fetch_all(pool)
        .await
        .context("list memories")?;
        for row in rows {
            let key: String = row.try_get(0)?;
            let updated_at: i64 = row.try_get(1)?;
            items.push(json!({
                "scope": kind,
                "scope_id": id,
                "key": key,
                "updated_at": updated_at,
            }));
        }
    }
    Ok(Value::Array(items))
}

async fn get(pool: &SqlitePool, caller: &Caller, args: &Value) -> anyhow::Result<Value> {
    let scope = scope_arg(args).context("missing scope")?;
    let key = required_key(args)?;
    let (kind, id) = resolve(caller, scope)?;
    let row = sqlx::query(
        "SELECT value, updated_at, source FROM layered_memories
         WHERE scope_kind = ?1 AND scope_id = ?2 AND key = ?3",
    )
    .bind(kind)
    .bind(&id)
    .bind(&key)
    .fetch_optional(pool)
    .await
    .context("get memory")?;
    let Some(row) = row else {
        return Ok(Value::Null);
    };
    let value: String = row.try_get(0)?;
    let updated_at: i64 = row.try_get(1)?;
    let source: Option<String> = row.try_get(2)?;
    Ok(row_json(kind, &id, &key, &value, updated_at, source))
}

async fn upsert(
    pool: &SqlitePool,
    caller: &Caller,
    args: &Value,
    now: i64,
) -> anyhow::Result<Value> {
    let scope = scope_arg(args).context("missing scope")?;
    let key = required_key(args)?;
    let value = required_value(args)?;
    let (kind, id) = resolve(caller, scope)?;
    sqlx::query(
        "INSERT INTO layered_memories (scope_kind, scope_id, key, value, updated_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(scope_kind, scope_id, key) DO UPDATE SET
            value = excluded.value,
            updated_at = excluded.updated_at,
            source = excluded.source",
    )
    .bind(kind)
    .bind(&id)
    .bind(&key)
    .bind(&value)
    .bind(now)
    .bind(caller.source.as_deref())
    .execute(pool)
    .await
    .context("upsert memory")?;
    Ok(row_json(
        kind,
        &id,
        &key,
        &value,
        now,
        caller.source.clone(),
    ))
}

async fn forget(pool: &SqlitePool, caller: &Caller, args: &Value) -> anyhow::Result<Value> {
    let scope = scope_arg(args).context("missing scope")?;
    let key = required_key(args)?;
    let (kind, id) = resolve(caller, scope)?;
    let prefix = args.get("prefix").and_then(Value::as_bool).unwrap_or(false);
    let sql = if prefix {
        "DELETE FROM layered_memories
         WHERE scope_kind = ?1 AND scope_id = ?2 AND substr(key, 1, length(?3)) = ?3"
    } else {
        "DELETE FROM layered_memories
         WHERE scope_kind = ?1 AND scope_id = ?2 AND key = ?3"
    };
    let done = sqlx::query(sql)
        .bind(kind)
        .bind(&id)
        .bind(&key)
        .execute(pool)
        .await
        .context("forget memory")?;
    Ok(json!({ "deleted": done.rows_affected() }))
}

fn row_json(
    kind: &str,
    id: &str,
    key: &str,
    value: &str,
    updated_at: i64,
    source: Option<String>,
) -> Value {
    json!({
        "scope": kind,
        "scope_id": id,
        "key": key,
        "value": value,
        "updated_at": updated_at,
        "source": source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        ensure_schema(&pool).await.unwrap();
        pool
    }

    fn host(user_id: i64, group_id: Option<i64>, source: Option<&str>) -> Value {
        let mut args = json!({ "user_id": user_id });
        if let Some(id) = group_id {
            args["group_id"] = json!(id);
        }
        if let Some(source) = source {
            args["source"] = json!(source);
        }
        args
    }

    fn with(mut args: Value, extra: Value) -> Value {
        let obj = args.as_object_mut().unwrap();
        for (key, value) in extra.as_object().unwrap() {
            obj.insert(key.clone(), value.clone());
        }
        args
    }

    fn triples(value: &Value) -> Vec<(String, String, String)> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["scope"].as_str().unwrap().to_string(),
                    row["scope_id"].as_str().unwrap().to_string(),
                    row["key"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    async fn put(pool: &SqlitePool, args: &Value, now: i64) {
        call(pool, "upsert", args, now).await.unwrap();
    }

    #[tokio::test]
    async fn old_db_gains_the_table_and_keeps_the_session_row() {
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
        crate::session::ensure_schema(&pool).await.unwrap();
        crate::session::ensure_schema(&pool).await.unwrap();
        let cols: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('layered_memories') ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            cols,
            vec![
                "key".to_string(),
                "scope_id".to_string(),
                "scope_kind".to_string(),
                "source".to_string(),
                "updated_at".to_string(),
                "value".to_string(),
            ]
        );
        let summary: Option<String> =
            sqlx::query_scalar("SELECT summary FROM sessions WHERE session_key = 'group:1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(summary, None);
        let kept: i64 =
            sqlx::query_scalar("SELECT updated_at FROM sessions WHERE session_key = 'group:1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(kept, 5);
        put(
            &pool,
            &with(
                host(9, Some(1), Some("group:1")),
                json!({"scope": "user", "key": "nick", "value": "小明"}),
            ),
            8,
        )
        .await;
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM layered_memories")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn list_scopes_and_forget_stay_inside_the_caller() {
        let pool = pool().await;
        let group = host(42, Some(7), Some("group:7"));
        let other = host(99, Some(7), Some("group:7"));
        let other_group = host(42, Some(8), Some("group:8"));
        put(
            &pool,
            &with(
                group.clone(),
                json!({"scope": "account", "key": "tone", "value": "短"}),
            ),
            1,
        )
        .await;
        put(
            &pool,
            &with(
                group.clone(),
                json!({"scope": "group", "key": "rule", "value": "别刷屏"}),
            ),
            2,
        )
        .await;
        put(
            &pool,
            &with(
                group.clone(),
                json!({"scope": "user", "key": "nick", "value": "小明"}),
            ),
            3,
        )
        .await;
        put(
            &pool,
            &with(
                group.clone(),
                json!({"scope": "user", "key": "ab", "value": "甲"}),
            ),
            4,
        )
        .await;
        put(
            &pool,
            &with(
                group.clone(),
                json!({"scope": "user", "key": "abc", "value": "乙"}),
            ),
            5,
        )
        .await;
        put(
            &pool,
            &with(
                group.clone(),
                json!({"scope": "user", "key": "b", "value": "丙"}),
            ),
            6,
        )
        .await;
        put(
            &pool,
            &with(
                other.clone(),
                json!({"scope": "user", "key": "nick", "value": "别人"}),
            ),
            7,
        )
        .await;
        put(
            &pool,
            &with(
                other_group,
                json!({"scope": "group", "key": "rule", "value": "他群"}),
            ),
            8,
        )
        .await;

        let listed = call(&pool, "list", &group, 9).await.unwrap();
        assert_eq!(
            triples(&listed),
            vec![
                ("account".into(), "".into(), "tone".into()),
                ("group".into(), "7".into(), "rule".into()),
                ("user".into(), "42".into(), "ab".into()),
                ("user".into(), "42".into(), "abc".into()),
                ("user".into(), "42".into(), "b".into()),
                ("user".into(), "42".into(), "nick".into()),
            ]
        );
        let only_user = call(
            &pool,
            "list",
            &with(group.clone(), json!({"scope": "user"})),
            9,
        )
        .await
        .unwrap();
        assert!(triples(&only_user).iter().all(|row| row.0 == "user"));
        let private_list = call(&pool, "list", &host(42, None, Some("private")), 9)
            .await
            .unwrap();
        assert_eq!(
            triples(&private_list),
            vec![
                ("account".into(), "".into(), "tone".into()),
                ("user".into(), "42".into(), "ab".into()),
                ("user".into(), "42".into(), "abc".into()),
                ("user".into(), "42".into(), "b".into()),
                ("user".into(), "42".into(), "nick".into()),
            ]
        );

        let exact = call(
            &pool,
            "forget",
            &with(group.clone(), json!({"scope": "user", "key": "ab"})),
            10,
        )
        .await
        .unwrap();
        assert_eq!(exact["deleted"].as_u64(), Some(1));
        let still: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM layered_memories WHERE scope_kind = 'user' AND scope_id = '42' AND key = 'abc'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(still, 1);
        let prefixed = call(
            &pool,
            "forget",
            &with(
                group.clone(),
                json!({"scope": "user", "key": "a", "prefix": true}),
            ),
            11,
        )
        .await
        .unwrap();
        assert_eq!(prefixed["deleted"].as_u64(), Some(1));
        let other_kept = call(
            &pool,
            "get",
            &with(other, json!({"scope": "user", "key": "nick"})),
            12,
        )
        .await
        .unwrap();
        assert_eq!(other_kept["value"], "别人");
        let missing = call(
            &pool,
            "forget",
            &with(group.clone(), json!({"scope": "user", "key": "gone"})),
            13,
        )
        .await
        .unwrap();
        assert_eq!(missing["deleted"].as_u64(), Some(0));
        let gone = call(
            &pool,
            "get",
            &with(group, json!({"scope": "user", "key": "abc"})),
            14,
        )
        .await
        .unwrap();
        assert!(gone.is_null());
    }

    #[tokio::test]
    async fn upsert_overwrites_and_scopes_are_forced() {
        let pool = pool().await;
        let speaker = host(42, Some(7), Some("group:7"));
        let first = call(
            &pool,
            "upsert",
            &with(
                speaker.clone(),
                json!({"scope": "user", "key": " nick ", "value": " 小明 "}),
            ),
            10,
        )
        .await
        .unwrap();
        assert_eq!(first["scope"], "user");
        assert_eq!(first["scope_id"], "42");
        assert_eq!(first["key"], "nick");
        assert_eq!(first["value"], "小明");
        assert_eq!(first["updated_at"], 10);
        assert_eq!(first["source"], "group:7");
        let second = call(
            &pool,
            "upsert",
            &with(
                speaker.clone(),
                json!({"scope": "user", "key": "nick", "value": "小红"}),
            ),
            20,
        )
        .await
        .unwrap();
        assert_eq!(second["value"], "小红");
        assert_eq!(second["updated_at"], 20);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM layered_memories")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 1);

        let account = call(
            &pool,
            "upsert",
            &with(
                speaker.clone(),
                json!({"scope": "account", "key": "tone", "value": "短"}),
            ),
            21,
        )
        .await
        .unwrap();
        assert_eq!(account["scope_id"], "");
        let group = call(
            &pool,
            "upsert",
            &with(
                speaker,
                json!({"scope": "group", "key": "rule", "value": "别刷屏"}),
            ),
            22,
        )
        .await
        .unwrap();
        assert_eq!(group["scope"], "group");
        assert_eq!(group["scope_id"], "7");

        let private = host(42, None, None);
        let stored = call(
            &pool,
            "upsert",
            &with(
                private.clone(),
                json!({"scope": "user", "key": "food", "value": "面"}),
            ),
            23,
        )
        .await
        .unwrap();
        assert!(stored["source"].is_null());
        let err = call(
            &pool,
            "upsert",
            &with(
                private,
                json!({"scope": "group", "key": "rule", "value": "不行"}),
            ),
            24,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not a group"));
    }

    #[tokio::test]
    async fn limits_and_bad_args_fail() {
        let pool = pool().await;
        let speaker = host(1, Some(2), Some("group:2"));
        let long_ok = "测".repeat(VALUE_MAX);
        put(
            &pool,
            &with(
                speaker.clone(),
                json!({"scope": "user", "key": "v", "value": long_ok}),
            ),
            1,
        )
        .await;
        let too_long = "测".repeat(VALUE_MAX + 1);
        let err = call(
            &pool,
            "upsert",
            &with(
                speaker.clone(),
                json!({"scope": "user", "key": "v", "value": too_long}),
            ),
            2,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("bad value"));
        let key_ok = "k".repeat(KEY_MAX);
        put(
            &pool,
            &with(
                speaker.clone(),
                json!({"scope": "account", "key": key_ok, "value": "ok"}),
            ),
            3,
        )
        .await;
        let key_bad = "k".repeat(KEY_MAX + 1);
        assert!(call(
            &pool,
            "upsert",
            &with(
                speaker.clone(),
                json!({"scope": "account", "key": key_bad, "value": "ok"})
            ),
            4,
        )
        .await
        .is_err());
        for args in [
            json!({"scope": "user", "key": " ", "value": "x"}),
            json!({"scope": "user", "key": "a", "value": "  "}),
            json!({"scope": "nope", "key": "a", "value": "x"}),
            json!({"key": "a", "value": "x"}),
            json!({"scope": "user", "value": "x"}),
            json!({"scope": "user", "key": "a"}),
        ] {
            assert!(call(&pool, "upsert", &with(speaker.clone(), args), 5)
                .await
                .is_err());
        }
        assert!(call(&pool, "list", &json!({}), 6).await.is_err());
        assert!(call(&pool, "drop", &speaker, 7).await.is_err());
        let empty = call(
            &pool,
            "list",
            &with(host(3, None, None), json!({"scope": "user"})),
            8,
        )
        .await
        .unwrap();
        assert_eq!(empty, json!([]));
    }
}
