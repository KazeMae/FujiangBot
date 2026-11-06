use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{Local, TimeZone};
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use super::{
    apply_human, cancel_job, claim, create_job, ensure_schema, load, next_cron, parse_human, tick,
    tool_call, Draft, Fail, HumanCmd, ScheduleGate, When, TICK_SECS, USAGE,
};

fn draft(
    kind: &str,
    group_id: Option<i64>,
    user_id: Option<i64>,
    cron: Option<String>,
    at: Option<i64>,
    payload: &str,
) -> Draft {
    Draft {
        kind: kind.to_string(),
        group_id,
        user_id,
        cron,
        at,
        payload: payload.to_string(),
        enabled: true,
    }
}
use crate::schedule_cron::next_cron as cron_next;

struct Gate {
    allow: bool,
    down: bool,
    fail_send: bool,
    asked: Mutex<Vec<(i64, i64)>>,
    sent: Mutex<Vec<(String, i64, String)>>,
}

impl Gate {
    fn ok() -> Self {
        Self {
            allow: true,
            down: false,
            fail_send: false,
            asked: Mutex::new(Vec::new()),
            sent: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<(i64, i64)> {
        self.asked.lock().unwrap().clone()
    }

    fn sent(&self) -> Vec<(String, i64, String)> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl ScheduleGate for Gate {
    async fn group_ok(&self, speaker: i64, group_id: i64) -> anyhow::Result<bool> {
        self.asked.lock().unwrap().push((speaker, group_id));
        if self.down {
            anyhow::bail!("napcat down");
        }
        Ok(self.allow)
    }

    async fn deliver(&self, kind: &str, target: i64, text: &str) -> anyhow::Result<()> {
        if self.fail_send {
            anyhow::bail!("send failed");
        }
        self.sent
            .lock()
            .unwrap()
            .push((kind.to_string(), target, text.to_string()));
        Ok(())
    }
}

async fn mem() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    ensure_schema(&pool).await.unwrap();
    pool
}

fn local_ts(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> i64 {
    Local
        .with_ymd_and_hms(y, m, d, h, min, s)
        .single()
        .unwrap()
        .timestamp()
}

async fn rows(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_jobs")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[test]
fn cron_and_one_shot_next_run() {
    let morning = local_ts(2026, 1, 7, 7, 30, 0);
    assert_eq!(
        next_cron("0 8 * * *", morning),
        Some(local_ts(2026, 1, 7, 8, 0, 0))
    );
    assert_eq!(
        next_cron("0 8 * * *", local_ts(2026, 1, 7, 8, 0, 30)),
        Some(local_ts(2026, 1, 8, 8, 0, 0))
    );
    assert_eq!(
        next_cron("0 8 * * 1", local_ts(2026, 1, 7, 9, 0, 0)),
        Some(local_ts(2026, 1, 12, 8, 0, 0))
    );
    assert_eq!(
        next_cron("0 8 * * 7", local_ts(2026, 1, 3, 0, 0, 0)),
        Some(local_ts(2026, 1, 4, 8, 0, 0))
    );
    assert_eq!(
        next_cron("*/15 8 * * *", local_ts(2026, 1, 7, 7, 50, 0)),
        Some(local_ts(2026, 1, 7, 8, 0, 0))
    );
    assert_eq!(
        next_cron("*/15 8 * * *", local_ts(2026, 1, 7, 8, 0, 0)),
        Some(local_ts(2026, 1, 7, 8, 15, 0))
    );
    assert_eq!(
        next_cron("0 0 1 * 1", local_ts(2026, 1, 26, 0, 0, 1)),
        Some(local_ts(2026, 2, 1, 0, 0, 0))
    );
    assert_eq!(cron_next("60 0 * * *", morning), None);
    assert_eq!(cron_next("0 0 31 2 *", morning), None);
    assert_eq!(cron_next("0 8 * *", morning), None);
    const { assert!(TICK_SECS >= 15) };
}

#[tokio::test]
async fn schema_alter_adds_the_job_columns() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE scheduled_jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            creator_qq INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    ensure_schema(&pool).await.unwrap();
    ensure_schema(&pool).await.unwrap();
    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('scheduled_jobs')")
            .fetch_all(&pool)
            .await
            .unwrap();
    for column in [
        "id",
        "creator_qq",
        "cron",
        "at",
        "action",
        "kind",
        "group_id",
        "user_id",
        "payload",
        "enabled",
        "created_at",
        "next_run_at",
        "claim_until",
        "claim_token",
        "last_error",
    ] {
        assert!(names.iter().any(|name| name == column), "{column}");
    }
}

#[tokio::test]
async fn creator_is_injected_and_private_peer_cannot_be_forged() {
    let pool = mem().await;
    let gate = Gate::ok();
    let saved = tool_call(
        &pool,
        7,
        "schedule_create",
        &json!({
            "kind": "group",
            "group_id": 42,
            "at": 2_000_000_000,
            "payload": "开会",
            "creator_qq": 999,
            "user_id": 999
        }),
        1_700_000_000,
        &gate,
    )
    .await
    .unwrap_err();
    assert!(matches!(saved, Fail::Bad));
    assert_eq!(rows(&pool).await, 0);
    assert_eq!(gate.asked(), vec![]);

    let made = tool_call(
        &pool,
        7,
        "schedule_create",
        &json!({
            "kind": "group",
            "group_id": 42,
            "at": 2_000_000_000,
            "payload": "开会",
            "creator_qq": 999
        }),
        1_700_000_000,
        &gate,
    )
    .await
    .unwrap();
    assert_eq!(made["creator_qq"], 7);
    assert_eq!(gate.asked(), vec![(7, 42)]);
    let job = load(&pool, made["id"].as_i64().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.creator_qq, 7);
    assert_eq!(job.group_id, Some(42));
    assert_eq!(job.user_id, None);
    assert_eq!(job.at, Some(2_000_000_000));
    assert_eq!(job.action, "send_text");
    assert_eq!(job.next_run_at, Some(2_000_000_000));

    let forged = tool_call(
        &pool,
        7,
        "schedule_create",
        &json!({
            "kind": "private",
            "user_id": 8,
            "at": 2_000_000_000,
            "payload": "偷看",
            "creator_qq": 8
        }),
        1_700_000_000,
        &gate,
    )
    .await
    .unwrap_err();
    assert!(matches!(forged, Fail::Denied));

    let self_job = tool_call(
        &pool,
        7,
        "schedule_create",
        &json!({
            "kind": "private",
            "at": 1_600_000_000,
            "payload": "提醒自己",
            "creator_qq": 999
        }),
        1_700_000_000,
        &gate,
    )
    .await
    .unwrap();
    let job = load(&pool, self_job["id"].as_i64().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.creator_qq, 7);
    assert_eq!(job.user_id, Some(7));
    assert_eq!(job.next_run_at, Some(1_700_000_000));
    assert!(gate.sent().is_empty());
}

#[tokio::test]
async fn list_update_and_cancel_stay_with_the_creator() {
    let pool = mem().await;
    let gate = Gate::ok();
    let now = 1_700_000_000;
    let owned = create_job(
        &pool,
        7,
        draft("private", None, None, None, Some(now + 50), "自己的"),
        now,
        &gate,
    )
    .await
    .unwrap();
    create_job(
        &pool,
        8,
        draft("private", None, None, None, Some(now + 50), "别人的"),
        now,
        &gate,
    )
    .await
    .unwrap();
    let listed = tool_call(&pool, 7, "schedule_list", &json!({}), now, &gate)
        .await
        .unwrap();
    let rows = listed.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["creator_qq"], 7);
    assert_eq!(rows[0]["payload"], "自己的");

    let denied = tool_call(
        &pool,
        8,
        "schedule_update",
        &json!({"id": owned.id, "payload": "改掉"}),
        now,
        &gate,
    )
    .await
    .unwrap_err();
    assert!(matches!(denied, Fail::Denied));
    let denied = tool_call(
        &pool,
        8,
        "schedule_cancel",
        &json!({"id": owned.id}),
        now,
        &gate,
    )
    .await
    .unwrap_err();
    assert!(matches!(denied, Fail::Denied));
    let job = load(&pool, owned.id).await.unwrap().unwrap();
    assert_eq!(job.payload, "自己的");
    assert_eq!(job.enabled, 1);

    let updated = tool_call(
        &pool,
        7,
        "schedule_update",
        &json!({"id": owned.id, "payload": "改过"}),
        now,
        &gate,
    )
    .await
    .unwrap();
    assert_eq!(updated["creator_qq"], 7);
    assert_eq!(
        load(&pool, owned.id).await.unwrap().unwrap().payload,
        "改过"
    );
    tool_call(
        &pool,
        7,
        "schedule_cancel",
        &json!({"id": owned.id}),
        now,
        &gate,
    )
    .await
    .unwrap();
    let job = load(&pool, owned.id).await.unwrap().unwrap();
    assert_eq!(job.enabled, 0);
    assert_eq!(job.next_run_at, None);
}

#[tokio::test]
async fn human_commands_do_not_need_the_model() {
    let pool = mem().await;
    let gate = Gate::ok();
    let now = 1_700_000_000;
    assert!(matches!(parse_human(std::iter::empty()), HumanCmd::Help));
    assert!(matches!(
        parse_human(["list", "extra"].into_iter()),
        HumanCmd::Bad
    ));
    assert!(matches!(
        parse_human(["create", "private", "at", "1800000000", "带", "空格"].into_iter()),
        HumanCmd::Create { ref payload, ref when, .. }
            if payload == "带 空格" && matches!(when, When::At(1800000000))
    ));
    let text = apply_human(
        &pool,
        7,
        &parse_human(
            [
                "create", "group", "42", "cron", "0", "8", "*", "*", "*", "早",
            ]
            .into_iter(),
        ),
        now,
        &gate,
    )
    .await
    .unwrap();
    assert!(text.contains("已记下定时任务 #"));
    assert_eq!(gate.asked(), vec![(7, 42)]);
    let listed = apply_human(&pool, 7, &HumanCmd::List, now, &gate)
        .await
        .unwrap();
    assert!(listed.contains("早"));
    assert!(listed.contains("cron 0 8 * * *"));
    let hidden = apply_human(&pool, 8, &HumanCmd::List, now, &gate)
        .await
        .unwrap();
    assert_eq!(hidden, "还没有定时任务。");
    let missing = apply_human(&pool, 8, &HumanCmd::Cancel(1), now, &gate)
        .await
        .unwrap();
    assert_eq!(missing, "没有这条定时任务。");
    assert_eq!(load(&pool, 1).await.unwrap().unwrap().enabled, 1);
    let cancelled = apply_human(&pool, 7, &HumanCmd::Cancel(1), now, &gate)
        .await
        .unwrap();
    assert!(cancelled.contains("已取消"));
    assert!(USAGE.contains("不调用模型"));
    let bad = apply_human(&pool, 7, &HumanCmd::Bad, now, &gate)
        .await
        .unwrap();
    assert!(bad.contains(".agent schedule list"));
}

#[tokio::test]
async fn one_shot_and_cron_fire_advance_or_retry() {
    let pool = mem().await;
    let now = local_ts(2026, 1, 7, 8, 0, 5);
    let gate = Gate::ok();
    let once = create_job(
        &pool,
        7,
        draft("private", None, None, None, Some(now - 5), "一次"),
        now,
        &gate,
    )
    .await
    .unwrap();
    assert_eq!(once.next_run_at, now);
    tick(&pool, &gate, now).await.unwrap();
    assert_eq!(
        gate.sent(),
        vec![("private".to_string(), 7, "一次".to_string())]
    );
    let job = load(&pool, once.id).await.unwrap().unwrap();
    assert_eq!(job.enabled, 0);
    assert_eq!(job.next_run_at, None);
    assert!(job.last_error.is_none());

    let cron = create_job(
        &pool,
        7,
        draft(
            "group",
            Some(42),
            None,
            Some("0 8 * * *".to_string()),
            None,
            "每天",
        ),
        now,
        &gate,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE scheduled_jobs SET next_run_at = ?2 WHERE id = ?1")
        .bind(cron.id)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
    tick(&pool, &gate, now).await.unwrap();
    let job = load(&pool, cron.id).await.unwrap().unwrap();
    assert_eq!(job.enabled, 1);
    assert_eq!(job.next_run_at, next_cron("0 8 * * *", now));
    assert!(gate.sent().iter().any(|item| item.2 == "每天"));

    let denied = Gate {
        allow: false,
        ..Gate::ok()
    };
    let blocked = create_job(
        &pool,
        7,
        draft("group", Some(9), None, None, Some(now), "进不去"),
        now,
        &Gate::ok(),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE scheduled_jobs SET next_run_at = ?2 WHERE id = ?1")
        .bind(blocked.id)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
    tick(&pool, &denied, now).await.unwrap();
    assert!(denied.sent().is_empty());
    let job = load(&pool, blocked.id).await.unwrap().unwrap();
    assert_eq!(job.enabled, 0);
    assert_eq!(job.last_error.as_deref(), Some("denied"));

    let flaky = Gate {
        fail_send: true,
        ..Gate::ok()
    };
    let retry = create_job(
        &pool,
        7,
        draft("private", None, None, None, Some(now), "再试"),
        now,
        &gate,
    )
    .await
    .unwrap();
    tick(&pool, &flaky, now).await.unwrap();
    let job = load(&pool, retry.id).await.unwrap().unwrap();
    assert_eq!(job.enabled, 1);
    assert_eq!(job.next_run_at, Some(now));
    assert_eq!(job.claim_token, None);
    assert!(job.last_error.as_deref().unwrap().contains("send failed"));
    tick(&pool, &gate, now).await.unwrap();
    let job = load(&pool, retry.id).await.unwrap().unwrap();
    assert_eq!(job.enabled, 0);
    assert!(gate.sent().iter().any(|item| item.2 == "再试"));
}

#[tokio::test]
async fn claim_is_single_winner_and_a_restart_waits_for_the_lease() {
    let pool = mem().await;
    let now = 1_700_000_000_i64;
    let gate = Gate::ok();
    let job = create_job(
        &pool,
        7,
        draft("private", None, None, None, Some(now), "租约"),
        now,
        &gate,
    )
    .await
    .unwrap();
    let first = claim(&pool, job.id, now).await.unwrap();
    assert!(first.is_some());
    assert!(claim(&pool, job.id, now).await.unwrap().is_none());
    tick(&pool, &gate, now + 1).await.unwrap();
    assert!(gate.sent().is_empty());
    tick(&pool, &gate, now + super::LEASE_SECS).await.unwrap();
    assert_eq!(gate.sent().len(), 1);

    let (shared, path) = file_pool().await;
    let raced = create_job(
        &shared,
        7,
        draft("private", None, None, None, Some(now), "抢"),
        now,
        &gate,
    )
    .await
    .unwrap();
    let left = shared.clone();
    let right = shared.clone();
    let id = raced.id;
    let a = tokio::spawn(async move { claim(&left, id, now).await.unwrap() });
    let b = tokio::spawn(async move { claim(&right, id, now).await.unwrap() });
    let (a, b) = tokio::join!(a, b);
    let wins = [a.unwrap().is_some(), b.unwrap().is_some()]
        .into_iter()
        .filter(|won| *won)
        .count();
    assert_eq!(wins, 1);
    drop(shared);
    cleanup(&path);
}

#[tokio::test]
async fn a_non_text_action_is_not_sent() {
    let pool = mem().await;
    let now = 50_i64;
    sqlx::query(
        "INSERT INTO scheduled_jobs
         (creator_qq, \"at\", action, kind, user_id, payload, enabled, created_at, next_run_at)
         VALUES (7, 50, 'get_friend_msg_history', 'private', 7, 'nope', 1, 50, 50)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let gate = Gate::ok();
    tick(&pool, &gate, now).await.unwrap();
    assert!(gate.sent().is_empty());
    assert!(gate.asked().is_empty());
    let job = load(&pool, 1).await.unwrap().unwrap();
    assert_eq!(job.enabled, 0);
    assert_eq!(job.last_error.as_deref(), Some("denied"));
    let gone = cancel_job(&pool, 9, 1).await.unwrap_err();
    assert!(matches!(gone, Fail::Denied));
}

async fn file_pool() -> (SqlitePool, PathBuf) {
    static N: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!(
        "fj-sched-{}-{}.sqlite",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("PRAGMA busy_timeout = 5000")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();
    ensure_schema(&pool).await.unwrap();
    (pool, path)
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let _ = std::fs::remove_file(wal);
    let mut shm = path.as_os_str().to_owned();
    shm.push("-shm");
    let _ = std::fs::remove_file(shm);
}
