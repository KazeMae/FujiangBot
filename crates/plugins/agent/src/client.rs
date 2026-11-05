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

#[derive(Debug, Clone, Serialize)]
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

    pub async fn chat_completions(&self, messages: &[ChatMessage]) -> Result<String, LlmError> {
        let response = self
            .http
            .post(chat_url(&self.base_url))
            .timeout(self.timeout)
            .bearer_auth(&self.api_key)
            .json(&ChatRequest {
                model: &self.model,
                messages,
                temperature: self.temperature,
                stream: false,
            })
            .send()
            .await
            .map_err(map_reqwest)?;
        let status = response.status();
        let body = response.text().await.map_err(map_reqwest)?;
        if !status.is_success() {
            return Err(LlmError::Api {
                status: status.as_u16(),
                body: snippet(&body),
            });
        }
        parse_completion(&body)
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
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    temperature: f64,
    stream: bool,
}

fn require_key(env_name: &str, raw: Option<&str>) -> Result<String, LlmError> {
    match raw.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => Ok(key.to_string()),
        None => Err(LlmError::MissingApiKey {
            env: env_name.to_string(),
        }),
    }
}

fn chat_url(base: &str) -> String {
    format!("{}/chat/completions", base.trim().trim_end_matches('/'))
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

fn parse_completion(body: &str) -> Result<String, LlmError> {
    let value: Value =
        serde_json::from_str(body).map_err(|err| LlmError::Decode(err.to_string()))?;
    let Some(choices) = value.get("choices").and_then(Value::as_array) else {
        return Err(LlmError::Truncated("missing choices".into()));
    };
    let Some(content) = choices
        .first()
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
    else {
        return Err(LlmError::Truncated("missing message content".into()));
    };
    match content {
        Value::String(text) => Ok(text.clone()),
        Value::Null => Err(LlmError::Truncated("null content".into())),
        other => Err(LlmError::Decode(other.to_string())),
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
    async fn success_returns_assistant_content() {
        let rec = Rec::default();
        let app = Router::new()
            .route("/chat/completions", post(record_ok))
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
            .chat_completions(&[ChatMessage {
                role: Role::User,
                content: "hi".into(),
            }])
            .await
            .unwrap();
        assert_eq!(text, "福酱在");
        let seen = rec.0.lock().unwrap().take().expect("request recorded");
        assert_eq!(seen.auth, "Bearer env-key-from-config");
        assert!(
            seen.content_type.starts_with("application/json"),
            "{}",
            seen.content_type
        );
        assert_eq!(seen.body["model"], "deepseek-flash");
        assert_eq!(seen.body["stream"], false);
        let temp = seen.body["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < 1e-9, "{temp}");
        assert_eq!(seen.body["messages"][0]["role"], "user");
        assert_eq!(seen.body["messages"][0]["content"], "hi");
    }

    #[tokio::test]
    async fn timeout_when_handler_sleeps() {
        let app = Router::new().route("/chat/completions", post(slow));
        let base = spawn_app(app).await;
        let client = test_client(&base, 1);
        let result = tokio::time::timeout(
            Duration::from_secs(4),
            client.chat_completions(&[ChatMessage {
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
    async fn status_401_and_429_include_status_and_body() {
        expect_api(
            StatusCode::UNAUTHORIZED,
            "bad-key-body",
            401,
            "bad-key-body",
        )
        .await;
        expect_api(StatusCode::TOO_MANY_REQUESTS, "slow-down", 429, "slow-down").await;
    }

    #[tokio::test]
    async fn truncated_json_is_decode_error() {
        let app = Router::new().route(
            "/chat/completions",
            post(|| async { (StatusCode::OK, r#"{"choices":[{"message":{"content":"hel""#) }),
        );
        let err = chat_err(app).await;
        assert!(matches!(err, LlmError::Decode(_)), "{err}");
    }

    #[tokio::test]
    async fn missing_choices_and_null_content_are_truncated() {
        let empty = Router::new().route(
            "/chat/completions",
            post(|| async { Json(json!({"id": "x", "choices": []})) }),
        );
        let err = chat_err(empty).await;
        assert!(matches!(err, LlmError::Truncated(_)), "{err}");

        let null_body = Router::new().route(
            "/chat/completions",
            post(|| async {
                Json(json!({
                    "choices": [{"message": {"role": "assistant", "content": null}}]
                }))
            }),
        );
        let err = chat_err(null_body).await;
        assert!(matches!(err, LlmError::Truncated(_)), "{err}");
    }

    async fn expect_api(status: StatusCode, body: &'static str, want: u16, needle: &str) {
        let app = Router::new().route(
            "/chat/completions",
            post(move || async move { (status, body) }),
        );
        let err = chat_err(app).await;
        match err {
            LlmError::Api {
                status: got,
                body: text,
            } => {
                assert_eq!(got, want);
                assert!(text.contains(needle), "{text}");
            }
            other => panic!("expected api {want}, got {other}"),
        }
    }

    async fn chat_err(app: Router) -> LlmError {
        let base = spawn_app(app).await;
        let client = test_client(&base, 60);
        client
            .chat_completions(&[ChatMessage {
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
        auth: String,
        content_type: String,
        body: Value,
    }

    async fn record_ok(
        State(rec): State<Rec>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let seen = Seen {
            auth: header(&headers, axum::http::header::AUTHORIZATION),
            content_type: header(&headers, axum::http::header::CONTENT_TYPE),
            body,
        };
        *rec.0.lock().unwrap() = Some(seen);
        Json(json!({
            "choices": [{"message": {"role": "assistant", "content": "福酱在"}}]
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
