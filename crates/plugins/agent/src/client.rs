use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::config::AgentConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("missing API key in environment variable {env}")]
    MissingApiKey { env: String },
    #[error("http: {0}")]
    Http(reqwest::Error),
    #[error("LLM API status {status}: {body}")]
    Api { status: u16, body: String },
    #[error("LLM request timed out")]
    Timeout,
    #[error("failed to decode LLM response: {0}")]
    Decode(String),
    #[error("truncated or incomplete LLM response: {0}")]
    Truncated(String),
}

pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    temperature: f64,
    timeout: Duration,
}

impl LlmClient {
    /// Reads the key from the env var named by `cfg.api_key_env`. Never panics.
    pub fn from_config(cfg: &AgentConfig, http: reqwest::Client) -> Result<Self, LlmError> {
        let raw = std::env::var(&cfg.api_key_env).ok();
        let api_key = require_key(&cfg.api_key_env, raw.as_deref())?;
        Ok(Self::new(cfg, http, api_key))
    }

    pub async fn complete(&self, messages: &[ChatMessage]) -> Result<String, LlmError> {
        let request = responses_request(&self.model, self.temperature, messages);
        let (status, body) = self.send_json("responses", &request).await?;
        if !status.is_success() {
            return Err(LlmError::Api {
                status: status.as_u16(),
                body: error_message_or_snippet(&body),
            });
        }
        parse_responses(&body)
    }

    fn new(cfg: &AgentConfig, http: reqwest::Client, api_key: String) -> Self {
        Self {
            http,
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
            api_key,
            temperature: cfg.temperature,
            timeout: Duration::from_secs(cfg.timeout_secs),
        }
    }

    async fn send_json(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<(reqwest::StatusCode, String), LlmError> {
        let response = self
            .http
            .post(endpoint(&self.base_url, path))
            .timeout(self.timeout)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(map_reqwest)?;
        let status = response.status();
        let text = response.text().await.map_err(map_reqwest)?;
        Ok((status, text))
    }
}

fn require_key(env_name: &str, raw: Option<&str>) -> Result<String, LlmError> {
    match raw.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => Ok(key.to_string()),
        None => Err(LlmError::MissingApiKey {
            env: env_name.to_string(),
        }),
    }
}

fn endpoint(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim().trim_end_matches('/'), path)
}

#[derive(Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    input: Vec<ResponsesMessage<'a>>,
    temperature: f64,
    stream: bool,
}

#[derive(Serialize)]
struct ResponsesMessage<'a> {
    role: &'a str,
    content: &'a str,
}

fn responses_request<'a>(
    model: &'a str,
    temperature: f64,
    messages: &'a [ChatMessage],
) -> ResponsesRequest<'a> {
    let mut instructions = String::new();
    let mut input = Vec::new();
    for message in messages {
        match message.role {
            Role::System => {
                if message.content.is_empty() {
                    continue;
                }
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                instructions.push_str(&message.content);
            }
            Role::User => input.push(ResponsesMessage {
                role: "user",
                content: &message.content,
            }),
            Role::Assistant => input.push(ResponsesMessage {
                role: "assistant",
                content: &message.content,
            }),
        }
    }
    ResponsesRequest {
        model,
        instructions: if instructions.is_empty() {
            None
        } else {
            Some(instructions)
        },
        input,
        temperature,
        stream: false,
    }
}

fn map_reqwest(err: reqwest::Error) -> LlmError {
    if err.is_timeout() {
        LlmError::Timeout
    } else {
        LlmError::Http(err)
    }
}

fn snippet(body: &str) -> String {
    const MAX: usize = 400;
    let body = body.trim();
    if body.chars().count() <= MAX {
        return body.to_string();
    }
    let mut out: String = body.chars().take(MAX).collect();
    out.push_str("...");
    out
}

fn error_message_or_snippet(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return snippet(body);
    };
    let message = value
        .get("error")
        .and_then(|err| err.get("message"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    match message {
        Some(text) => snippet(text),
        None => snippet(body),
    }
}

fn parse_responses(body: &str) -> Result<String, LlmError> {
    let value: Value =
        serde_json::from_str(body).map_err(|err| LlmError::Decode(err.to_string()))?;
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        if !text.is_empty() {
            return Ok(text.to_string());
        }
    }
    let Some(output) = value.get("output").and_then(Value::as_array) else {
        return Err(LlmError::Truncated("missing output".into()));
    };
    let mut text = String::new();
    for item in output {
        if !is_message_item(item) {
            continue;
        }
        text.push_str(&message_text(item));
    }
    if text.is_empty() {
        Err(LlmError::Truncated("empty output".into()))
    } else {
        Ok(text)
    }
}

fn is_message_item(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => true,
        Some(_) => false,
        None => item.get("role").and_then(Value::as_str) == Some("assistant"),
    }
}

fn message_text(item: &Value) -> String {
    let mut out = String::new();
    match item.get("content") {
        Some(Value::String(text)) => out.push_str(text),
        Some(Value::Array(parts)) => {
            for part in parts {
                if let Some(text) = output_text_part(part) {
                    out.push_str(text);
                }
            }
        }
        _ => {}
    }
    out
}

fn output_text_part(part: &Value) -> Option<&str> {
    match part {
        Value::String(text) => Some(text),
        other => {
            if other.get("type").and_then(Value::as_str) != Some("output_text") {
                return None;
            }
            other.get("text").and_then(Value::as_str)
        }
    }
}

#[cfg(test)]
mod tests {
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
            let mut cfg = AgentConfig::from_value(&json!({}));
            cfg.base_url = format!("{base}/");
            cfg.api_key_env = name.into();
            let http = reqwest::Client::builder().build().unwrap();
            match LlmClient::from_config(&cfg, http) {
                Ok(client) => client,
                Err(err) => panic!("{err}"),
            }
        };
        let text = client
            .complete(&[
                ChatMessage {
                    role: Role::System,
                    content: "be brief".into(),
                },
                ChatMessage {
                    role: Role::System,
                    content: String::new(),
                },
                ChatMessage {
                    role: Role::System,
                    content: "stay kind".into(),
                },
                ChatMessage {
                    role: Role::User,
                    content: "hi".into(),
                },
                ChatMessage {
                    role: Role::Assistant,
                    content: "hello".into(),
                },
            ])
            .await
            .unwrap();
        assert_eq!(text, "福酱在");
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
        assert_eq!(seen.body["instructions"], "be brief\nstay kind");
        assert_eq!(seen.body["input"].as_array().unwrap().len(), 2);
        assert_eq!(seen.body["input"][0]["role"], "user");
        assert_eq!(seen.body["input"][0]["content"], "hi");
        assert_eq!(seen.body["input"][1]["role"], "assistant");
        assert_eq!(seen.body["input"][1]["content"], "hello");
        assert!(seen.body.get("previous_response_id").is_none());
        assert!(seen.body.get("store").is_none());
        let temp = seen.body["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < 1e-9, "{temp}");
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
        let text = client
            .complete(&[ChatMessage {
                role: Role::User,
                content: "hi".into(),
            }])
            .await
            .unwrap();
        assert_eq!(text, "AB");
    }

    async fn complete_err(app: Router) -> LlmError {
        let base = spawn_app(app).await;
        let client = test_client(&base, 60);
        client
            .complete(&[ChatMessage {
                role: Role::User,
                content: "hi".into(),
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
        let mut cfg = AgentConfig::from_value(&json!({}));
        cfg.base_url = base.to_string();
        cfg.timeout_secs = timeout_secs;
        let http = reqwest::Client::builder().build().unwrap();
        LlmClient::new(&cfg, http, "test-key".into())
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
}
