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
    save_exchange(pool, key, 11, &"x".repeat(62_000), "旧回复")
        .await
        .unwrap();
    save_exchange(pool, key, 22, "近期", "近期回复")
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
        Mode::Empty => Json(text("   ")),
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
    }
}

fn text(body: &str) -> Value {
    json!({"output_text": body})
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
    let out = before_turn(&pool, &cfg, &client(&cfg), key, messages, &[]).await;
    let Ok(messages) = out else {
        panic!("expected compact");
    };
    assert_eq!(session::summary_of(&pool, key).await.unwrap(), "摘要OK");
    let left = session::active_messages(&pool, key).await.unwrap();
    assert_eq!(left.len(), 2);
    assert_eq!(left[0].content, "近期");
    assert_eq!(left[0].user_id, Some(22));
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
        }
    }
    assert!(saw_old_sender);
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
    let out = before_turn(&pool, &cfg, &client(&cfg), key, messages, &[]).await;
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
    let out = before_turn(&pool, &cfg, &client(&cfg), key, messages, &[]).await;
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

struct Big;

#[async_trait]
impl ToolHub for Big {
    async fn invoke(&self, _: &str, _: &str, _: Value) -> anyhow::Result<Value> {
        Ok(json!("y".repeat(30_000)))
    }
}
