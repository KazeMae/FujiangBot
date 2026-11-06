use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::Value;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::ConnectOptions;

use crate::runtime::AppState;

const TOP_N: i64 = 20;

#[derive(Debug, Serialize, PartialEq, Eq)]
struct UsageReport {
    day: String,
    day_id: i64,
    totals: TokenTotals,
    by_user: Vec<UserUsage>,
    by_session: Vec<SessionUsage>,
}

#[derive(Debug, Serialize, sqlx::FromRow, PartialEq, Eq)]
struct TokenTotals {
    calls: i64,
    in_tokens: i64,
    out_tokens: i64,
}

#[derive(Debug, Serialize, sqlx::FromRow, PartialEq, Eq)]
struct UserUsage {
    user_id: i64,
    calls: i64,
    in_tokens: i64,
    out_tokens: i64,
}

#[derive(Debug, Serialize, sqlx::FromRow, PartialEq, Eq)]
struct SessionUsage {
    session_key: String,
    calls: i64,
    in_tokens: i64,
    out_tokens: i64,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct PersonasView {
    system_prompt: String,
    personas: Vec<PersonaItem>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct PersonaItem {
    name: String,
    system_prompt: String,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

pub(crate) async fn get_usage(
    State(state): State<Arc<AppState>>,
    RawQuery(query): RawQuery,
) -> Response {
    let day_text = query_day(query.as_deref());
    let (day, day_id) = match resolve_day(day_text.as_deref(), unix_now()) {
        Ok(pair) => pair,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(ErrorBody { error })).into_response();
        }
    };
    let path = agent_db_path(&state.ctx.store);
    match usage_report(&path, &day, day_id).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("{e:#}"),
            }),
        )
            .into_response(),
    }
}

pub(crate) async fn get_personas(State(state): State<Arc<AppState>>) -> Response {
    let cfg = state.cfg.read().await;
    let raw = cfg.plugins.configs.get("agent").unwrap_or(&Value::Null);
    Json(personas_from_config(raw)).into_response()
}

pub(crate) fn agent_db_path(store: &fujiang_store::Store) -> PathBuf {
    store
        .data_dir()
        .join("plugin-data")
        .join("agent")
        .join("plugin.sqlite")
}

pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn utc_day(now: i64) -> i64 {
    now.div_euclid(86_400)
}

fn query_day(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    for pair in raw.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "day" {
            return Some(value.to_string());
        }
    }
    None
}

fn resolve_day(raw: Option<&str>, now_unix: i64) -> Result<(String, i64), String> {
    let id = match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => utc_day(now_unix),
        Some(text) => calendar_day(text)?,
    };
    let day = format_day(id).ok_or_else(|| "day 超出范围".to_string())?;
    Ok((day, id))
}

fn calendar_day(text: &str) -> Result<i64, String> {
    let date = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| "day 须为 YYYY-MM-DD".to_string())?;
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("unix epoch");
    Ok(date.signed_duration_since(epoch).num_days())
}

fn format_day(day_id: i64) -> Option<String> {
    let ts = day_id.checked_mul(86_400)?;
    Some(
        chrono::DateTime::from_timestamp(ts, 0)?
            .format("%Y-%m-%d")
            .to_string(),
    )
}

fn empty_report(day: &str, day_id: i64) -> UsageReport {
    UsageReport {
        day: day.to_string(),
        day_id,
        totals: TokenTotals {
            calls: 0,
            in_tokens: 0,
            out_tokens: 0,
        },
        by_user: Vec::new(),
        by_session: Vec::new(),
    }
}

async fn usage_report(path: &Path, day: &str, day_id: i64) -> anyhow::Result<UsageReport> {
    let Some(mut conn) = open_read(path).await? else {
        return Ok(empty_report(day, day_id));
    };
    if !has_table(&mut conn, "usage_daily").await? {
        return Ok(empty_report(day, day_id));
    }
    let totals = sqlx::query_as::<_, TokenTotals>(
        "SELECT CAST(COALESCE(SUM(calls), 0) AS INTEGER) AS calls,
                CAST(COALESCE(SUM(in_tokens), 0) AS INTEGER) AS in_tokens,
                CAST(COALESCE(SUM(out_tokens), 0) AS INTEGER) AS out_tokens
         FROM usage_daily WHERE day = ?1",
    )
    .bind(day_id)
    .fetch_one(&mut conn)
    .await?;
    let by_user = sqlx::query_as::<_, UserUsage>(
        "SELECT user_id,
                CAST(SUM(calls) AS INTEGER) AS calls,
                CAST(SUM(in_tokens) AS INTEGER) AS in_tokens,
                CAST(SUM(out_tokens) AS INTEGER) AS out_tokens
         FROM usage_daily WHERE day = ?1
         GROUP BY user_id
         ORDER BY calls DESC, user_id ASC
         LIMIT ?2",
    )
    .bind(day_id)
    .bind(TOP_N)
    .fetch_all(&mut conn)
    .await?;
    let by_session = sqlx::query_as::<_, SessionUsage>(
        "SELECT session_key,
                CAST(SUM(calls) AS INTEGER) AS calls,
                CAST(SUM(in_tokens) AS INTEGER) AS in_tokens,
                CAST(SUM(out_tokens) AS INTEGER) AS out_tokens
         FROM usage_daily WHERE day = ?1
         GROUP BY session_key
         ORDER BY calls DESC, session_key ASC
         LIMIT ?2",
    )
    .bind(day_id)
    .bind(TOP_N)
    .fetch_all(&mut conn)
    .await?;
    Ok(UsageReport {
        day: day.to_string(),
        day_id,
        totals,
        by_user,
        by_session,
    })
}

fn personas_from_config(v: &Value) -> PersonasView {
    let system_prompt = v
        .get("system_prompt")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut personas = Vec::new();
    if let Some(obj) = v.get("personas").and_then(Value::as_object) {
        let mut names: Vec<&str> = obj.keys().map(String::as_str).collect();
        names.sort_unstable();
        for name in names {
            let Some(prompt) = obj
                .get(name)
                .and_then(|body| body.get("system_prompt"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            personas.push(PersonaItem {
                name: name.to_string(),
                system_prompt: prompt.to_string(),
            });
        }
    }
    PersonasView {
        system_prompt,
        personas,
    }
}

pub(crate) async fn open_read(path: &Path) -> anyhow::Result<Option<sqlx::SqliteConnection>> {
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(
        SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .create_if_missing(false)
            .connect()
            .await?,
    ))
}

pub(crate) async fn has_table(
    conn: &mut sqlx::SqliteConnection,
    name: &str,
) -> anyhow::Result<bool> {
    let hit: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
            .bind(name)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(hit.is_some())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fj-admin-agent-{}-{n}", std::process::id()));
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

    #[test]
    fn day_matches_utc_euclidean() {
        let (day, id) = resolve_day(Some("2026-11-06"), 0).unwrap();
        assert_eq!(day, "2026-11-06");
        assert_eq!(
            resolve_day(None, id * 86_400 + 12 * 3600).unwrap(),
            ("2026-11-06".to_string(), id)
        );
        assert_eq!(resolve_day(None, id * 86_400 + 86_400).unwrap().1, id + 1);
        let (past, past_id) = resolve_day(Some("1969-12-31"), 0).unwrap();
        assert_eq!(past, "1969-12-31");
        assert_eq!(past_id, -1);
        assert_eq!(resolve_day(None, -1).unwrap().1, -1);
        assert!(resolve_day(Some("2026-02-31"), 0).is_err());
        assert!(resolve_day(Some("06-11-2026"), 0).is_err());
    }

    #[test]
    fn personas_come_from_agent_config() {
        let view = personas_from_config(&json!({
            "model": "deepseek-flash",
            "system_prompt": " top ",
            "rate_limit": { "per_user_per_hour": 30 },
            "personas": {
                "strict": { "system_prompt": "be strict", "note": "keep" },
                "default": { "system_prompt": "from default" },
                "empty": { "system_prompt": "" },
                "skip": { "priority": 1 }
            }
        }));
        assert_eq!(view.system_prompt, " top ");
        assert_eq!(
            view.personas,
            vec![
                PersonaItem {
                    name: "default".into(),
                    system_prompt: "from default".into(),
                },
                PersonaItem {
                    name: "empty".into(),
                    system_prompt: "".into(),
                },
                PersonaItem {
                    name: "strict".into(),
                    system_prompt: "be strict".into(),
                },
            ]
        );
        let missing = personas_from_config(&Value::Null);
        assert_eq!(missing.system_prompt, "");
        assert!(missing.personas.is_empty());
    }

    async fn seed(path: &Path, day: i64) {
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
        insert(&pool, day, "group:1", 1, 5, 10, 20).await;
        insert(&pool, day, "group:2", 1, 3, 1, 2).await;
        insert(&pool, day, "group:1", 2, 4, 7, 8).await;
        insert(&pool, day + 1, "group:99", 99, 100, 100, 100).await;
        for user in 3..=22 {
            insert(&pool, day, &format!("private:{user}"), user, 1, 0, 0).await;
        }
        pool.close().await;
    }

    async fn insert(
        pool: &sqlx::SqlitePool,
        day: i64,
        session: &str,
        user: i64,
        calls: i64,
        inn: i64,
        out_tokens: i64,
    ) {
        sqlx::query(
            "INSERT INTO usage_daily (day, session_key, user_id, calls, in_tokens, out_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(day)
        .bind(session)
        .bind(user)
        .bind(calls)
        .bind(inn)
        .bind(out_tokens)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn usage_report_reads_fixture_top_20() {
        let scratch = Scratch::new();
        let path = scratch.0.join("plugin.sqlite");
        let (day, id) = resolve_day(Some("2026-11-06"), 0).unwrap();
        seed(&path, id).await;
        let before = names_in(&scratch.0);
        let report = usage_report(&path, &day, id).await.unwrap();
        assert_eq!(names_in(&scratch.0), before);
        assert_eq!(report.day, day);
        assert_eq!(report.day_id, id);
        assert_eq!(
            report.totals,
            TokenTotals {
                calls: 32,
                in_tokens: 18,
                out_tokens: 30,
            }
        );
        assert_eq!(report.by_user.len(), 20);
        assert_eq!(
            report.by_user[0],
            UserUsage {
                user_id: 1,
                calls: 8,
                in_tokens: 11,
                out_tokens: 22,
            }
        );
        assert_eq!(
            report.by_user[1],
            UserUsage {
                user_id: 2,
                calls: 4,
                in_tokens: 7,
                out_tokens: 8,
            }
        );
        assert_eq!(report.by_user[19].user_id, 20);
        assert!(report
            .by_user
            .iter()
            .all(|row| row.user_id != 21 && row.user_id != 99));
        assert_eq!(report.by_session.len(), 20);
        assert_eq!(
            report.by_session[0],
            SessionUsage {
                session_key: "group:1".into(),
                calls: 9,
                in_tokens: 17,
                out_tokens: 28,
            }
        );
        assert_eq!(report.by_session[1].session_key, "group:2");
        assert_eq!(report.by_session[1].calls, 3);
        let keys: Vec<&str> = report
            .by_session
            .iter()
            .map(|row| row.session_key.as_str())
            .collect();
        assert!(keys.contains(&"private:10"));
        assert!(keys.contains(&"private:7"));
        assert!(!keys.contains(&"private:8"));
        assert!(!keys.contains(&"private:9"));
        assert!(!keys.contains(&"group:99"));
    }

    #[tokio::test]
    async fn missing_file_is_empty_and_creates_nothing() {
        let scratch = Scratch::new();
        let path = scratch.0.join("missing").join("plugin.sqlite");
        let (day, id) = resolve_day(Some("2026-11-06"), 0).unwrap();
        let report = usage_report(&path, &day, id).await.unwrap();
        assert!(!path.exists());
        assert!(!scratch.0.join("missing").exists());
        assert_eq!(report, empty_report(&day, id));
        assert!(names_in(&scratch.0).is_empty());
    }

    #[tokio::test]
    async fn missing_usage_table_is_empty() {
        let scratch = Scratch::new();
        let path = scratch.0.join("plugin.sqlite");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (session_key TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let before = names_in(&scratch.0);
        let (day, id) = resolve_day(Some("2026-11-06"), 0).unwrap();
        let report = usage_report(&path, &day, id).await.unwrap();
        assert_eq!(report, empty_report(&day, id));
        assert_eq!(names_in(&scratch.0), before);
        let mut conn = SqliteConnectOptions::new()
            .filename(&path)
            .read_only(true)
            .create_if_missing(false)
            .connect()
            .await
            .unwrap();
        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert!(!tables.iter().any(|name| name == "usage_daily"));
    }
}
