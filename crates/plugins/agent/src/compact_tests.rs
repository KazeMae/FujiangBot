use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};

use super::*;

fn host() -> HostUsage {
    HostUsage {
        user_id: 33,
        now: 86_400,
    }
}
use crate::client::{ChatMessage, ImagePart, LlmClient, LlmError, Role};
use crate::plan::{plan_chat, ChatPlan};
use crate::session::{self, save_exchange};
use crate::tools::{self, BuiltinTools, ToolHub, ToolPass, ToolSpec};
use crate::AgentConfig;

#[derive(Clone, Copy)]
enum Mode {
    Summary,
    Empty,
    Boom,
    Huge,
    Hang,
    Tool,
    Reset,
    Forget,
    Save,
    Persona,
    ForgetEmpty,
    MeteredEmpty,
}

#[derive(Clone)]
struct App {
    hits: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Value>>>,
    mode: Mode,
    pool: sqlx::SqlitePool,
    key: String,
}

async fn pool() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    session::ensure_schema(&pool).await.unwrap();
    pool
}

fn cfg(base: &str, limit: u64) -> AgentConfig {
    let mut cfg = AgentConfig::from_value(&json!({
        "model": "local-model",
        "system_prompt": "人设",
        "context_budget": limit,
        "model_context_limit": limit,
        "max_history_turns": 1,
        "timeout_secs": 60,
        "enable_rank_tools": false,
        "enable_contest_tools": false,
        "enable_problem_tools": false,
        "enable_memo_tools": false,
        "enable_fun_tools": false,
        "enable_memory_tools": false,
        "rate_limit": { "per_user_per_hour": 0, "per_group_per_hour": 0 }
    }));
    cfg.base_url = base.to_string();
    cfg
}

async fn seed(pool: &sqlx::SqlitePool, key: &str) {
    session::save_faced(
        pool,
        key,
        &session::UserSnap {
            user_id: 11,
            nickname: Some("旧称".into()),
            card: Some("旧牌".into()),
            reply_id: Some(1101),
        },
        &"x".repeat(62_000),
        "旧回复",
    )
    .await
    .unwrap();
    session::save_faced(
        pool,
        key,
        &session::UserSnap {
            user_id: 22,
            nickname: Some("近称".into()),
            card: Some("近牌".into()),
            reply_id: Some(2202),
        },
        "近期",
        "近期回复",
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO layered_memories (scope_kind, scope_id, key, value, updated_at, source)
         VALUES ('group', '7', 'secret', '分层秘密XYZ', 1, 'group:7')",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn serve(app: App) -> String {
    let app = Router::new()
        .route("/responses", post(handle))
        .with_state(app);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for _ in 0..50 {
        if TcpStream::connect(addr).await.is_ok() {
            return format!("http://{addr}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("mock llm did not accept");
}

async fn handle(State(app): State<App>, Json(body): Json<Value>) -> Json<Value> {
    app.hits.fetch_add(1, Ordering::SeqCst);
    app.bodies.lock().unwrap().push(body);
    match app.mode {
        Mode::Summary => Json(text("摘要OK")),
        Mode::Empty => Json(json!({"output_text": "   "})),
        Mode::Boom => Json(json!({"error": {"message": "no"}})),
        Mode::Huge => Json(text(&"长".repeat(40_000))),
        Mode::Hang => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Json(text("晚了"))
        }
        Mode::Tool => Json(json!({
            "output": [{
                "type": "function_call",
                "call_id": "c1",
                "name": "rank_list",
                "arguments": "{}"
            }]
        })),
        Mode::Reset => {
            session::clear_messages(&app.pool, &app.key).await.unwrap();
            session::clear_summary(&app.pool, &app.key).await.unwrap();
            Json(text("新摘要"))
        }
        Mode::Forget => {
            session::clear_summary(&app.pool, &app.key).await.unwrap();
            Json(text("新摘要"))
        }
        Mode::Save => {
            save_exchange(&app.pool, &app.key, 44, "新回合", "新回复")
                .await
                .unwrap();
            Json(text("新摘要"))
        }
        Mode::Persona => {
            session::set_persona(&app.pool, &app.key, "strict", 9)
                .await
                .unwrap();
            Json(text("新摘要"))
        }
        Mode::ForgetEmpty => {
            session::clear_summary(&app.pool, &app.key).await.unwrap();
            Json(text("新摘要"))
        }
        Mode::MeteredEmpty => Json(json!({
            "output_text": "   ",
            "usage": {"input_tokens": 9, "output_tokens": 4}
        })),
    }
}

fn text(body: &str) -> Value {
    json!({
        "output_text": body,
        "usage": {"input_tokens": 9, "output_tokens": 4}
    })
}

async fn usage_of(pool: &sqlx::SqlitePool) -> (i64, i64, i64, i64) {
    let row: Option<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT calls, in_tokens, out_tokens, user_id FROM usage_daily WHERE session_key = 'group:7'",
    )
    .fetch_optional(pool)
    .await
    .unwrap();
    row.unwrap_or((0, 0, 0, 0))
}

fn client(cfg: &AgentConfig) -> LlmClient {
    LlmClient::new(cfg, reqwest::Client::new(), "test-key".into())
}

async fn speak(pool: &sqlx::SqlitePool, cfg: &AgentConfig, key: &str) -> Vec<ChatMessage> {
    let plan = plan_chat(pool, cfg, key, 33, "当前", 5, true)
        .await
        .unwrap();
    let ChatPlan::Speak { messages } = plan else {
        panic!("speak");
    };
    messages
}

fn rows(messages: &[ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .map(|message| message.content.clone())
        .collect()
}

#[tokio::test]
async fn compact_keeps_recent_speaker_and_leaves_layered_memory() {
    let pool = pool().await;
    let key = "group:7";
    seed(&pool, key).await;
    let state = App {
        hits: Arc::new(AtomicUsize::new(0)),
        bodies: Arc::new(Mutex::new(Vec::new())),
        mode: Mode::Summary,
        pool: pool.clone(),
        key: key.to_string(),
    };
    let bodies = state.bodies.clone();
    let hits = state.hits.clone();
    let base = serve(state).await;
    let cfg = cfg(&base, 70_000);
    let messages = speak(&pool, &cfg, key).await;
    let out = before_turn(&pool, &cfg, &client(&cfg), key, messages, &[], host()).await;
    let Ok(messages) = out else {
        panic!("expected compact");
    };
    assert_eq!(session::summary_of(&pool, key).await.unwrap(), "摘要OK");
    let left = session::active_messages(&pool, key).await.unwrap();
    assert_eq!(left.len(), 2);
    assert_eq!(left[0].content, "近期");
    assert_eq!(left[0].user_id, Some(22));
    assert_eq!(left[0].nickname.as_deref(), Some("近称"));
    assert_eq!(left[0].card.as_deref(), Some("近牌"));
    assert_eq!(left[0].reply_id, Some(2202));
    assert!(rows(&messages)
        .iter()
        .any(|row| row.contains("\"user_id\":22")));
    assert!(rows(&messages).iter().any(|row| row.contains("当前")));
    assert!(!rows(&messages)
        .iter()
        .any(|row| row.contains(&"x".repeat(100))));
    let secret: String =
        sqlx::query_scalar("SELECT value FROM layered_memories WHERE key = 'secret'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(secret, "分层秘密XYZ");
    {
        let seen = bodies.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let body = &seen[0];
        assert!(body.get("tools").is_none());
        assert_eq!(body["max_output_tokens"], 4096);
        let content = body["input"][0]["content"].as_str().expect("transcript");
        assert!(!content.contains("分层秘密XYZ"));
        let mut saw_old_sender = false;
        for line in content.lines() {
            let Ok(item) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if item["host_sender"]["user_id"].as_i64() == Some(11) {
                saw_old_sender = true;
                assert_eq!(item["untrusted"]["nickname"], "旧称");
                assert_eq!(item["untrusted"]["card"], "旧牌");
                assert_eq!(item["host_reply"]["message_id"], 1101);
            }
        }
        assert!(saw_old_sender);
    }
    assert_eq!(usage_of(&pool).await, (1, 9, 4, 33));
}

async fn keeps_history(mode: Mode) {
    let pool = pool().await;
    let key = "group:7";
    seed(&pool, key).await;
    if matches!(mode, Mode::Forget) {
        sqlx::query("UPDATE sessions SET summary = '旧摘要' WHERE session_key = ?1")
            .bind(key)
            .execute(&pool)
            .await
            .unwrap();
    }
    let state = App {
        hits: Arc::new(AtomicUsize::new(0)),
        bodies: Arc::new(Mutex::new(Vec::new())),
        mode,
        pool: pool.clone(),
        key: key.to_string(),
    };
    let hits = state.hits.clone();
    let base = serve(state).await;
    let mut cfg = cfg(&base, 70_000);
    if matches!(mode, Mode::Hang) {
        cfg.timeout_secs = 1;
    }
    let before = session::active_messages(&pool, key).await.unwrap().len();
    let messages = speak(&pool, &cfg, key).await;
    let out = before_turn(&pool, &cfg, &client(&cfg), key, messages, &[], host()).await;
    if matches!(
        mode,
        Mode::Reset | Mode::Forget | Mode::ForgetEmpty | Mode::Save | Mode::Persona
    ) {
        assert!(out.is_err());
    }
    let summary = session::summary_of(&pool, key).await.unwrap();
    assert_ne!(summary, "新摘要");
    assert_ne!(summary, "摘要OK");
    assert!(!summary.contains('长'));
    if matches!(mode, Mode::Reset) {
        assert!(session::active_messages(&pool, key)
            .await
            .unwrap()
            .is_empty());
        assert!(summary.is_empty());
    } else if matches!(mode, Mode::Save) {
        let left = session::active_messages(&pool, key).await.unwrap();
        assert!(left.len() > before);
        assert!(left.iter().any(|row| row.content.len() == 62_000));
    } else if !matches!(mode, Mode::Forget | Mode::ForgetEmpty) {
        assert_eq!(
            session::active_messages(&pool, key).await.unwrap().len(),
            before
        );
        assert!(summary.is_empty());
    } else {
        assert!(summary.is_empty());
        assert_eq!(
            session::active_messages(&pool, key).await.unwrap().len(),
            before
        );
    }
    let secret: String =
        sqlx::query_scalar("SELECT value FROM layered_memories WHERE key = 'secret'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(secret, "分层秘密XYZ");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let (calls, inn, out, user) = usage_of(&pool).await;
    assert_eq!(calls, 1);
    assert_eq!(user, 33);
    if matches!(mode, Mode::Empty | Mode::Boom | Mode::Hang) {
        assert_eq!((inn, out), (0, 0));
    } else {
        assert_eq!((inn, out), (9, 4));
    }
}

#[tokio::test]
async fn failed_empty_huge_timeout_and_races_keep_history() {
    for mode in [
        Mode::Empty,
        Mode::Boom,
        Mode::Huge,
        Mode::Hang,
        Mode::Reset,
        Mode::Forget,
        Mode::Save,
        Mode::Persona,
        Mode::ForgetEmpty,
    ] {
        keeps_history(mode).await;
    }
}

#[tokio::test]
async fn zero_history_turns_keeps_one_exchange() {
    let pool = pool().await;
    let key = "group:7";
    seed(&pool, key).await;
    let state = App {
        hits: Arc::new(AtomicUsize::new(0)),
        bodies: Arc::new(Mutex::new(Vec::new())),
        mode: Mode::Summary,
        pool: pool.clone(),
        key: key.to_string(),
    };
    let base = serve(state).await;
    let mut cfg = cfg(&base, 70_000);
    cfg.max_history_turns = 0;
    let messages = speak(&pool, &cfg, key).await;
    let out = before_turn(&pool, &cfg, &client(&cfg), key, messages, &[], host()).await;
    let Ok(_) = out else {
        panic!("expected compact");
    };
    let left = session::active_messages(&pool, key).await.unwrap();
    assert_eq!(left.len(), 2);
    assert_eq!(left[0].content, "近期");
    assert_eq!(session::summary_of(&pool, key).await.unwrap(), "摘要OK");
}

#[tokio::test]
async fn closed_window_image_and_tool_growth_do_not_drop_history() {
    let pool = pool().await;
    let key = "group:7";
    save_exchange(&pool, key, 5, "短", "答").await.unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let state = App {
        hits: hits.clone(),
        bodies: Arc::new(Mutex::new(Vec::new())),
        mode: Mode::Tool,
        pool: pool.clone(),
        key: key.to_string(),
    };
    let base = serve(state).await;
    let mut closed = cfg(&base, 50_000);
    closed.model_context_limit = None;
    let err = client(&closed)
        .complete(&[ChatMessage {
            role: Role::User,
            content: "hi".into(),
            images: Vec::new(),
        }])
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::ContextClosed));
    let messages = speak(&pool, &closed, key).await;
    let closed_turn =
        before_turn(&pool, &closed, &client(&closed), key, messages, &[], host()).await;
    assert!(matches!(closed_turn, Err(Gate::Closed)));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert_eq!(usage_of(&pool).await, (0, 0, 0, 0));
    let mut open = cfg(&base, 20_000);
    let err = client(&open)
        .complete(&[ChatMessage {
            role: Role::User,
            content: "看图".into(),
            images: vec![ImagePart::new("data:image/png;base64,aa".into())],
        }])
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::ImageUnmetered));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    open.model = "local-model".into();
    let spec = ToolSpec::parse("rank.list", "榜", json!({"type": "object"})).unwrap();
    let before_hits = hits.load(Ordering::SeqCst);
    let turn = tools::run_tool_loop(
        &client(&open),
        &[ChatMessage {
            role: Role::User,
            content: "查".into(),
            images: Vec::new(),
        }],
        &[spec],
        BuiltinTools::OFF,
        &ToolPass::rounds(2),
        &Big,
    )
    .await;
    assert!(matches!(turn.reply, Err(LlmError::OverBudget)));
    assert_eq!(hits.load(Ordering::SeqCst), before_hits + 1);
    assert_eq!(session::active_messages(&pool, key).await.unwrap().len(), 2);
}

#[tokio::test]
async fn metered_image_is_sent_and_unmetered_or_oversized_is_not() {
    let pool = pool().await;
    let key = "group:7";
    let hits = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let state = App {
        hits: hits.clone(),
        bodies: bodies.clone(),
        mode: Mode::Summary,
        pool: pool.clone(),
        key: key.to_string(),
    };
    let base = serve(state).await;
    let mut cfg = cfg(&base, 80_000);
    cfg.image_token_upper_bound = Some(1_024);
    let url = "data:image/png;base64,aa";
    let message = ChatMessage {
        role: Role::User,
        content: "正文里的 image_url 不是图片字段".into(),
        images: vec![ImagePart::new(url.into())],
    };
    let spec = ToolSpec::parse("rank.list", "榜", json!({"type": "object"})).unwrap();
    let turn = tools::run_tool_loop(
        &client(&cfg),
        &[message],
        &[spec],
        BuiltinTools::OFF,
        &ToolPass::rounds(2),
        &Big,
    )
    .await;
    assert!(turn.reply.is_ok());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    {
        let seen = bodies.lock().unwrap();
        let parts = seen[0]["input"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["text"], "正文里的 image_url 不是图片字段");
        assert_eq!(parts[1]["image_url"], url);
    }
    let mut many = ChatMessage {
        role: Role::User,
        content: "多图".into(),
        images: Vec::new(),
    };
    for _ in 0..601 {
        many.images.push(ImagePart::new(url.into()));
    }
    let err = client(&cfg).complete(&[many]).await.unwrap_err();
    assert!(matches!(err, LlmError::PayloadLimited));
    cfg.image_token_upper_bound = None;
    let err = client(&cfg)
        .complete(&[ChatMessage {
            role: Role::User,
            content: "看".into(),
            images: vec![ImagePart::new(url.into())],
        }])
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::ImageUnmetered));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    cfg.image_token_upper_bound = Some(1_024);
    cfg.model_context_limit = Some(2_000);
    cfg.context_budget = 2_000;
    let err = client(&cfg)
        .complete(&[ChatMessage {
            role: Role::User,
            content: "看".into(),
            images: vec![ImagePart::new(url.into())],
        }])
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::OverBudget));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    cfg.model_context_limit = Some(80_000);
    cfg.context_budget = 80_000;
    cfg.image_token_upper_bound = None;
    client(&cfg)
        .complete(&[ChatMessage {
            role: Role::User,
            content: "纯文本".into(),
            images: Vec::new(),
        }])
        .await
        .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn empty_output_with_usage_records_nine_four_and_keeps_history() {
    let pool = pool().await;
    let key = "group:7";
    seed(&pool, key).await;
    let state = App {
        hits: Arc::new(AtomicUsize::new(0)),
        bodies: Arc::new(Mutex::new(Vec::new())),
        mode: Mode::MeteredEmpty,
        pool: pool.clone(),
        key: key.to_string(),
    };
    let hits = state.hits.clone();
    let base = serve(state).await;
    let cfg = cfg(&base, 70_000);
    let before = session::active_messages(&pool, key).await.unwrap().len();
    let messages = speak(&pool, &cfg, key).await;
    let original = messages.clone();
    let out = before_turn(&pool, &cfg, &client(&cfg), key, messages, &[], host()).await;
    let Ok(restored) = out else {
        panic!("expected original history");
    };
    assert_eq!(restored, original);
    assert!(session::summary_of(&pool, key).await.unwrap().is_empty());
    assert_eq!(
        session::active_messages(&pool, key).await.unwrap().len(),
        before
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(usage_of(&pool).await, (1, 9, 4, 33));
}

#[tokio::test]
async fn usage_write_failure_after_one_call_stays_busy() {
    let pool = pool().await;
    let key = "group:7";
    seed(&pool, key).await;
    let state = App {
        hits: Arc::new(AtomicUsize::new(0)),
        bodies: Arc::new(Mutex::new(Vec::new())),
        mode: Mode::Summary,
        pool: pool.clone(),
        key: key.to_string(),
    };
    let hits = state.hits.clone();
    let base = serve(state).await;
    sqlx::query("DROP TABLE usage_daily")
        .execute(&pool)
        .await
        .unwrap();
    let cfg = cfg(&base, 70_000);
    let before = session::active_messages(&pool, key).await.unwrap();
    let messages = speak(&pool, &cfg, key).await;
    let out = before_turn(&pool, &cfg, &client(&cfg), key, messages, &[], host()).await;
    assert!(matches!(out, Err(Gate::Busy)));
    assert!(session::summary_of(&pool, key).await.unwrap().is_empty());
    let left = session::active_messages(&pool, key).await.unwrap();
    assert_eq!(left.len(), before.len());
    assert_eq!(left[0].content, before[0].content);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn compact_keeps_the_original_image_on_the_returned_turn() {
    let pool = pool().await;
    let key = "group:7";
    seed(&pool, key).await;
    let state = App {
        hits: Arc::new(AtomicUsize::new(0)),
        bodies: Arc::new(Mutex::new(Vec::new())),
        mode: Mode::Summary,
        pool: pool.clone(),
        key: key.to_string(),
    };
    let hits = state.hits.clone();
    let bodies = state.bodies.clone();
    let base = serve(state).await;
    let mut cfg = cfg(&base, 70_000);
    cfg.image_token_upper_bound = Some(1_024);
    let url = "data:image/png;base64,\"quoted\\path";
    let mut messages = speak(&pool, &cfg, key).await;
    messages
        .last_mut()
        .unwrap()
        .images
        .push(ImagePart::new(url.into()));
    let n = client(&cfg).request_bytes(&messages, &[], &[]).unwrap();
    assert!((63_000..=70_000).contains(&n));
    let current_text = messages.last().unwrap().content.clone();
    let out = before_turn(&pool, &cfg, &client(&cfg), key, messages, &[], host()).await;
    let Ok(messages) = out else {
        panic!("expected compact");
    };
    assert_eq!(messages.last().unwrap().images[0].data_url(), url);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    {
        let seen = bodies.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let body = seen[0].to_string();
        assert!(!body.contains(url));
        assert_eq!(seen[0]["max_output_tokens"], 4096);
    }
    assert_eq!(session::summary_of(&pool, key).await.unwrap(), "摘要OK");
    client(&cfg).complete(&messages).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    {
        let seen = bodies.lock().unwrap();
        let parts = seen[1]["input"].as_array().unwrap().last().unwrap()["content"]
            .as_array()
            .unwrap();
        assert_eq!(parts[0]["text"], current_text);
        assert_eq!(parts[1]["image_url"], url);
    }
}

struct Big;

#[async_trait]
impl ToolHub for Big {
    async fn invoke(&self, _: &str, _: &str, _: Value) -> anyhow::Result<Value> {
        Ok(json!("y".repeat(30_000)))
    }
}
