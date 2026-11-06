use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tracing::warn;

use crate::schedule_cron::next_cron;

pub(crate) const TICK_SECS: u64 = 15;
pub(crate) const LEASE_SECS: i64 = 120;
const DUE_LIMIT: i64 = 20;
const LIST_LIMIT: i64 = 50;
const PAYLOAD_MAX: usize = 800;
const ACTION: &str = "send_text";

pub(crate) const USAGE: &str = "\
.agent schedule list\n\
.agent schedule create group <群号> at <unix秒> <正文>\n\
.agent schedule create group <群号> cron <分> <时> <日> <月> <周> <正文>\n\
.agent schedule create private at <unix秒> <正文>\n\
.agent schedule create private cron <分> <时> <日> <月> <周> <正文>\n\
.agent schedule cancel <编号>\n\
cron 用本机本地时间，五段是分、时、日、月、周。周 0 和 7 都是周日。支持 *、数字、逗号、a-b 和 */n。日和周都写了具体值时，碰上其中一个就到点。私聊只发给自己。这条命令不调用模型。";

const COLUMNS: &[(&str, &str)] = &[
    ("cron", "TEXT"),
    ("\"at\"", "INTEGER"),
    ("action", "TEXT NOT NULL DEFAULT 'send_text'"),
    ("kind", "TEXT NOT NULL DEFAULT 'private'"),
    ("group_id", "INTEGER"),
    ("user_id", "INTEGER"),
    ("payload", "TEXT NOT NULL DEFAULT ''"),
    ("enabled", "INTEGER NOT NULL DEFAULT 1"),
    ("created_at", "INTEGER NOT NULL DEFAULT 0"),
    ("next_run_at", "INTEGER"),
    ("claim_until", "INTEGER"),
    ("claim_token", "TEXT"),
    ("last_error", "TEXT"),
];

const LOAD_SQL: &str = "SELECT id, creator_qq, cron, \"at\", action, kind, group_id, user_id, payload, enabled, created_at, next_run_at, claim_until, claim_token, last_error FROM scheduled_jobs";

#[derive(Debug)]
pub(crate) enum Fail {
    Denied,
    Bad,
    Down(anyhow::Error),
}

#[derive(Debug)]
pub(crate) struct Saved {
    pub id: i64,
    pub next_run_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HumanCmd {
    Help,
    List,
    Create {
        kind: String,
        group_id: Option<i64>,
        when: When,
        payload: String,
    },
    Cancel(i64),
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum When {
    At(i64),
    Cron(String),
}

pub(crate) struct Job {
    pub id: i64,
    pub creator_qq: i64,
    pub cron: Option<String>,
    pub at: Option<i64>,
    pub action: String,
    pub kind: String,
    pub group_id: Option<i64>,
    pub user_id: Option<i64>,
    pub payload: String,
    pub enabled: i64,
    pub created_at: i64,
    pub next_run_at: Option<i64>,
    claim_token: Option<String>,
    pub last_error: Option<String>,
}

struct Spec {
    cron: Option<String>,
    at: Option<i64>,
    kind: String,
    group_id: Option<i64>,
    user_id: Option<i64>,
    payload: String,
    enabled: i64,
    next_run_at: Option<i64>,
}

struct Draft {
    kind: String,
    group_id: Option<i64>,
    user_id: Option<i64>,
    cron: Option<String>,
    at: Option<i64>,
    payload: String,
    enabled: bool,
}

#[async_trait]
pub(crate) trait ScheduleGate: Send + Sync {
    async fn group_ok(&self, speaker: i64, group_id: i64) -> anyhow::Result<bool>;
    async fn deliver(&self, kind: &str, target: i64, text: &str) -> anyhow::Result<()>;
}

pub(crate) async fn ensure_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS scheduled_jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            creator_qq INTEGER NOT NULL
        )",
    )
    .execute(pool)
    .await
    .context("create scheduled_jobs")?;
    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('scheduled_jobs')")
            .fetch_all(pool)
            .await
            .context("scheduled_jobs columns")?;
    for (name, ddl) in COLUMNS {
        let bare = name.trim_matches('"');
        if names.iter().any(|have| have == bare) {
            continue;
        }
        sqlx::query(&format!(
            "ALTER TABLE scheduled_jobs ADD COLUMN {name} {ddl}"
        ))
        .execute(pool)
        .await
        .with_context(|| format!("add scheduled_jobs.{bare}"))?;
    }
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS scheduled_jobs_due ON scheduled_jobs(enabled, next_run_at)",
    )
    .execute(pool)
    .await
    .context("scheduled_jobs index")?;
    Ok(())
}

pub(crate) fn parse_human<'a, I>(mut parts: I) -> HumanCmd
where
    I: Iterator<Item = &'a str>,
{
    match parts.next().unwrap_or("") {
        "" | "help" | "-h" | "--help" => HumanCmd::Help,
        "list" => {
            if parts.next().is_some() {
                HumanCmd::Bad
            } else {
                HumanCmd::List
            }
        }
        "cancel" => parse_cancel(parts.next(), parts.next().is_some()),
        "create" => parse_create(parts),
        _ => HumanCmd::Bad,
    }
}

pub(crate) async fn apply_human<G: ScheduleGate + ?Sized>(
    pool: &SqlitePool,
    speaker: i64,
    cmd: &HumanCmd,
    now: i64,
    gate: &G,
) -> anyhow::Result<String> {
    match cmd {
        HumanCmd::Help | HumanCmd::Bad => Ok(USAGE.to_string()),
        HumanCmd::List => {
            let jobs = list_for(pool, speaker).await?;
            Ok(format_list(&jobs))
        }
        HumanCmd::Cancel(id) => match cancel_job(pool, speaker, *id).await {
            Ok(()) => Ok(format!("已取消定时任务 #{id}。")),
            Err(Fail::Denied) => Ok("没有这条定时任务。".to_string()),
            Err(Fail::Bad) => Ok(USAGE.to_string()),
            Err(Fail::Down(err)) => Err(err),
        },
        HumanCmd::Create {
            kind,
            group_id,
            when,
            payload,
        } => {
            let (cron, at) = match when {
                When::At(at) => (None, Some(*at)),
                When::Cron(cron) => (Some(cron.clone()), None),
            };
            match create_job(
                pool,
                speaker,
                Draft {
                    kind: kind.clone(),
                    group_id: *group_id,
                    user_id: None,
                    cron,
                    at,
                    payload: payload.clone(),
                    enabled: true,
                },
                now,
                gate,
            )
            .await
            {
                Ok(saved) => Ok(format!(
                    "已记下定时任务 #{}，下次 {}。",
                    saved.id, saved.next_run_at
                )),
                Err(Fail::Denied) => Ok("这个目标现在不能发。".to_string()),
                Err(Fail::Bad) => Ok("时间或正文不对。发 .agent schedule 看写法。".to_string()),
                Err(Fail::Down(err)) => {
                    warn!(error = %err, "agent schedule");
                    Ok("现在没查成，过一会儿再试。".to_string())
                }
            }
        }
    }
}

pub(crate) async fn tool_call<G: ScheduleGate + ?Sized>(
    pool: &SqlitePool,
    speaker: i64,
    wire: &str,
    args: &Value,
    now: i64,
    gate: &G,
) -> Result<Value, Fail> {
    if speaker <= 0 {
        return Err(Fail::Denied);
    }
    match wire {
        "schedule_list" => list_json(pool, speaker).await,
        "schedule_create" => create_json(pool, speaker, args, now, gate).await,
        "schedule_update" => update_json(pool, speaker, args, now, gate).await,
        "schedule_cancel" => cancel_json(pool, speaker, args).await,
        _ => Err(Fail::Bad),
    }
}

pub(crate) async fn tick<G: ScheduleGate + ?Sized>(
    pool: &SqlitePool,
    gate: &G,
    now: i64,
) -> anyhow::Result<()> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM scheduled_jobs
         WHERE enabled = 1
           AND next_run_at IS NOT NULL
           AND next_run_at <= ?1
           AND (claim_until IS NULL OR claim_until <= ?1)
         ORDER BY next_run_at, id
         LIMIT ?2",
    )
    .bind(now)
    .bind(DUE_LIMIT)
    .fetch_all(pool)
    .await
    .context("schedule due")?;
    for id in ids {
        let Some(token) = claim(pool, id, now).await? else {
            continue;
        };
        match fire_claimed(pool, gate, id, &token, now).await {
            Ok(Step::Done) => {}
            Ok(Step::Retry(reason)) => {
                warn!(job = id, error = %reason, "agent schedule");
                if let Err(err) = release(pool, id, &token, &reason).await {
                    warn!(job = id, error = %err, "agent schedule release");
                }
            }
            Err(err) => warn!(job = id, error = %err, "agent schedule"),
        }
    }
    Ok(())
}

pub(crate) async fn claim(pool: &SqlitePool, id: i64, now: i64) -> anyhow::Result<Option<String>> {
    let token = claim_token(id, now);
    let until = now.saturating_add(LEASE_SECS);
    let done = sqlx::query(
        "UPDATE scheduled_jobs
         SET claim_until = ?2, claim_token = ?3
         WHERE id = ?1
           AND enabled = 1
           AND next_run_at IS NOT NULL
           AND next_run_at <= ?4
           AND (claim_until IS NULL OR claim_until <= ?4)",
    )
    .bind(id)
    .bind(until)
    .bind(&token)
    .bind(now)
    .execute(pool)
    .await
    .context("schedule claim")?;
    Ok((done.rows_affected() == 1).then_some(token))
}

async fn create_job<G: ScheduleGate + ?Sized>(
    pool: &SqlitePool,
    speaker: i64,
    draft: Draft,
    now: i64,
    gate: &G,
) -> Result<Saved, Fail> {
    let spec = screen(speaker, draft, now)?;
    permit(gate, speaker, &spec).await?;
    let id = insert_job(pool, speaker, &spec, now)
        .await
        .map_err(Fail::Down)?;
    Ok(Saved {
        id,
        next_run_at: spec.next_run_at.unwrap_or(now),
    })
}

async fn create_json<G: ScheduleGate + ?Sized>(
    pool: &SqlitePool,
    speaker: i64,
    args: &Value,
    now: i64,
    gate: &G,
) -> Result<Value, Fail> {
    if !action_ok(args) {
        return Err(Fail::Bad);
    }
    let saved = create_job(
        pool,
        speaker,
        Draft {
            kind: required_str(args, "kind")?.to_string(),
            group_id: opt_i64(args, "group_id")?,
            user_id: opt_i64(args, "user_id")?,
            cron: opt_string(args, "cron")?,
            at: opt_i64(args, "at")?,
            payload: required_str(args, "payload")?.to_string(),
            enabled: true,
        },
        now,
        gate,
    )
    .await?;
    Ok(json!({
        "id": saved.id,
        "creator_qq": speaker,
        "next_run_at": saved.next_run_at,
        "enabled": true
    }))
}

async fn update_json<G: ScheduleGate + ?Sized>(
    pool: &SqlitePool,
    speaker: i64,
    args: &Value,
    now: i64,
    gate: &G,
) -> Result<Value, Fail> {
    if !action_ok(args) {
        return Err(Fail::Bad);
    }
    let id = required_id(args, "id")?;
    let job = owned(pool, speaker, id).await?;
    let payload = opt_string(args, "payload")?.unwrap_or(job.payload);
    let enabled = opt_bool(args, "enabled")?.unwrap_or(job.enabled != 0);
    let kind = opt_string(args, "kind")?.unwrap_or(job.kind);
    let group_id = match presence_i64(args, "group_id")? {
        None => job.group_id,
        Some(id) => id,
    };
    let user_id = match presence_i64(args, "user_id")? {
        None => job.user_id,
        Some(id) => id,
    };
    let (cron, at) = match (presence_string(args, "cron")?, presence_i64(args, "at")?) {
        (None, None) => (job.cron, job.at),
        (Some(cron), None) => (cron, None),
        (None, Some(at)) => (None, at),
        (Some(_), Some(_)) => return Err(Fail::Bad),
    };
    let spec = screen(
        speaker,
        Draft {
            kind,
            group_id,
            user_id,
            cron,
            at,
            payload,
            enabled,
        },
        now,
    )?;
    if spec.enabled != 0 {
        permit(gate, speaker, &spec).await?;
    }
    let n = write_spec(pool, id, speaker, &spec)
        .await
        .map_err(Fail::Down)?;
    if n == 0 {
        return Err(Fail::Denied);
    }
    Ok(json!({
        "id": id,
        "creator_qq": speaker,
        "enabled": spec.enabled != 0,
        "next_run_at": spec.next_run_at
    }))
}

async fn cancel_json(pool: &SqlitePool, speaker: i64, args: &Value) -> Result<Value, Fail> {
    let id = required_id(args, "id")?;
    cancel_job(pool, speaker, id).await?;
    Ok(json!({"id": id, "enabled": false}))
}

async fn list_json(pool: &SqlitePool, speaker: i64) -> Result<Value, Fail> {
    let jobs = list_for(pool, speaker).await.map_err(Fail::Down)?;
    Ok(Value::Array(jobs.iter().map(job_json).collect()))
}

async fn list_for(pool: &SqlitePool, speaker: i64) -> anyhow::Result<Vec<Job>> {
    let rows = sqlx::query(&format!(
        "{LOAD_SQL} WHERE creator_qq = ?1 ORDER BY id DESC LIMIT ?2"
    ))
    .bind(speaker)
    .bind(LIST_LIMIT)
    .fetch_all(pool)
    .await
    .context("schedule list")?;
    rows.iter().map(job_from).collect()
}

async fn cancel_job(pool: &SqlitePool, speaker: i64, id: i64) -> Result<(), Fail> {
    if speaker <= 0 || id <= 0 {
        return Err(Fail::Denied);
    }
    let done = sqlx::query(
        "UPDATE scheduled_jobs
         SET enabled = 0, next_run_at = NULL, claim_until = NULL, claim_token = NULL
         WHERE id = ?1 AND creator_qq = ?2",
    )
    .bind(id)
    .bind(speaker)
    .execute(pool)
    .await
    .map_err(|err| Fail::Down(err.into()))?;
    if done.rows_affected() == 0 {
        Err(Fail::Denied)
    } else {
        Ok(())
    }
}

async fn owned(pool: &SqlitePool, speaker: i64, id: i64) -> Result<Job, Fail> {
    let job = load(pool, id)
        .await
        .map_err(Fail::Down)?
        .ok_or(Fail::Denied)?;
    if job.creator_qq != speaker {
        Err(Fail::Denied)
    } else {
        Ok(job)
    }
}

async fn permit<G: ScheduleGate + ?Sized>(gate: &G, speaker: i64, spec: &Spec) -> Result<(), Fail> {
    if spec.kind != "group" {
        return Ok(());
    }
    let group_id = spec.group_id.ok_or(Fail::Bad)?;
    match gate.group_ok(speaker, group_id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Fail::Denied),
        Err(err) => Err(Fail::Down(err)),
    }
}

fn screen(speaker: i64, draft: Draft, now: i64) -> Result<Spec, Fail> {
    if speaker <= 0 {
        return Err(Fail::Denied);
    }
    let payload = draft.payload.trim();
    let chars = payload.chars().count();
    if chars == 0 || chars > PAYLOAD_MAX {
        return Err(Fail::Bad);
    }
    let (kind, group_id, user_id) = match draft.kind.as_str() {
        "group" => {
            if draft.user_id.is_some() {
                return Err(Fail::Bad);
            }
            let group_id = draft.group_id.filter(|id| *id > 0).ok_or(Fail::Bad)?;
            ("group", Some(group_id), None)
        }
        "private" => {
            if draft.group_id.is_some() {
                return Err(Fail::Bad);
            }
            if draft.user_id.is_some_and(|id| id != speaker) {
                return Err(Fail::Denied);
            }
            ("private", None, Some(speaker))
        }
        _ => return Err(Fail::Bad),
    };
    let (cron, at, upcoming) = match (draft.cron, draft.at) {
        (Some(cron), None) => {
            let cron = cron.split_whitespace().collect::<Vec<_>>().join(" ");
            let upcoming = next_cron(&cron, now).ok_or(Fail::Bad)?;
            (Some(cron), None, upcoming)
        }
        (None, Some(at)) if at > 0 => (None, Some(at), at.max(now)),
        _ => return Err(Fail::Bad),
    };
    Ok(Spec {
        cron,
        at,
        kind: kind.to_string(),
        group_id,
        user_id,
        payload: payload.to_string(),
        enabled: i64::from(draft.enabled),
        next_run_at: draft.enabled.then_some(upcoming),
    })
}

async fn insert_job(pool: &SqlitePool, speaker: i64, spec: &Spec, now: i64) -> anyhow::Result<i64> {
    let done = sqlx::query(
        "INSERT INTO scheduled_jobs
         (creator_qq, cron, \"at\", action, kind, group_id, user_id, payload, enabled, created_at, next_run_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )
    .bind(speaker)
    .bind(&spec.cron)
    .bind(spec.at)
    .bind(ACTION)
    .bind(&spec.kind)
    .bind(spec.group_id)
    .bind(spec.user_id)
    .bind(&spec.payload)
    .bind(spec.enabled)
    .bind(now)
    .bind(spec.next_run_at)
    .execute(pool)
    .await
    .context("schedule insert")?;
    Ok(done.last_insert_rowid())
}

async fn write_spec(pool: &SqlitePool, id: i64, speaker: i64, spec: &Spec) -> anyhow::Result<u64> {
    let done = sqlx::query(
        "UPDATE scheduled_jobs
         SET cron = ?3, \"at\" = ?4, action = ?5, kind = ?6, group_id = ?7, user_id = ?8,
             payload = ?9, enabled = ?10, next_run_at = ?11, claim_until = NULL,
             claim_token = NULL, last_error = NULL
         WHERE id = ?1 AND creator_qq = ?2",
    )
    .bind(id)
    .bind(speaker)
    .bind(&spec.cron)
    .bind(spec.at)
    .bind(ACTION)
    .bind(&spec.kind)
    .bind(spec.group_id)
    .bind(spec.user_id)
    .bind(&spec.payload)
    .bind(spec.enabled)
    .bind(spec.next_run_at)
    .execute(pool)
    .await
    .context("schedule update")?;
    Ok(done.rows_affected())
}

enum Step {
    Done,
    Retry(String),
}

async fn fire_claimed<G: ScheduleGate + ?Sized>(
    pool: &SqlitePool,
    gate: &G,
    id: i64,
    token: &str,
    now: i64,
) -> anyhow::Result<Step> {
    let Some(job) = load(pool, id).await? else {
        return Ok(Step::Done);
    };
    if job.claim_token.as_deref() != Some(token) || job.enabled == 0 {
        return Ok(Step::Done);
    }
    let (kind, target) = match decide(&job, gate).await {
        Ok(Decision::Finish(error)) => {
            ack(pool, id, token, &advance_of(&job, now, error)).await?;
            return Ok(Step::Done);
        }
        Ok(Decision::Send { kind, target }) => (kind, target),
        Err(err) => return Ok(Step::Retry(clip_err(&err))),
    };
    if !holds(pool, id, token).await? {
        return Ok(Step::Done);
    }
    if let Err(err) = gate.deliver(kind, target, &job.payload).await {
        return Ok(Step::Retry(clip_err(&err)));
    }
    ack(pool, id, token, &advance_of(&job, now, None)).await?;
    Ok(Step::Done)
}

enum Decision {
    Send { kind: &'static str, target: i64 },
    Finish(Option<String>),
}

async fn decide<G: ScheduleGate + ?Sized>(job: &Job, gate: &G) -> anyhow::Result<Decision> {
    if job.action != ACTION {
        return Ok(Decision::Finish(Some("denied".to_string())));
    }
    match job.kind.as_str() {
        "group" => {
            let Some(group_id) = job.group_id else {
                return Ok(Decision::Finish(Some("denied".to_string())));
            };
            if gate.group_ok(job.creator_qq, group_id).await? {
                Ok(Decision::Send {
                    kind: "group",
                    target: group_id,
                })
            } else {
                warn!(
                    job = job.id,
                    group_id,
                    creator = job.creator_qq,
                    "agent schedule denied"
                );
                Ok(Decision::Finish(Some("denied".to_string())))
            }
        }
        "private" => {
            if job.user_id != Some(job.creator_qq) {
                warn!(job = job.id, "agent schedule denied");
                return Ok(Decision::Finish(Some("denied".to_string())));
            }
            Ok(Decision::Send {
                kind: "private",
                target: job.creator_qq,
            })
        }
        _ => Ok(Decision::Finish(Some("denied".to_string()))),
    }
}

struct Advanced {
    enabled: i64,
    next_run_at: Option<i64>,
    last_error: Option<String>,
}

fn advance_of(job: &Job, now: i64, last_error: Option<String>) -> Advanced {
    match job.cron.as_deref() {
        Some(cron) => match next_cron(cron, now) {
            Some(next) => Advanced {
                enabled: 1,
                next_run_at: Some(next),
                last_error,
            },
            None => Advanced {
                enabled: 0,
                next_run_at: None,
                last_error: last_error.or_else(|| Some("no next".to_string())),
            },
        },
        None => Advanced {
            enabled: 0,
            next_run_at: None,
            last_error,
        },
    }
}

async fn ack(pool: &SqlitePool, id: i64, token: &str, advanced: &Advanced) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE scheduled_jobs
         SET enabled = ?2, next_run_at = ?3, last_error = ?4, claim_until = NULL, claim_token = NULL
         WHERE id = ?1 AND claim_token = ?5",
    )
    .bind(id)
    .bind(advanced.enabled)
    .bind(advanced.next_run_at)
    .bind(&advanced.last_error)
    .bind(token)
    .execute(pool)
    .await
    .context("schedule ack")?;
    Ok(())
}

async fn release(pool: &SqlitePool, id: i64, token: &str, reason: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE scheduled_jobs
         SET claim_until = NULL, claim_token = NULL, last_error = ?3
         WHERE id = ?1 AND claim_token = ?2",
    )
    .bind(id)
    .bind(token)
    .bind(reason)
    .execute(pool)
    .await
    .context("schedule release")?;
    Ok(())
}

async fn holds(pool: &SqlitePool, id: i64, token: &str) -> anyhow::Result<bool> {
    let current: Option<String> =
        sqlx::query_scalar("SELECT claim_token FROM scheduled_jobs WHERE id = ?1 AND enabled = 1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .context("schedule hold")?;
    Ok(current.as_deref() == Some(token))
}

pub(crate) async fn load(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<Job>> {
    let row = sqlx::query(&format!("{LOAD_SQL} WHERE id = ?1"))
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("schedule load")?;
    row.as_ref().map(job_from).transpose()
}

fn job_from(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Job> {
    use sqlx::Row;
    Ok(Job {
        id: row.try_get(0)?,
        creator_qq: row.try_get(1)?,
        cron: row.try_get(2)?,
        at: row.try_get(3)?,
        action: row.try_get(4)?,
        kind: row.try_get(5)?,
        group_id: row.try_get(6)?,
        user_id: row.try_get(7)?,
        payload: row.try_get(8)?,
        enabled: row.try_get(9)?,
        created_at: row.try_get(10)?,
        next_run_at: row.try_get(11)?,
        claim_token: row.try_get(13)?,
        last_error: row.try_get(14)?,
    })
}

fn job_json(job: &Job) -> Value {
    json!({
        "id": job.id,
        "creator_qq": job.creator_qq,
        "cron": job.cron,
        "at": job.at,
        "action": job.action,
        "kind": job.kind,
        "group_id": job.group_id,
        "user_id": job.user_id,
        "payload": job.payload,
        "enabled": job.enabled != 0,
        "created_at": job.created_at,
        "next_run_at": job.next_run_at,
        "last_error": job.last_error,
    })
}

fn format_list(jobs: &[Job]) -> String {
    if jobs.is_empty() {
        return "还没有定时任务。".to_string();
    }
    jobs.iter()
        .map(|job| {
            let state = if job.enabled == 0 { "已取消" } else { "开" };
            let when = match (&job.cron, job.at) {
                (Some(cron), _) => format!("cron {cron}"),
                (None, Some(at)) => format!("一次性 {at}"),
                _ => "时间不明".to_string(),
            };
            let target = if job.kind == "group" {
                format!("群 {}", job.group_id.unwrap_or(0))
            } else {
                "私聊自己".to_string()
            };
            let next = job
                .next_run_at
                .map(|stamp| stamp.to_string())
                .unwrap_or_else(|| "-".to_string());
            format!(
                "#{id} {target} {when} {state} 下次 {next}\n{payload}",
                id = job.id,
                payload = job.payload
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_cancel(id: Option<&str>, extra: bool) -> HumanCmd {
    if extra {
        return HumanCmd::Bad;
    }
    match id.and_then(positive_token) {
        Some(id) => HumanCmd::Cancel(id),
        None => HumanCmd::Bad,
    }
}

fn parse_create<'a, I>(mut parts: I) -> HumanCmd
where
    I: Iterator<Item = &'a str>,
{
    let (kind, group_id, when_word) = match parts.next().unwrap_or("") {
        "group" => {
            let Some(group_id) = parts.next().and_then(positive_token) else {
                return HumanCmd::Bad;
            };
            ("group", Some(group_id), parts.next().unwrap_or(""))
        }
        "private" => ("private", None, parts.next().unwrap_or("")),
        _ => return HumanCmd::Bad,
    };
    let when = match when_word {
        "at" => match parts.next().and_then(positive_token) {
            Some(at) => When::At(at),
            None => return HumanCmd::Bad,
        },
        "cron" => {
            let fields: Vec<&str> = parts.by_ref().take(5).collect();
            if fields.len() != 5 {
                return HumanCmd::Bad;
            }
            When::Cron(fields.join(" "))
        }
        _ => return HumanCmd::Bad,
    };
    let payload = parts.collect::<Vec<_>>().join(" ");
    if payload.trim().is_empty() {
        return HumanCmd::Bad;
    }
    HumanCmd::Create {
        kind: kind.to_string(),
        group_id,
        when,
        payload,
    }
}

fn positive_token(text: &str) -> Option<i64> {
    text.parse::<i64>().ok().filter(|value| *value > 0)
}

fn action_ok(args: &Value) -> bool {
    match args.get("action") {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.trim() == ACTION,
        Some(_) => false,
    }
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, Fail> {
    match args.get(key) {
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(text),
        _ => Err(Fail::Bad),
    }
}

fn required_id(args: &Value, key: &str) -> Result<i64, Fail> {
    arg_i64(args, key).filter(|id| *id > 0).ok_or(Fail::Bad)
}

fn opt_string(args: &Value, key: &str) -> Result<Option<String>, Fail> {
    match presence_string(args, key)? {
        None => Ok(None),
        Some(Some(text)) => Ok(Some(text)),
        Some(None) => Err(Fail::Bad),
    }
}

fn opt_i64(args: &Value, key: &str) -> Result<Option<i64>, Fail> {
    match presence_i64(args, key)? {
        None => Ok(None),
        Some(Some(value)) => Ok(Some(value)),
        Some(None) => Err(Fail::Bad),
    }
}

fn opt_bool(args: &Value, key: &str) -> Result<Option<bool>, Fail> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(*flag)),
        Some(Value::Number(number)) => number
            .as_i64()
            .map(|value| Some(value != 0))
            .ok_or(Fail::Bad),
        Some(_) => Err(Fail::Bad),
    }
}

fn presence_string(args: &Value, key: &str) -> Result<Option<Option<String>>, Fail> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim();
            if text.is_empty() {
                Ok(Some(None))
            } else {
                Ok(Some(Some(text.to_string())))
            }
        }
        Some(_) => Err(Fail::Bad),
    }
}

fn presence_i64(args: &Value, key: &str) -> Result<Option<Option<i64>>, Fail> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => Ok(Some(arg_i64(args, key))),
    }
}

fn arg_i64(args: &Value, key: &str) -> Option<i64> {
    let value = args.get(key)?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|item| i64::try_from(item).ok()))
        .or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().parse::<i64>().ok())
        })
        .filter(|item| *item > 0)
}

fn claim_token(id: i64, now: i64) -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    format!("{id}:{now}:{}", N.fetch_add(1, Ordering::Relaxed))
}

fn clip_err(err: &anyhow::Error) -> String {
    let text = err.to_string();
    let mut out: String = text.chars().take(180).collect();
    if text.chars().count() > 180 {
        out.push('…');
    }
    out
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod schedule_tests;
