use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use super::*;
use crate::client::{ChatMessage, LlmClient, Role};
use crate::config::AgentConfig;

#[test]
fn parses_service_method_and_rejects_a_bad_name() {
    let spec = ToolSpec::parse(
        " rank.list ",
        " 当前榜 ",
        json!({"type": "object", "properties": {"limit": {"type": "integer"}}}),
    )
    .unwrap();
    assert_eq!(spec.name(), "rank.list");
    assert_eq!(spec.wire_name(), "rank_list");
    assert_eq!(spec.service(), "rank");
    assert_eq!(spec.method(), "list");
    assert_eq!(spec.description(), "当前榜");

    let nested = ToolSpec::parse("contest.upcoming.more", "x", json!(1)).unwrap();
    assert_eq!(nested.service(), "contest");
    assert_eq!(nested.method(), "upcoming.more");
    assert_eq!(nested.wire_name(), "contest_upcoming_more");
    assert_eq!(nested.parameters(), &default_parameters());

    assert!(ToolSpec::parse("rank", "x", json!({})).is_none());
    assert!(ToolSpec::parse(".list", "x", json!({})).is_none());
    assert!(ToolSpec::parse("rank.", "x", json!({})).is_none());
    assert!(ToolSpec::parse("has space.list", "x", json!({})).is_none());
    assert!(assemble_tools(&[], BuiltinTools::OFF).is_empty());

    let first = ToolSpec::parse("a.b_c", "one", json!({})).unwrap();
    let second = ToolSpec::parse("a_b.c", "two", json!({})).unwrap();
    assert_eq!(first.wire_name(), "a_b_c");
    let kept = assemble_tools(&[first, second], BuiltinTools::OFF);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name(), "a.b_c");
    assert_eq!(kept[0].service(), "a");
    assert_eq!(kept[0].method(), "b_c");
}

#[test]
fn builtin_tools_follow_the_two_switches() {
    let on = assemble_tools(&[], BuiltinTools::ON);
    assert_eq!(
        on.iter().map(ToolSpec::wire_name).collect::<Vec<_>>(),
        vec!["rank_list", "contest_upcoming", "contest_today"]
    );
    assert_eq!(on[0].service(), "rank");
    assert_eq!(on[0].method(), "list");
    assert_eq!(
        on[0].description(),
        "列出本群 Codeforces 榜上的队员，含 handle 和 rating。不需要参数。"
    );
    assert_eq!(on[0].parameters(), &default_parameters());
    assert_eq!(on[1].service(), "contest");
    assert_eq!(on[1].method(), "upcoming");
    assert_eq!(
        on[1].description(),
        "列出即将开始的比赛。可选字符串参数 oj，例如 Codeforces 或 Atcoder。不传则返回全部。"
    );
    assert_eq!(
        on[1].parameters(),
        &json!({
            "type": "object",
            "properties": { "oj": { "type": "string" } }
        })
    );
    assert!(on[1].parameters().get("required").is_none());
    assert_eq!(on[2].method(), "today");
    assert_eq!(on[2].description(), "列出今天开始的比赛。不需要参数。");
    assert_eq!(on[2].parameters(), &default_parameters());

    let rank_only = assemble_tools(
        &[],
        BuiltinTools {
            rank: true,
            contest: false,
        },
    );
    assert_eq!(
        rank_only
            .iter()
            .map(ToolSpec::wire_name)
            .collect::<Vec<_>>(),
        vec!["rank_list"]
    );
    let contest_only = assemble_tools(
        &[],
        BuiltinTools {
            rank: false,
            contest: true,
        },
    );
    assert_eq!(
        contest_only
            .iter()
            .map(ToolSpec::wire_name)
            .collect::<Vec<_>>(),
        vec!["contest_upcoming", "contest_today"]
    );
    assert!(assemble_tools(&[], BuiltinTools::OFF).is_empty());

    let extra = ToolSpec::parse("memo.search", "查备忘", json!({})).unwrap();
    let with_extra = assemble_tools(&[extra], BuiltinTools::ON);
    assert_eq!(with_extra[3].name(), "memo.search");
    assert_eq!(with_extra[3].wire_name(), "memo_search");

    let configured = ToolSpec::parse(
        "rank.list",
        "别的说明",
        json!({"type": "object", "properties": {"limit": {"type": "integer"}}}),
    )
    .unwrap();
    let kept_builtin = assemble_tools(std::slice::from_ref(&configured), BuiltinTools::ON);
    assert_eq!(kept_builtin.len(), 3);
    assert_eq!(kept_builtin[0].description(), on[0].description());
    assert_eq!(kept_builtin[0].parameters(), on[0].parameters());

    let kept_config = assemble_tools(&[configured], BuiltinTools::OFF);
    assert_eq!(kept_config.len(), 1);
    assert_eq!(kept_config[0].description(), "别的说明");
    assert_eq!(kept_config[0].service(), "rank");
    assert_eq!(kept_config[0].method(), "list");
}

#[tokio::test]
async fn tool_call_round_trips_through_the_hub_then_returns_text() {
    let script = Script::new(Kind::ToolThenText);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let schema = json!({"type": "object", "properties": {"limit": {"type": "integer"}}});
    let tools = vec![
        ToolSpec::parse("rank.list", "当前榜", schema.clone()).unwrap(),
        ToolSpec::parse("contest.upcoming", "近期比赛", json!({"type": "object"})).unwrap(),
    ];
    let hub = Rec::default();
    let messages = vec![
        ChatMessage {
            role: Role::System,
            content: "be brief".into(),
            images: Vec::new(),
        },
        ChatMessage {
            role: Role::User,
            content: "最近有比赛吗".into(),
            images: Vec::new(),
        },
    ];
    let reply = run_tool_loop(&client, &messages, &tools, BuiltinTools::OFF, 3, &hub)
        .await
        .unwrap();
    assert_eq!(reply, ToolReply::Text("本周有两场".into()));
    assert_eq!(
        hub.log.lock().unwrap().clone(),
        vec![
            ("rank".into(), "list".into(), json!({"limit": 2})),
            ("contest".into(), "upcoming".into(), json!({})),
        ]
    );
    let bodies = script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[0].get("tools").is_some());
    assert_eq!(bodies[0]["tools"][0]["type"], "function");
    assert_eq!(bodies[0]["tools"][0]["name"], "rank_list");
    assert_eq!(bodies[0]["tools"][0]["description"], "当前榜");
    assert_eq!(bodies[0]["tools"][0]["parameters"], schema);
    assert_eq!(bodies[0]["tools"][1]["name"], "contest_upcoming");
    assert_eq!(bodies[0]["instructions"], "be brief");
    assert_eq!(bodies[0]["input"].as_array().unwrap().len(), 1);
    assert!(bodies[0].get("previous_response_id").is_none());
    assert_eq!(bodies[1]["tools"][0]["name"], "rank_list");
    let input = bodies[1]["input"].as_array().unwrap();
    assert_eq!(input.len(), 5);
    assert_eq!(input[0]["role"], "user");
    assert_eq!(input[0]["content"], "最近有比赛吗");
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["call_id"], "call_rank");
    assert_eq!(input[1]["name"], "rank_list");
    assert_eq!(input[1]["arguments"], r#"{"limit":2}"#);
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["call_id"], "call_rank");
    assert_eq!(input[2]["output"], r#"{"via":"rank.list"}"#);
    assert_eq!(input[3]["name"], "contest_upcoming");
    assert_eq!(input[3]["arguments"], "{}");
    assert_eq!(input[4]["type"], "function_call_output");
    assert_eq!(input[4]["output"], r#"{"via":"contest.upcoming"}"#);
    assert_eq!(bodies[1]["instructions"], "be brief");
    assert!(bodies[1].get("previous_response_id").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn round_cap_degrades_without_another_hub_call() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let script = Script::new(Kind::AlwaysTool);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let tools = vec![ToolSpec::parse("rank.list", "当前榜", json!({})).unwrap()];
    let hub = Rec::default();
    let reply = run_tool_loop(&client, &user("再查"), &tools, BuiltinTools::OFF, 1, &hub)
        .await
        .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_CAP_REPLY));
    assert_eq!(hub.log.lock().unwrap().len(), 1);
    assert_eq!(script.hits.load(Ordering::SeqCst), 2);
    assert!(catch.has("agent tool round cap"));

    let script = Script::new(Kind::AlwaysTool);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(&client, &user("再查"), &tools, BuiltinTools::OFF, 0, &hub)
        .await
        .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_CAP_REPLY));
    assert!(hub.log.lock().unwrap().is_empty());
    assert_eq!(script.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn hub_and_tool_errors_degrade_and_warn() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let tools = vec![ToolSpec::parse("rank.list", "当前榜", json!({})).unwrap()];

    let script = Script::new(Kind::ToolThenText);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec {
        log: Mutex::new(Vec::new()),
        fail: true,
    };
    let reply = run_tool_loop(&client, &user("榜"), &tools, BuiltinTools::OFF, 3, &hub)
        .await
        .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert_eq!(script.hits.load(Ordering::SeqCst), 1);
    assert_eq!(hub.log.lock().unwrap().len(), 1);
    assert!(catch.has("agent tool"));
    assert!(!catch.has("agent tool round cap"));

    let script = Script::new(Kind::BadArgs);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(&client, &user("榜"), &tools, BuiltinTools::OFF, 3, &hub)
        .await
        .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert!(hub.log.lock().unwrap().is_empty());
    assert_eq!(script.hits.load(Ordering::SeqCst), 1);
    assert!(catch.has("agent tool arguments"));

    let script = Script::new(Kind::Unknown);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(&client, &user("榜"), &tools, BuiltinTools::OFF, 3, &hub)
        .await
        .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert!(hub.log.lock().unwrap().is_empty());
    assert!(catch.has("agent tool unknown"));
}

#[tokio::test]
async fn empty_tools_call_complete_once() {
    let script = Script::new(Kind::Text);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(&client, &user("在吗"), &[], BuiltinTools::OFF, 3, &hub)
        .await
        .unwrap();
    assert_eq!(reply, ToolReply::Text("本周有两场".into()));
    assert!(hub.log.lock().unwrap().is_empty());
    assert_eq!(script.hits.load(Ordering::SeqCst), 1);
    let bodies = script.bodies.lock().unwrap();
    assert!(bodies[0].get("tools").is_none());
    assert!(bodies[0].get("previous_response_id").is_none());
}

#[tokio::test]
async fn missing_call_id_is_a_decode_error() {
    let script = Script::new(Kind::MissingId);
    let base = spawn_app(script).await;
    let client = test_client(&base);
    let tools = vec![ToolSpec::parse("rank.list", "当前榜", json!({})).unwrap()];
    let hub = Rec::default();
    let err = run_tool_loop(&client, &user("榜"), &tools, BuiltinTools::OFF, 3, &hub)
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Decode(_)), "{err}");
    assert!(hub.log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn builtin_rank_and_contest_calls_return_the_model_text() {
    let script = Script::new(Kind::ThreeThenText);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(
        &client,
        &user("今天有比赛吗，榜怎么样"),
        &[],
        BuiltinTools::ON,
        3,
        &hub,
    )
    .await
    .unwrap();
    assert_eq!(
        reply,
        ToolReply::Text("今天 Codeforces 一场。榜上 12 人。".into())
    );
    assert_eq!(
        hub.log.lock().unwrap().clone(),
        vec![
            ("rank".into(), "list".into(), json!({})),
            (
                "contest".into(),
                "upcoming".into(),
                json!({"oj": "Codeforces"})
            ),
            ("contest".into(), "today".into(), json!({})),
        ]
    );
    let bodies = script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    let names: Vec<_> = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["rank_list", "contest_upcoming", "contest_today"]
    );
    assert_eq!(
        bodies[0]["tools"][0]["description"],
        "列出本群 Codeforces 榜上的队员，含 handle 和 rating。不需要参数。"
    );
    assert_eq!(bodies[1]["input"][2]["output"], r#"{"via":"rank.list"}"#);
    assert_eq!(
        bodies[1]["input"][4]["output"],
        r#"{"via":"contest.upcoming"}"#
    );
    assert_eq!(
        bodies[1]["input"][6]["output"],
        r#"{"via":"contest.today"}"#
    );
}

#[tokio::test(flavor = "current_thread")]
async fn builtin_contest_today_error_uses_the_degraded_reply() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let script = Script::new(Kind::TodayCall);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec {
        log: Mutex::new(Vec::new()),
        fail: true,
    };
    let reply = run_tool_loop(
        &client,
        &user("今天有比赛吗"),
        &[],
        BuiltinTools::ON,
        3,
        &hub,
    )
    .await
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    let log = hub.log.lock().unwrap().clone();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].0, "contest");
    assert_eq!(log[0].1, "today");
    assert_eq!(script.hits.load(Ordering::SeqCst), 1);
    assert!(catch.has("agent tool"));
}

#[tokio::test]
async fn disabled_builtin_group_stays_out_of_the_request() {
    let script = Script::new(Kind::Text);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let extra = vec![ToolSpec::parse("memo.search", "查备忘", json!({})).unwrap()];
    let reply = run_tool_loop(
        &client,
        &user("今天比赛"),
        &extra,
        BuiltinTools {
            rank: false,
            contest: true,
        },
        3,
        &hub,
    )
    .await
    .unwrap();
    assert_eq!(reply, ToolReply::Text("本周有两场".into()));
    let bodies = script.bodies.lock().unwrap();
    let names: Vec<_> = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "contest_upcoming".to_string(),
            "contest_today".to_string(),
            "memo_search".to_string(),
        ]
    );
    assert!(hub.log.lock().unwrap().is_empty());
}

fn user(text: &str) -> Vec<ChatMessage> {
    vec![ChatMessage {
        role: Role::User,
        content: text.into(),
        images: Vec::new(),
    }]
}

fn test_client(base: &str) -> LlmClient {
    let mut cfg = AgentConfig::from_value(&json!({}));
    cfg.base_url = base.to_string();
    cfg.timeout_secs = 60;
    LlmClient::new(&cfg, reqwest::Client::new(), "test-key".into())
}

#[derive(Clone, Copy)]
enum Kind {
    ToolThenText,
    AlwaysTool,
    Text,
    BadArgs,
    Unknown,
    MissingId,
    ThreeThenText,
    TodayCall,
}

#[derive(Clone)]
struct Script {
    hits: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Value>>>,
    kind: Kind,
}

impl Script {
    fn new(kind: Kind) -> Self {
        Self {
            hits: Arc::new(AtomicUsize::new(0)),
            bodies: Arc::new(Mutex::new(Vec::new())),
            kind,
        }
    }
}

async fn spawn_app(script: Script) -> String {
    let app = Router::new()
        .route("/responses", post(handle))
        .with_state(script);
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
    panic!("mock llm server did not accept on {addr}");
}

async fn handle(State(script): State<Script>, Json(body): Json<Value>) -> Json<Value> {
    let n = script.hits.fetch_add(1, Ordering::SeqCst);
    script.bodies.lock().unwrap().push(body);
    Json(match script.kind {
        Kind::Text => text_body(),
        Kind::ToolThenText if n == 0 => two_calls(),
        Kind::ToolThenText => text_body(),
        Kind::AlwaysTool => one_call(&format!("call_{n}")),
        Kind::BadArgs if n == 0 => one_call_with("call_bad", "rank_list", "not-json"),
        Kind::BadArgs => text_body(),
        Kind::Unknown if n == 0 => one_call_with("call_x", "no_such", "{}"),
        Kind::Unknown => text_body(),
        Kind::ThreeThenText if n == 0 => three_calls(),
        Kind::ThreeThenText => saying("今天 Codeforces 一场。榜上 12 人。"),
        Kind::TodayCall => one_call_with("call_today", "contest_today", "{}"),
        Kind::MissingId => json!({
            "output": [{
                "type": "function_call",
                "name": "rank_list",
                "arguments": "{}"
            }]
        }),
    })
}

fn text_body() -> Value {
    saying("本周有两场")
}

fn saying(text: &str) -> Value {
    json!({
        "output_text": text,
        "output": [{
            "type": "message",
            "content": [{"type": "output_text", "text": "NO"}]
        }]
    })
}

fn three_calls() -> Value {
    json!({
        "output": [
            {
                "type": "function_call",
                "call_id": "call_rank",
                "name": "rank_list",
                "arguments": "{}"
            },
            {
                "type": "function_call",
                "call_id": "call_up",
                "name": "contest_upcoming",
                "arguments": "{\"oj\":\"Codeforces\"}"
            },
            {
                "type": "function_call",
                "call_id": "call_today",
                "name": "contest_today",
                "arguments": "{}"
            }
        ]
    })
}

fn two_calls() -> Value {
    json!({
        "output": [
            {
                "type": "function_call",
                "call_id": "call_rank",
                "name": "rank_list",
                "arguments": "{\"limit\":2}"
            },
            {
                "type": "function_call",
                "call_id": "call_contest",
                "name": "contest_upcoming",
                "arguments": "{}"
            }
        ]
    })
}

fn one_call(call_id: &str) -> Value {
    one_call_with(call_id, "rank_list", "{}")
}

fn one_call_with(call_id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "output": [{
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": arguments
        }]
    })
}

#[derive(Default)]
struct Rec {
    log: Mutex<Vec<(String, String, Value)>>,
    fail: bool,
}

#[async_trait]
impl ToolHub for Rec {
    async fn invoke(&self, service: &str, method: &str, args: Value) -> anyhow::Result<Value> {
        self.log
            .lock()
            .unwrap()
            .push((service.to_string(), method.to_string(), args));
        if self.fail {
            anyhow::bail!("service down");
        }
        Ok(json!({"via": format!("{service}.{method}")}))
    }
}

#[derive(Clone, Default)]
struct Catch(Arc<Mutex<Vec<String>>>);

impl Catch {
    fn has(&self, needle: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains(needle))
    }
}

struct Msg(String);

impl Visit for Msg {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl Subscriber for Catch {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::WARN
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Id::from_u64(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut msg = Msg(String::new());
        event.record(&mut msg);
        self.0.lock().unwrap().push(msg.0);
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}
