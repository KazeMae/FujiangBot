use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::ConnectOptions;

use crate::admin_agent::{agent_db_path, has_table, open_read, unix_now};
use crate::runtime::AppState;

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

const KEY_MAX: usize = 64;
const VALUE_MAX: usize = 500;
const SOURCE_MAX: usize = 200;
const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 200;
const MEMORY_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS layered_memories (
    scope_kind TEXT NOT NULL CHECK (scope_kind IN ('account', 'group', 'user')),
    scope_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    source TEXT,
    UNIQUE (scope_kind, scope_id, key)
)";

#[derive(Debug, Serialize, sqlx::FromRow, PartialEq, Eq)]
struct MemoryRow {
    scope_kind: String,
    scope_id: String,
    key: String,
    value: String,
    updated_at: i64,
    source: Option<String>,
}

#[derive(Serialize)]
struct MemoryList {
    rows: Vec<MemoryRow>,
}

#[derive(Serialize)]
struct Deleted {
    deleted: u64,
}

struct MemoryKey {
    scope_kind: String,
    scope_id: String,
    key: String,
}

struct ListFilter {
    scope_kind: Option<String>,
    scope_id: Option<String>,
    limit: i64,
}

enum SourceUpdate {
    Keep,
    Set(Option<String>),
}

struct Upsert {
    key: MemoryKey,
    value: String,
    source: SourceUpdate,
}

pub(crate) async fn get_memories(
    State(state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
) -> Response {
    let filter = match list_filter(query.as_deref()) {
        Ok(filter) => filter,
        Err(error) => return bad(error),
    };
    let path = agent_db_path(&state.ctx.store);
    match list_memories(&path, &filter).await {
        Ok(rows) => Json(MemoryList { rows }).into_response(),
        Err(e) => server_error(e),
    }
}

pub(crate) async fn get_memory(
    State(state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
) -> Response {
    let key = match memory_key_from_delete(query.as_deref(), &[]) {
        Ok(key) => key,
        Err(error) => return bad(error),
    };
    let path = agent_db_path(&state.ctx.store);
    match memory_row(&path, &key).await {
        Ok(Some(row)) => Json(row).into_response(),
        Ok(None) => missing_memory(),
        Err(e) => server_error(e),
    }
}

pub(crate) async fn put_memory(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    // 运维口：按请求里的 scope 写入，不走 agent 工具的宿主注入。
    let write = match upsert_from_json(&body) {
        Ok(write) => write,
        Err(error) => return bad(error),
    };
    let path = agent_db_path(&state.ctx.store);
    match upsert_memory(&path, &write, unix_now()).await {
        Ok(row) => Json(row).into_response(),
        Err(e) => server_error(e),
    }
}

pub(crate) async fn delete_memory(
    State(state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    let key = match memory_key_from_delete(query.as_deref(), &body) {
        Ok(key) => key,
        Err(error) => return bad(error),
    };
    let path = agent_db_path(&state.ctx.store);
    match delete_memory_row(&path, &key).await {
        Ok(deleted) => Json(Deleted { deleted }).into_response(),
        Err(e) => server_error(e),
    }
}

fn bad(error: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(ErrorBody { error })).into_response()
}

fn missing_memory() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: "没有这条记忆".into(),
        }),
    )
        .into_response()
}

fn server_error(err: anyhow::Error) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            error: format!("{err:#}"),
        }),
    )
        .into_response()
}

async fn open_write(path: &Path, create: bool) -> anyhow::Result<sqlx::SqliteConnection> {
    if create {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
    }
    Ok(SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create)
        .busy_timeout(std::time::Duration::from_secs(5))
        .connect()
        .await?)
}

async fn list_memories(path: &Path, filter: &ListFilter) -> anyhow::Result<Vec<MemoryRow>> {
    let Some(mut conn) = open_read(path).await? else {
        return Ok(Vec::new());
    };
    if !has_table(&mut conn, "layered_memories").await? {
        return Ok(Vec::new());
    }
    let rows = match (&filter.scope_kind, &filter.scope_id) {
        (None, None) => {
            sqlx::query_as::<_, MemoryRow>(
                "SELECT scope_kind, scope_id, key, value, updated_at, source
                 FROM layered_memories ORDER BY scope_kind, scope_id, key LIMIT ?1",
            )
            .bind(filter.limit)
            .fetch_all(&mut conn)
            .await?
        }
        (Some(kind), None) => {
            sqlx::query_as::<_, MemoryRow>(
                "SELECT scope_kind, scope_id, key, value, updated_at, source
                 FROM layered_memories WHERE scope_kind = ?1
                 ORDER BY scope_kind, scope_id, key LIMIT ?2",
            )
            .bind(kind)
            .bind(filter.limit)
            .fetch_all(&mut conn)
            .await?
        }
        (None, Some(id)) => {
            sqlx::query_as::<_, MemoryRow>(
                "SELECT scope_kind, scope_id, key, value, updated_at, source
                 FROM layered_memories WHERE scope_id = ?1
                 ORDER BY scope_kind, scope_id, key LIMIT ?2",
            )
            .bind(id)
            .bind(filter.limit)
            .fetch_all(&mut conn)
            .await?
        }
        (Some(kind), Some(id)) => {
            sqlx::query_as::<_, MemoryRow>(
                "SELECT scope_kind, scope_id, key, value, updated_at, source
                 FROM layered_memories WHERE scope_kind = ?1 AND scope_id = ?2
                 ORDER BY scope_kind, scope_id, key LIMIT ?3",
            )
            .bind(kind)
            .bind(id)
            .bind(filter.limit)
            .fetch_all(&mut conn)
            .await?
        }
    };
    Ok(rows)
}

async fn memory_row(path: &Path, key: &MemoryKey) -> anyhow::Result<Option<MemoryRow>> {
    let Some(mut conn) = open_read(path).await? else {
        return Ok(None);
    };
    if !has_table(&mut conn, "layered_memories").await? {
        return Ok(None);
    }
    Ok(sqlx::query_as::<_, MemoryRow>(
        "SELECT scope_kind, scope_id, key, value, updated_at, source
         FROM layered_memories
         WHERE scope_kind = ?1 AND scope_id = ?2 AND key = ?3",
    )
    .bind(&key.scope_kind)
    .bind(&key.scope_id)
    .bind(&key.key)
    .fetch_optional(&mut conn)
    .await?)
}

async fn upsert_memory(path: &Path, write: &Upsert, now: i64) -> anyhow::Result<MemoryRow> {
    let mut conn = open_write(path, true).await?;
    if !has_table(&mut conn, "layered_memories").await? {
        sqlx::query(MEMORY_SCHEMA).execute(&mut conn).await?;
    }
    match &write.source {
        SourceUpdate::Keep => {
            sqlx::query(
                "INSERT INTO layered_memories (scope_kind, scope_id, key, value, updated_at, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL)
                 ON CONFLICT(scope_kind, scope_id, key) DO UPDATE SET
                    value = excluded.value,
                    updated_at = excluded.updated_at",
            )
            .bind(&write.key.scope_kind)
            .bind(&write.key.scope_id)
            .bind(&write.key.key)
            .bind(&write.value)
            .bind(now)
            .execute(&mut conn)
            .await?;
        }
        SourceUpdate::Set(source) => {
            sqlx::query(
                "INSERT INTO layered_memories (scope_kind, scope_id, key, value, updated_at, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(scope_kind, scope_id, key) DO UPDATE SET
                    value = excluded.value,
                    updated_at = excluded.updated_at,
                    source = excluded.source",
            )
            .bind(&write.key.scope_kind)
            .bind(&write.key.scope_id)
            .bind(&write.key.key)
            .bind(&write.value)
            .bind(now)
            .bind(source)
            .execute(&mut conn)
            .await?;
        }
    }
    let row = sqlx::query_as::<_, MemoryRow>(
        "SELECT scope_kind, scope_id, key, value, updated_at, source
         FROM layered_memories
         WHERE scope_kind = ?1 AND scope_id = ?2 AND key = ?3",
    )
    .bind(&write.key.scope_kind)
    .bind(&write.key.scope_id)
    .bind(&write.key.key)
    .fetch_one(&mut conn)
    .await?;
    Ok(row)
}

async fn delete_memory_row(path: &Path, key: &MemoryKey) -> anyhow::Result<u64> {
    if !path.is_file() {
        return Ok(0);
    }
    let mut conn = open_write(path, false).await?;
    if !has_table(&mut conn, "layered_memories").await? {
        return Ok(0);
    }
    let done = sqlx::query(
        "DELETE FROM layered_memories
         WHERE scope_kind = ?1 AND scope_id = ?2 AND key = ?3",
    )
    .bind(&key.scope_kind)
    .bind(&key.scope_id)
    .bind(&key.key)
    .execute(&mut conn)
    .await?;
    Ok(done.rows_affected())
}

fn list_filter(raw: Option<&str>) -> Result<ListFilter, String> {
    let scope_kind = match query_param(raw, "scope_kind") {
        None => None,
        Some(text) if text.trim().is_empty() => None,
        Some(text) => {
            let text = text.trim();
            check_kind(text)?;
            Some(text.to_string())
        }
    };
    let scope_id = match query_param(raw, "scope_id") {
        None => None,
        Some(text) => {
            let text = text.trim();
            if text.chars().count() > KEY_MAX {
                return Err("scope_id 过长".into());
            }
            if scope_kind.as_deref() == Some("account") && !text.is_empty() {
                return Err("account 的 scope_id 须为空".into());
            }
            Some(text.to_string())
        }
    };
    Ok(ListFilter {
        scope_kind,
        scope_id,
        limit: parse_limit(query_param(raw, "limit").as_deref())?,
    })
}

fn memory_key_from_delete(query: Option<&str>, body: &[u8]) -> Result<MemoryKey, String> {
    let mut kind = query_param(query, "scope_kind");
    let mut scope_id = query_param(query, "scope_id");
    let mut key = query_param(query, "key");
    if !body.iter().all(u8::is_ascii_whitespace) {
        let value: Value =
            serde_json::from_slice(body).map_err(|_| "body 不是 JSON".to_string())?;
        let obj = value
            .as_object()
            .ok_or_else(|| "body 须为 JSON 对象".to_string())?;
        if obj.contains_key("scope_kind") {
            kind = Some(required_str(obj, "scope_kind")?);
        }
        if obj.contains_key("scope_id") {
            scope_id = Some(required_str(obj, "scope_id")?);
        }
        if obj.contains_key("key") {
            key = Some(required_str(obj, "key")?);
        }
    }
    parse_key(
        kind.as_deref().ok_or("缺少 scope_kind")?,
        scope_id.as_deref().ok_or("缺少 scope_id")?,
        key.as_deref().ok_or("缺少 key")?,
    )
}

fn upsert_from_json(body: &Value) -> Result<Upsert, String> {
    let obj = body
        .as_object()
        .ok_or_else(|| "body 须为 JSON 对象".to_string())?;
    let key = parse_key(
        &required_str(obj, "scope_kind")?,
        &required_str(obj, "scope_id")?,
        &required_str(obj, "key")?,
    )?;
    let value = parse_value(&required_str(obj, "value")?)?;
    let source = if obj.contains_key("source") {
        match obj.get("source") {
            Some(Value::Null) => SourceUpdate::Set(None),
            Some(Value::String(text)) => {
                let text = text.trim();
                if text.is_empty() {
                    SourceUpdate::Set(None)
                } else if text.chars().count() > SOURCE_MAX {
                    return Err("source 过长".into());
                } else {
                    SourceUpdate::Set(Some(text.to_string()))
                }
            }
            _ => return Err("source 须为字符串".into()),
        }
    } else {
        SourceUpdate::Keep
    };
    Ok(Upsert { key, value, source })
}

fn required_str(obj: &Map<String, Value>, name: &str) -> Result<String, String> {
    match obj.get(name) {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(format!("{name} 须为字符串")),
        None => Err(format!("缺少 {name}")),
    }
}

fn check_kind(kind: &str) -> Result<(), String> {
    match kind {
        "account" | "group" | "user" => Ok(()),
        _ => Err("scope_kind 须为 account、group 或 user".into()),
    }
}

fn parse_key(kind: &str, scope_id: &str, key: &str) -> Result<MemoryKey, String> {
    let kind = kind.trim();
    check_kind(kind)?;
    let scope_id = scope_id.trim();
    if scope_id.chars().count() > KEY_MAX {
        return Err("scope_id 过长".into());
    }
    if kind == "account" {
        if !scope_id.is_empty() {
            return Err("account 的 scope_id 须为空".into());
        }
    } else if scope_id.is_empty() {
        return Err("scope_id 不能为空".into());
    }
    let key = key.trim();
    let n = key.chars().count();
    if n == 0 || n > KEY_MAX {
        return Err("key 长度须为 1 到 64".into());
    }
    Ok(MemoryKey {
        scope_kind: kind.to_string(),
        scope_id: scope_id.to_string(),
        key: key.to_string(),
    })
}

fn parse_value(value: &str) -> Result<String, String> {
    let value = value.trim();
    let n = value.chars().count();
    if n == 0 || n > VALUE_MAX {
        return Err("value 长度须为 1 到 500".into());
    }
    Ok(value.to_string())
}

fn parse_limit(raw: Option<&str>) -> Result<i64, String> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(DEFAULT_LIMIT);
    };
    let n: i64 = text.parse().map_err(|_| "limit 须为正整数".to_string())?;
    if (1..=MAX_LIMIT).contains(&n) {
        Ok(n)
    } else {
        Err(format!("limit 须为 1 到 {MAX_LIMIT}"))
    }
}

fn query_param(raw: Option<&str>, name: &str) -> Option<String> {
    let raw = raw?;
    for pair in raw.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == name {
            return Some(form_decode(value));
        }
    }
    None
}

fn form_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::admin_agent::open_read;
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fj-admin-mem-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names = Vec::new();
        if dir.is_dir() {
            for ent in std::fs::read_dir(dir).unwrap() {
                names.push(ent.unwrap().file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        names
    }

    async fn seed_foreign(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true),
            )
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
        sqlx::query(
            "CREATE TABLE usage_daily (
                day INTEGER NOT NULL,
                session_key TEXT NOT NULL,
                user_id INTEGER NOT NULL,
                calls INTEGER NOT NULL,
                in_tokens INTEGER NOT NULL,
                out_tokens INTEGER NOT NULL,
                PRIMARY KEY (day, session_key, user_id)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO usage_daily (day, session_key, user_id, calls, in_tokens, out_tokens)
             VALUES (1, 'group:1', 9, 3, 4, 5)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    async fn foreign(path: &Path) -> (i64, i64, Vec<String>) {
        let mut conn = open_read(path).await.unwrap().unwrap();
        let updated: i64 =
            sqlx::query_scalar("SELECT updated_at FROM sessions WHERE session_key = 'group:1'")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        let calls: i64 = sqlx::query_scalar("SELECT calls FROM usage_daily WHERE user_id = 9")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&mut conn)
        .await
        .unwrap();
        (updated, calls, tables)
    }

    fn row(
        kind: &str,
        id: &str,
        key: &str,
        value: &str,
        at: i64,
        source: Option<&str>,
    ) -> MemoryRow {
        MemoryRow {
            scope_kind: kind.into(),
            scope_id: id.into(),
            key: key.into(),
            value: value.into(),
            updated_at: at,
            source: source.map(str::to_string),
        }
    }

    #[test]
    fn query_and_body_parse_scopes_without_caller_injection() {
        let all = list_filter(None).unwrap();
        assert!(all.scope_kind.is_none() && all.scope_id.is_none() && all.limit == 100);
        let filtered = list_filter(Some("scope_kind=user&scope_id=42&limit=2")).unwrap();
        assert_eq!(filtered.scope_kind.as_deref(), Some("user"));
        assert_eq!(filtered.scope_id.as_deref(), Some("42"));
        assert_eq!(filtered.limit, 2);
        assert!(list_filter(Some("scope_kind=&limit="))
            .unwrap()
            .scope_kind
            .is_none());
        assert!(list_filter(Some("scope_kind=nope")).is_err());
        assert!(list_filter(Some("scope_kind=account&scope_id=1")).is_err());
        assert!(list_filter(Some("limit=0")).is_err());
        assert!(list_filter(Some("limit=201")).is_err());
        assert!(list_filter(Some("limit=x")).is_err());
        let decoded = list_filter(Some("scope_id=%E5%B0%8F%E6%98%8E&limit=1")).unwrap();
        assert_eq!(decoded.scope_id.as_deref(), Some("小明"));

        let from_query =
            memory_key_from_delete(Some("scope_kind=account&scope_id=&key=tone"), b"  ").unwrap();
        assert_eq!(from_query.scope_kind, "account");
        assert_eq!(from_query.scope_id, "");
        assert_eq!(from_query.key, "tone");
        let over = memory_key_from_delete(
            Some("scope_kind=user&scope_id=1&key=a"),
            br#"{"key":" nick ","scope_id":"999"}"#,
        )
        .unwrap();
        assert_eq!(over.scope_kind, "user");
        assert_eq!(over.scope_id, "999");
        assert_eq!(over.key, "nick");
        assert!(memory_key_from_delete(Some("scope_kind=user&key=a"), b"").is_err());
        assert!(memory_key_from_delete(None, b"{").is_err());
        assert!(memory_key_from_delete(None, b"[]").is_err());
        assert!(
            memory_key_from_delete(None, br#"{"scope_id":1,"scope_kind":"user","key":"a"}"#)
                .is_err()
        );

        let write = upsert_from_json(&json!({
            "scope_kind": " user ",
            "scope_id": " 999 ",
            "key": " nick ",
            "value": " 别人 ",
            "user_id": 1,
            "group_id": 7
        }))
        .unwrap();
        assert_eq!(write.key.scope_kind, "user");
        assert_eq!(write.key.scope_id, "999");
        assert_eq!(write.key.key, "nick");
        assert_eq!(write.value, "别人");
        assert!(matches!(write.source, SourceUpdate::Keep));
        let cleared = upsert_from_json(&json!({
            "scope_kind": "group",
            "scope_id": "7",
            "key": "rule",
            "value": "短",
            "source": "  "
        }))
        .unwrap();
        assert!(matches!(cleared.source, SourceUpdate::Set(None)));
        let set = upsert_from_json(&json!({
            "scope_kind": "account",
            "scope_id": "",
            "key": "tone",
            "value": "短",
            "source": " admin "
        }))
        .unwrap();
        assert!(matches!(set.source, SourceUpdate::Set(Some(text)) if text == "admin"));
        assert!(upsert_from_json(
            &json!({"scope_kind":"user","scope_id":"1","key":"a","value":" "})
        )
        .is_err());
        assert!(upsert_from_json(&json!({
            "scope_kind": "user",
            "scope_id": "1",
            "key": "a",
            "value": "测".repeat(501)
        }))
        .is_err());
        assert!(upsert_from_json(
            &json!({"scope_kind":"account","scope_id":"1","key":"a","value":"x"})
        )
        .is_err());
    }

    #[tokio::test]
    async fn reads_are_empty_and_create_nothing() {
        let scratch = Scratch::new();
        let missing = scratch.0.join("missing").join("plugin.sqlite");
        let filter = list_filter(None).unwrap();
        assert!(list_memories(&missing, &filter).await.unwrap().is_empty());
        let key = memory_key_from_delete(Some("scope_kind=user&scope_id=1&key=a"), b"").unwrap();
        assert!(memory_row(&missing, &key).await.unwrap().is_none());
        assert_eq!(delete_memory_row(&missing, &key).await.unwrap(), 0);
        assert!(!missing.exists());
        assert!(!scratch.0.join("missing").exists());
        assert!(names_in(&scratch.0).is_empty());

        let path = scratch.0.join("plugin.sqlite");
        seed_foreign(&path).await;
        let before = names_in(&scratch.0);
        let (updated, calls, tables) = foreign(&path).await;
        assert!(list_memories(&path, &filter).await.unwrap().is_empty());
        assert!(memory_row(&path, &key).await.unwrap().is_none());
        assert_eq!(delete_memory_row(&path, &key).await.unwrap(), 0);
        assert_eq!(names_in(&scratch.0), before);
        assert!(!tables.iter().any(|name| name == "layered_memories"));
        assert_eq!(foreign(&path).await, (updated, calls, tables));
    }

    #[tokio::test]
    async fn upsert_any_scope_and_delete_one_key_leave_other_tables() {
        let scratch = Scratch::new();
        let path = scratch.0.join("plugin.sqlite");
        seed_foreign(&path).await;
        let (updated, calls, tables_before) = foreign(&path).await;

        let user = upsert_from_json(&json!({
            "scope_kind": "user",
            "scope_id": "999",
            "key": "nick",
            "value": "别人",
            "user_id": 1,
            "source": "group:7"
        }))
        .unwrap();
        let stored = upsert_memory(&path, &user, 10).await.unwrap();
        assert_eq!(
            stored,
            row("user", "999", "nick", "别人", 10, Some("group:7"))
        );

        let group = upsert_from_json(&json!({
            "scope_kind": "group",
            "scope_id": "7",
            "key": "rule",
            "value": "别刷屏"
        }))
        .unwrap();
        assert_eq!(
            upsert_memory(&path, &group, 11).await.unwrap(),
            row("group", "7", "rule", "别刷屏", 11, None)
        );
        let account = upsert_from_json(&json!({
            "scope_kind": "account",
            "scope_id": "",
            "key": "tone",
            "value": "短",
            "source": null
        }))
        .unwrap();
        assert_eq!(
            upsert_memory(&path, &account, 12).await.unwrap(),
            row("account", "", "tone", "短", 12, None)
        );

        let again = upsert_from_json(&json!({
            "scope_kind": "user",
            "scope_id": "999",
            "key": "nick",
            "value": "小明"
        }))
        .unwrap();
        assert_eq!(
            upsert_memory(&path, &again, 20).await.unwrap(),
            row("user", "999", "nick", "小明", 20, Some("group:7"))
        );
        let clear = upsert_from_json(&json!({
            "scope_kind": "user",
            "scope_id": "999",
            "key": "nick",
            "value": "小明",
            "source": ""
        }))
        .unwrap();
        assert_eq!(
            upsert_memory(&path, &clear, 21).await.unwrap(),
            row("user", "999", "nick", "小明", 21, None)
        );

        let long = "测".repeat(500);
        let capped = upsert_from_json(&json!({
            "scope_kind": "user",
            "scope_id": "999",
            "key": "bio",
            "value": long
        }))
        .unwrap();
        assert_eq!(
            upsert_memory(&path, &capped, 22)
                .await
                .unwrap()
                .value
                .chars()
                .count(),
            500
        );

        let listed = list_memories(&path, &list_filter(Some("limit=2")).unwrap())
            .await
            .unwrap();
        assert_eq!(
            listed,
            vec![
                row("account", "", "tone", "短", 12, None),
                row("group", "7", "rule", "别刷屏", 11, None),
            ]
        );
        let only_user = list_memories(
            &path,
            &list_filter(Some("scope_kind=user&scope_id=999")).unwrap(),
        )
        .await
        .unwrap();
        assert!(only_user
            .iter()
            .all(|item| item.scope_kind == "user" && item.scope_id == "999"));
        assert_eq!(only_user.len(), 2);
        let one =
            memory_key_from_delete(Some("scope_kind=group&scope_id=7&key=rule"), b"").unwrap();
        assert_eq!(
            memory_row(&path, &one).await.unwrap(),
            Some(row("group", "7", "rule", "别刷屏", 11, None))
        );
        let gone =
            memory_key_from_delete(Some("scope_kind=group&scope_id=7&key=missing"), b"").unwrap();
        assert!(memory_row(&path, &gone).await.unwrap().is_none());

        assert_eq!(delete_memory_row(&path, &one).await.unwrap(), 1);
        assert_eq!(delete_memory_row(&path, &one).await.unwrap(), 0);
        assert!(memory_row(
            &path,
            &memory_key_from_delete(Some("scope_kind=user&scope_id=999&key=nick"), b"").unwrap()
        )
        .await
        .unwrap()
        .is_some());

        let (updated_after, calls_after, tables_after) = foreign(&path).await;
        assert_eq!(updated_after, updated);
        assert_eq!(calls_after, calls);
        assert_eq!(
            tables_before,
            vec!["sessions".to_string(), "usage_daily".to_string()]
        );
        assert_eq!(
            tables_after,
            vec![
                "layered_memories".to_string(),
                "sessions".to_string(),
                "usage_daily".to_string()
            ]
        );
    }
}
