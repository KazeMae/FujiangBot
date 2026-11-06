//! Responses function tools call `ctx.call(service, method, args)`.
//!
//! A tool name is `service.method` (split on the first `.`). Responses function
//! names match `^[a-zA-Z0-9_-]+$`, so the request sends `.` as `_`.

use async_trait::async_trait;
use fujiang_core::BotContext;
use serde_json::Value;
use tracing::warn;

use crate::client::{ChatMessage, FunctionTool, LlmClient, LlmError, ModelOutput, PriorCall};

pub(crate) const TOOL_CAP_REPLY: &str = "要查的步骤太多了，我先停在这里。";
pub(crate) const TOOL_FAIL_REPLY: &str = "我没查成，过一会儿再问我一次。";

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    name: String,
    wire_name: String,
    service: String,
    method: String,
    description: String,
    parameters: Value,
}

impl ToolSpec {
    /// `name` is `service.method`. The first `.` splits the ServiceHub call.
    pub fn parse(name: &str, description: &str, parameters: Value) -> Option<Self> {
        let name = name.trim();
        let (service, method) = split_tool_name(name)?;
        let wire_name = name.replace('.', "_");
        if !wire_name_ok(&wire_name) {
            return None;
        }
        let parameters = if parameters.is_object() {
            parameters
        } else {
            default_parameters()
        };
        Some(Self {
            name: name.to_string(),
            wire_name,
            service: service.to_string(),
            method: method.to_string(),
            description: description.trim().to_string(),
            parameters,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn wire_name(&self) -> &str {
        &self.wire_name
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn parameters(&self) -> &Value {
        &self.parameters
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolReply {
    Text(String),
    Degraded(&'static str),
}

#[async_trait]
pub(crate) trait ToolHub: Send + Sync {
    async fn invoke(&self, service: &str, method: &str, args: Value) -> anyhow::Result<Value>;
}

#[async_trait]
impl ToolHub for BotContext {
    async fn invoke(&self, service: &str, method: &str, args: Value) -> anyhow::Result<Value> {
        self.call(service, method, args).await
    }
}

/// Config tools plus builtin specs. Issue #11 fills [`builtin_specs`].
pub(crate) fn assemble_tools(configured: &[ToolSpec]) -> Vec<ToolSpec> {
    let mut tools = builtin_specs();
    for spec in configured {
        if tools.iter().any(|kept| kept.wire_name == spec.wire_name) {
            continue;
        }
        tools.push(spec.clone());
    }
    tools
}

fn builtin_specs() -> Vec<ToolSpec> {
    Vec::new()
}

pub(crate) async fn run_tool_loop<H: ToolHub + ?Sized>(
    client: &LlmClient,
    messages: &[ChatMessage],
    configured: &[ToolSpec],
    max_rounds: u32,
    hub: &H,
) -> Result<ToolReply, LlmError> {
    let tools = assemble_tools(configured);
    if tools.is_empty() {
        return Ok(ToolReply::Text(client.complete(messages).await?));
    }
    let defs: Vec<FunctionTool<'_>> = tools
        .iter()
        .map(|spec| FunctionTool {
            name: spec.wire_name(),
            description: spec.description(),
            parameters: spec.parameters(),
        })
        .collect();
    let mut prior = Vec::new();
    let mut used = 0u32;
    loop {
        match client.complete_with_tools(messages, &prior, &defs).await? {
            ModelOutput::Text(text) => return Ok(ToolReply::Text(text)),
            ModelOutput::Calls(calls) => {
                if used >= max_rounds {
                    warn!(max_tool_rounds = max_rounds, "agent tool round cap");
                    return Ok(ToolReply::Degraded(TOOL_CAP_REPLY));
                }
                for call in calls {
                    let Some(spec) = tools.iter().find(|spec| spec.wire_name == call.name) else {
                        warn!(tool = %call.name, "agent tool unknown");
                        return Ok(ToolReply::Degraded(TOOL_FAIL_REPLY));
                    };
                    let args = match parse_arguments(&call.arguments) {
                        Ok(value) => value,
                        Err(err) => {
                            warn!(error = %err, tool = spec.name(), "agent tool arguments");
                            return Ok(ToolReply::Degraded(TOOL_FAIL_REPLY));
                        }
                    };
                    let value = match hub.invoke(spec.service(), spec.method(), args).await {
                        Ok(value) => value,
                        Err(err) => {
                            warn!(
                                error = %err,
                                tool = spec.name(),
                                service = spec.service(),
                                method = spec.method(),
                                "agent tool"
                            );
                            return Ok(ToolReply::Degraded(TOOL_FAIL_REPLY));
                        }
                    };
                    let output = serde_json::to_string(&value).unwrap_or_else(|_| "null".into());
                    prior.push(PriorCall {
                        call_id: call.call_id,
                        name: spec.wire_name.clone(),
                        arguments: call.arguments,
                        output,
                    });
                }
                used = used.saturating_add(1);
            }
        }
    }
}

fn split_tool_name(name: &str) -> Option<(&str, &str)> {
    let (service, method) = name.split_once('.')?;
    if service.is_empty() || method.is_empty() {
        None
    } else {
        Some((service, method))
    }
}

fn wire_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn default_parameters() -> Value {
    serde_json::json!({"type": "object", "properties": {}})
}

fn parse_arguments(raw: &str) -> Result<Value, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(trimmed).map_err(|_| "arguments are not json")
}

#[cfg(test)]
mod tests {
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
        assert!(assemble_tools(&[]).is_empty());

        let first = ToolSpec::parse("a.b_c", "one", json!({})).unwrap();
        let second = ToolSpec::parse("a_b.c", "two", json!({})).unwrap();
        assert_eq!(first.wire_name(), "a_b_c");
        let kept = assemble_tools(&[first, second]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name(), "a.b_c");
        assert_eq!(kept[0].service(), "a");
        assert_eq!(kept[0].method(), "b_c");
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
            },
            ChatMessage {
                role: Role::User,
                content: "最近有比赛吗".into(),
            },
        ];
        let reply = run_tool_loop(&client, &messages, &tools, 3, &hub)
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
        let reply = run_tool_loop(&client, &user("再查"), &tools, 1, &hub)
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
        let reply = run_tool_loop(&client, &user("再查"), &tools, 0, &hub)
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
        let reply = run_tool_loop(&client, &user("榜"), &tools, 3, &hub)
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
        let reply = run_tool_loop(&client, &user("榜"), &tools, 3, &hub)
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
        let reply = run_tool_loop(&client, &user("榜"), &tools, 3, &hub)
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
        let reply = run_tool_loop(&client, &user("在吗"), &[], 3, &hub)
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
        let err = run_tool_loop(&client, &user("榜"), &tools, 3, &hub)
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Decode(_)), "{err}");
        assert!(hub.log.lock().unwrap().is_empty());
    }

    fn user(text: &str) -> Vec<ChatMessage> {
        vec![ChatMessage {
            role: Role::User,
            content: text.into(),
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
        json!({
            "output_text": "本周有两场",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "NO"}]
            }]
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
}
