use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};

use super::*;

#[test]
fn blank_or_missing_key_is_err() {
    let missing = require_key("FUJIANG_LLM_KEY", None).unwrap_err();
    assert!(matches!(missing, LlmError::MissingApiKey { env } if env == "FUJIANG_LLM_KEY"));
    let blank = require_key("FUJIANG_LLM_KEY", Some(" \n")).unwrap_err();
    assert!(matches!(blank, LlmError::MissingApiKey { .. }));
    assert_eq!(
        require_key("FUJIANG_LLM_KEY", Some("  sk-test  ")).unwrap(),
        "sk-test"
    );
}

#[tokio::test]
async fn from_config_missing_env_does_not_panic() {
    let _gate = env_gate();
    let mut cfg = AgentConfig::from_value(&json!({}));
    cfg.api_key_env = "FUJIANG_LLM_KEY_DEFINITELY_UNSET_6".into();
    let http = reqwest::Client::builder().build().unwrap();
    let Err(err) = LlmClient::from_config(&cfg, http) else {
        panic!("expected missing key");
    };
    match err {
        LlmError::MissingApiKey { env } => {
            assert_eq!(env, "FUJIANG_LLM_KEY_DEFINITELY_UNSET_6");
        }
        other => panic!("{other}"),
    }
}

#[tokio::test]
async fn success_posts_responses_and_returns_output_text() {
    let rec = Rec::default();
    let app = Router::new()
        .route("/responses", post(record_responses))
        .with_state(rec.clone());
    let base = spawn_app(app).await;
    let client = {
        let _gate = env_gate();
        let name = "FUJIANG_LLM_KEY_FROM_CONFIG_TEST";
        // SAFETY: env_gate is held, and this block does not await.
        unsafe { std::env::set_var(name, "env-key-from-config") };
        let _clear = ClearEnv(name);
        let mut cfg = AgentConfig::from_value(&json!({"model_context_limit": 500_000}));
        cfg.base_url = format!("{base}/");
        cfg.api_key_env = name.into();
        let http = reqwest::Client::builder().build().unwrap();
        match LlmClient::from_config(&cfg, http) {
            Ok(client) => client,
            Err(err) => panic!("{err}"),
        }
    };
    let done = client
        .complete(&[
            ChatMessage {
                role: Role::System,
                content: "be brief".into(),
                images: Vec::new(),
            },
            ChatMessage {
                role: Role::System,
                content: String::new(),
                images: Vec::new(),
            },
            ChatMessage {
                role: Role::System,
                content: "stay kind".into(),
                images: Vec::new(),
            },
            ChatMessage {
                role: Role::User,
                content: "hi".into(),
                images: Vec::new(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "hello".into(),
                images: Vec::new(),
            },
        ])
        .await
        .unwrap();
    assert_eq!(done.text, "福酱在");
    assert_eq!(
        done.usage,
        TokenUse {
            input_tokens: 3,
            output_tokens: 2,
            input_known: true,
            output_known: true,
        }
    );
    let seen = rec.0.lock().unwrap().take().expect("request recorded");
    assert!(seen.path.ends_with("/responses"), "{}", seen.path);
    assert_eq!(seen.auth, "Bearer env-key-from-config");
    assert!(
        seen.content_type.starts_with("application/json"),
        "{}",
        seen.content_type
    );
    assert_eq!(seen.body["model"], "deepseek-flash");
    assert_eq!(seen.body["stream"], false);
    assert_eq!(seen.body["max_output_tokens"], 4096);
    assert_eq!(seen.body["instructions"], "be brief\nstay kind");
    assert_eq!(seen.body["input"].as_array().unwrap().len(), 2);
    assert_eq!(seen.body["input"][0]["role"], "user");
    assert_eq!(seen.body["input"][0]["content"], "hi");
    assert_eq!(seen.body["input"][1]["role"], "assistant");
    assert_eq!(seen.body["input"][1]["content"], "hello");
    assert!(seen.body.get("previous_response_id").is_none());
    assert!(seen.body.get("store").is_none());
    assert!(seen.body.get("tools").is_none());
    let temp = seen.body["temperature"].as_f64().unwrap();
    assert!((temp - 0.7).abs() < 1e-9, "{temp}");
}

#[test]
fn image_wire_is_built_but_unmetered_images_are_rejected() {
    let bytes = b"\xff\xd8\xff\x00";
    let part = crate::vision::image_part(bytes, 1024).expect("jpeg");
    let client = test_client("http://127.0.0.1:9", 60);
    let mut messages = vec![
        ChatMessage {
            role: Role::System,
            content: "be brief".into(),
            images: Vec::new(),
        },
        ChatMessage {
            role: Role::User,
            content: "之前".into(),
            images: Vec::new(),
        },
        ChatMessage {
            role: Role::Assistant,
            content: "嗯".into(),
            images: Vec::new(),
        },
        ChatMessage {
            role: Role::User,
            content: "看这张".into(),
            images: Vec::new(),
        },
    ];
    crate::vision::give_images(&mut messages, vec![part.clone()]);
    let body = rendered_request(&client, &messages, &[], &[]);
    assert_eq!(body["instructions"], "be brief");
    assert_eq!(body["input"][0]["content"], "之前");
    assert_eq!(body["input"][1]["content"], "嗯");
    let parts = body["input"][2]["content"].as_array().unwrap();
    assert_eq!(parts[0]["type"], "input_text");
    assert_eq!(parts[0]["text"], "看这张");
    assert_eq!(parts[1]["type"], "input_image");
    assert_eq!(parts[1]["image_url"], part.data_url());
    assert!(parts[1].get("detail").is_none());
    assert_eq!(parts.len(), 2);
    assert!(matches!(
        client.request_bound(&messages, &[], &[]).unwrap_err(),
        LlmError::ImageUnmetered
    ));
    messages.last_mut().unwrap().images[0] =
        ImagePart::new(format!("data:image/png;base64,{}", "a".repeat(4000)));
    assert!(matches!(
        client.request_bound(&messages, &[], &[]).unwrap_err(),
        LlmError::ImageUnmetered
    ));
    let capped = image_client(80_000, Some(1_024));
    let short = shot("data:image/png;base64,aa");
    let long = shot(&format!("data:image/png;base64,{}", "a".repeat(5_000)));
    let one = capped
        .request_bytes(std::slice::from_ref(&short), &[], &[])
        .unwrap();
    let big = capped
        .request_bytes(std::slice::from_ref(&long), &[], &[])
        .unwrap();
    assert_eq!(one, big);
    let mut both = short.clone();
    both.images.push(long.images[0].clone());
    let two = capped.request_bytes(&[both], &[], &[]).unwrap();
    assert!(two >= one + 1_024);
    assert!(two < one + 1_024 + 200);
    let mut many = short;
    for _ in 0..600 {
        many.images
            .push(ImagePart::new("data:image/png;base64,aa".into()));
    }
    assert!(matches!(
        capped.request_bound(&[many], &[], &[]).unwrap_err(),
        LlmError::PayloadLimited
    ));
    let tight = image_client(2_000, Some(1_024));
    assert!(matches!(
        tight.request_bound(&[long], &[], &[]).unwrap_err(),
        LlmError::OverBudget
    ));
}

#[test]
fn multi_image_wire_matches_escaped_json_and_the_body_boundary() {
    let urls = [
        "data:image/png;base64,aa",
        "data:image/png;base64,\"\\\u{2028}",
        "data:image/png;base64,a+b/c=",
    ];
    let message = ChatMessage {
        role: Role::User,
        content: "看\"图".into(),
        images: urls
            .iter()
            .map(|url| ImagePart::new((*url).into()))
            .collect(),
    };
    let client = image_client(80_000, Some(1_024));
    let projected = super::responses_request(
        &client.model,
        client.temperature,
        std::slice::from_ref(&message),
        &[],
        &[],
        crate::context_budget::OUTPUT_RESERVE,
        true,
    );
    let full = super::responses_request(
        &client.model,
        client.temperature,
        std::slice::from_ref(&message),
        &[],
        &[],
        crate::context_budget::OUTPUT_RESERVE,
        false,
    );
    let projected_len = serde_json::to_vec(&projected).unwrap().len() as u64;
    let full_len = serde_json::to_vec(&full).unwrap().len() as u64;
    let wire = super::physical_wire_len(projected_len, &urls).unwrap();
    assert_eq!(wire, full_len);
    let one = super::physical_wire_len(projected_len, &urls[..1]).unwrap();
    let two = super::physical_wire_len(projected_len, &urls[..2]).unwrap();
    let extra_two = super::json_string_extra(urls[1]).unwrap();
    assert_eq!(two, one + extra_two);
    assert!(two > one + urls[1].len() as u64);
    let max = crate::context_budget::MAX_REQUEST_BODY_BYTES;
    assert_eq!(super::physical_wire_len(max, &[]).unwrap(), max);
    assert!(super::physical_wire_len(max, &["\""]).unwrap() > max);
    let probe = shot("data:,x");
    let estimate = client
        .request_bytes(std::slice::from_ref(&probe), &[], &[])
        .unwrap();
    let projected = estimate - 1_024 - crate::context_budget::OUTPUT_RESERVE;
    let slack = max - projected;
    let quotes = ((slack.saturating_sub(7)) / 2) as usize;
    let under = format!("data:{},x", "\"".repeat(quotes));
    let over = format!("data:{},x", "\"".repeat(quotes + 1));
    assert!((under.len() as u64) <= max);
    assert!(super::json_string_extra(&under).unwrap() > under.len() as u64);
    assert!(super::physical_wire_len(projected, &[under.as_str()]).unwrap() <= max);
    assert!(super::physical_wire_len(projected, &[over.as_str()]).unwrap() > max);
    assert!(client
        .request_bytes(std::slice::from_ref(&shot(&under)), &[], &[])
        .is_ok());
    assert!(matches!(
        client
            .request_bytes(std::slice::from_ref(&shot(&over)), &[], &[])
            .unwrap_err(),
        LlmError::PayloadLimited
    ));
}

fn shot(url: &str) -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content: "正文里的 image_url 不是图片字段".into(),
        images: vec![ImagePart::new(url.into())],
    }
}

fn image_client(limit: u64, image_cap: Option<u64>) -> LlmClient {
    let mut cfg = AgentConfig::from_value(&json!({
        "model": "local-model",
        "context_budget": limit,
        "model_context_limit": limit
    }));
    cfg.base_url = "http://127.0.0.1:9".into();
    cfg.image_token_upper_bound = image_cap;
    LlmClient::new(&cfg, reqwest::Client::new(), "k".into())
}

#[test]
fn serialized_request_plus_output_reserve_is_the_upper_bound() {
    let client = sized_client(80_000);
    let bare = client.request_bound(&[user("hi")], &[], &[]).unwrap();
    assert!(bare >= crate::context_budget::OUTPUT_RESERVE + 2);
    assert_eq!(
        rendered_request(&client, &[user("hi")], &[], &[])["max_output_tokens"],
        4096
    );
    let quoted = client.request_bound(&[user("\"")], &[], &[]).unwrap();
    let plain = client.request_bound(&[user("a")], &[], &[]).unwrap();
    assert!(quoted > plain);
    let chinese = "你".repeat(4);
    let zh = client.request_bound(&[user(&chinese)], &[], &[]).unwrap();
    assert!(zh > u64::try_from(chinese.chars().count()).unwrap() / 4);
    assert!(zh >= u64::try_from(chinese.len()).unwrap());
    let params = json!({"type": "object", "properties": {"q": {"type": "string"}}});
    let tool = FunctionTool {
        name: "rank_list",
        description: "榜",
        parameters: &params,
    };
    let with_tool = client.request_bound(&[user("hi")], &[], &[tool]).unwrap();
    assert!(with_tool > bare);
    let prior = PriorCall {
        call_id: "c".into(),
        name: "rank_list".into(),
        arguments: "{}".into(),
        output: "x".repeat(1000),
    };
    let grown = client.request_bound(&[user("hi")], &[prior], &[]).unwrap();
    assert!(grown >= bare + 1000);
    let tiny = sized_client(1_000);
    assert!(matches!(
        tiny.request_bound(&[user("hi")], &[], &[]).unwrap_err(),
        LlmError::OverBudget
    ));
    let mut cfg = AgentConfig::from_value(&json!({"model": "local-model"}));
    cfg.base_url = "http://127.0.0.1:9".into();
    let closed = LlmClient::new(&cfg, reqwest::Client::new(), "k".into());
    assert!(matches!(
        closed.request_bytes(&[user("hi")], &[], &[]).unwrap_err(),
        LlmError::ContextClosed
    ));
}

#[tokio::test]
async fn timeout_when_handler_sleeps() {
    let app = Router::new().route("/responses", post(slow));
    let base = spawn_app(app).await;
    let client = test_client(&base, 1);
    let result = tokio::time::timeout(
        Duration::from_secs(4),
        client.complete(&[ChatMessage {
            role: Role::User,
            content: "hi".into(),
            images: Vec::new(),
        }]),
    )
    .await
    .expect("client hung past its timeout");
    let err = result.unwrap_err();
    assert!(matches!(err, LlmError::Timeout), "{err}");
}

#[tokio::test]
async fn error_status_uses_error_message() {
    let app = Router::new().route(
        "/responses",
        post(|| async {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {"message": "bad model", "type": "invalid_request_error"}
                })),
            )
        }),
    );
    let err = complete_err(app).await;
    match err {
        LlmError::Api { status, body } => {
            assert_eq!(status, 400);
            assert!(body.contains("bad model"), "{body}");
        }
        other => panic!("expected api 400, got {other}"),
    }
}

#[tokio::test]
async fn truncated_json_is_decode_error() {
    let app = Router::new().route(
        "/responses",
        post(|| async { (StatusCode::OK, r#"{"output_text":"hel""#) }),
    );
    let err = complete_err(app).await;
    assert!(matches!(err, LlmError::Decode(_)), "{err}");
}

#[tokio::test]
async fn empty_output_is_truncated() {
    let app = Router::new().route(
        "/responses",
        post(|| async { Json(json!({"output": [], "output_text": ""})) }),
    );
    let err = complete_err(app).await;
    assert!(matches!(err, LlmError::Truncated(_)), "{err}");
}

#[tokio::test]
async fn multi_segment_output_concatenates_message_text() {
    let app = Router::new().route(
        "/responses",
        post(|| async {
            Json(json!({
                "output": [
                    {
                        "type": "reasoning",
                        "summary": [{"type": "summary_text", "text": "think"}]
                    },
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [
                            {"type": "output_text", "text": "A"},
                            {"type": "output_text", "text": "B"}
                        ]
                    }
                ],
                "usage": {"input_tokens": 4, "output_tokens": 2, "total_tokens": 6}
            }))
        }),
    );
    let base = spawn_app(app).await;
    let client = test_client(&base, 60);
    let done = client
        .complete(&[ChatMessage {
            role: Role::User,
            content: "hi".into(),
            images: Vec::new(),
        }])
        .await
        .unwrap();
    assert_eq!(done.text, "AB");
    assert_eq!(
        done.usage,
        TokenUse {
            input_tokens: 4,
            output_tokens: 2,
            input_known: true,
            output_known: true,
        }
    );
}

#[tokio::test]
async fn bad_usage_fields_count_as_zero_tokens() {
    let app = Router::new().route(
        "/responses",
        post(|| async {
            Json(json!({
                "output_text": "好",
                "usage": {"input_tokens": -5, "output_tokens": "2", "total_tokens": 9}
            }))
        }),
    );
    let base = spawn_app(app).await;
    let client = test_client(&base, 60);
    let done = client
        .complete(&[ChatMessage {
            role: Role::User,
            content: "hi".into(),
            images: Vec::new(),
        }])
        .await
        .unwrap();
    assert_eq!(done.text, "好");
    assert!(!done.usage.known());
    assert!(!done.usage.input_known);
    assert!(!done.usage.output_known);
    assert_eq!(done.usage.input_tokens, 0);
    assert_eq!(done.usage.output_tokens, 0);
}

#[tokio::test]
async fn partial_usage_keeps_the_measured_field_only() {
    let app = Router::new().route(
        "/responses",
        post(|| async {
            Json(json!({
                "output_text": "好",
                "usage": {"input_tokens": 9}
            }))
        }),
    );
    let base = spawn_app(app).await;
    let client = test_client(&base, 60);
    let done = client.complete(&[user("hi")]).await.unwrap();
    assert!(done.usage.input_known);
    assert_eq!(done.usage.input_tokens, 9);
    assert!(!done.usage.output_known);
    assert_eq!(done.usage.output_tokens, 0);
    assert!(!done.usage.known());
}

#[tokio::test]
async fn missing_usage_is_unknown_and_a_reported_zero_is_measured() {
    let missing = Router::new().route(
        "/responses",
        post(|| async { Json(json!({"output_text": "好"})) }),
    );
    let base = spawn_app(missing).await;
    let client = test_client(&base, 60);
    let done = client.complete(&[user("hi")]).await.unwrap();
    assert!(!done.usage.input_known);
    assert!(!done.usage.output_known);
    assert!(!done.usage.known());
    assert_eq!(done.usage.input_tokens, 0);

    let measured = Router::new().route(
        "/responses",
        post(|| async {
            Json(json!({
                "output_text": "好",
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }))
        }),
    );
    let base = spawn_app(measured).await;
    let client = test_client(&base, 60);
    let done = client.complete(&[user("hi")]).await.unwrap();
    assert!(done.usage.known());
    assert!(done.usage.input_known);
    assert!(done.usage.output_known);
    assert_eq!(done.usage.input_tokens, 0);
    assert_eq!(done.usage.output_tokens, 0);
}

#[test]
fn output_reserve_is_sent_and_tool_schema_counts_in_the_projection() {
    let client = sized_client(500_000);
    let messages = [user("hi")];
    let bare = client.request_bytes(&messages, &[], &[]).unwrap();
    let parameters = json!({
        "type": "object",
        "properties": {
            "script": { "type": "string", "description": "workspace-schema-marker" }
        }
    });
    let tools = [FunctionTool {
        name: "rank_list",
        description: "列出榜",
        parameters: &parameters,
    }];
    let with_tools = client.request_bytes(&messages, &[], &tools).unwrap();
    assert!(with_tools > bare);
    let request = responses_request(
        "deepseek-flash",
        0.7,
        &messages,
        &[],
        &tools,
        crate::context_budget::OUTPUT_RESERVE,
        false,
    );
    let body = serde_json::to_value(&request).unwrap();
    assert_eq!(body["max_output_tokens"], 4096);
    assert_eq!(body["tools"][0]["name"], "rank_list");
    assert!(body["tools"][0]["parameters"]["properties"]["script"].is_object());
    let prior = [PriorCall {
        call_id: "c".into(),
        name: "rank_list".into(),
        arguments: "{\"handle\":\"x\"}".into(),
        output: "{\"ok\":true}".into(),
    }];
    let with_prior = client.request_bytes(&messages, &prior, &tools).unwrap();
    assert!(with_prior > with_tools);
}

async fn complete_err(app: Router) -> LlmError {
    let base = spawn_app(app).await;
    let client = test_client(&base, 60);
    client
        .complete(&[ChatMessage {
            role: Role::User,
            content: "hi".into(),
            images: Vec::new(),
        }])
        .await
        .unwrap_err()
}

fn env_gate() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

struct ClearEnv(&'static str);

impl Drop for ClearEnv {
    fn drop(&mut self) {
        // SAFETY: paired with set_var while env_gate is still held.
        unsafe { std::env::remove_var(self.0) }
    }
}

fn test_client(base: &str, timeout_secs: u64) -> LlmClient {
    let mut cfg = AgentConfig::from_value(&json!({"model_context_limit": 500_000}));
    cfg.base_url = base.to_string();
    cfg.timeout_secs = timeout_secs;
    let http = reqwest::Client::builder().build().unwrap();
    LlmClient::new(&cfg, http, "test-key".into())
}

fn sized_client(limit: u64) -> LlmClient {
    let mut cfg = AgentConfig::from_value(&json!({
        "model": "local-model",
        "context_budget": limit,
        "model_context_limit": limit
    }));
    cfg.base_url = "http://127.0.0.1:9".into();
    LlmClient::new(&cfg, reqwest::Client::new(), "k".into())
}

fn user(text: &str) -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content: text.to_string(),
        images: Vec::new(),
    }
}

async fn spawn_app(app: Router) -> String {
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

#[derive(Clone, Default)]
struct Rec(Arc<Mutex<Option<Seen>>>);

#[derive(Debug)]
struct Seen {
    path: String,
    auth: String,
    content_type: String,
    body: Value,
}

fn remember(rec: &Rec, uri: &axum::http::Uri, headers: &HeaderMap, body: Value) {
    let seen = Seen {
        path: uri.path().to_string(),
        auth: header(headers, axum::http::header::AUTHORIZATION),
        content_type: header(headers, axum::http::header::CONTENT_TYPE),
        body,
    };
    *rec.0.lock().unwrap() = Some(seen);
}

async fn record_responses(
    State(rec): State<Rec>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    remember(&rec, &uri, &headers, body);
    Json(json!({
        "output_text": "福酱在",
        "output": [{
            "type": "message",
            "content": [{"type": "output_text", "text": "NO"}]
        }],
        "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5}
    }))
}

fn header(headers: &HeaderMap, name: axum::http::HeaderName) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

async fn slow() {
    tokio::time::sleep(Duration::from_secs(5)).await;
}
