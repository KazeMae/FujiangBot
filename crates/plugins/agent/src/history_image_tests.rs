use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use fujiang_core::{Media, MessageEvent, Segment, Sender, Source};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::client::{ChatMessage, Role};
use crate::tools::{self, BuiltinTools, ToolPass, ToolReply};

fn jpeg() -> Vec<u8> {
    b"\xff\xd8\xff\x00".to_vec()
}

fn image(url: &str) -> Segment {
    Segment::Image {
        src: Media::Url(url.into()),
        summary: None,
    }
}

fn ev(id: i64, source: Source, segments: Vec<Segment>) -> MessageEvent {
    MessageEvent {
        id,
        time: 0,
        self_id: 9,
        sender: Sender {
            user_id: 3,
            nickname: None,
            card: None,
        },
        source,
        segments,
        raw_text: String::new(),
    }
}

struct Row {
    group_id: Option<i64>,
    peer: Option<i64>,
    message_id: i64,
    segments: Vec<Segment>,
}

struct Hub {
    rows: Vec<Row>,
    live: HashMap<i64, MessageEvent>,
    media: HashMap<String, Vec<u8>>,
    fetches: AtomicUsize,
    fetched: Mutex<Vec<String>>,
}

impl Hub {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            live: HashMap::new(),
            media: HashMap::new(),
            fetches: AtomicUsize::new(0),
            fetched: Mutex::new(Vec::new()),
        }
    }

    fn row(
        mut self,
        group_id: Option<i64>,
        peer: Option<i64>,
        message_id: i64,
        segments: Vec<Segment>,
    ) -> Self {
        self.rows.push(Row {
            group_id,
            peer,
            message_id,
            segments,
        });
        self
    }

    fn live(mut self, event: MessageEvent) -> Self {
        self.live.insert(event.id, event);
        self
    }

    fn media(mut self, url: &str, bytes: Vec<u8>) -> Self {
        self.media.insert(url.into(), bytes);
        self
    }
}

#[async_trait]
impl ToolHub for Hub {
    async fn invoke(&self, _: &str, _: &str, _: Value) -> anyhow::Result<Value> {
        anyhow::bail!("unavailable")
    }

    async fn session_message_segments(
        &self,
        group_id: Option<i64>,
        peer: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<Option<String>> {
        let found = self.rows.iter().find(|row| {
            row.message_id == message_id
                && match group_id {
                    Some(id) => row.group_id == Some(id),
                    None => peer.is_some() && row.peer == peer && row.group_id.is_none(),
                }
        });
        Ok(found.map(|row| serde_json::to_string(&row.segments).unwrap()))
    }

    async fn get_message(&self, id: i64) -> anyhow::Result<MessageEvent> {
        self.live
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing"))
    }

    async fn fetch_media_bytes(&self, media: &Media) -> anyhow::Result<Vec<u8>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        let Media::Url(url) = media else {
            anyhow::bail!("unsupported");
        };
        self.fetched.lock().unwrap().push(url.clone());
        self.media
            .get(url)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("expired"))
    }
}

fn scope(source: Source) -> MemoCallScope {
    MemoCallScope::from_source(source, 3)
}

#[test]
fn placeholders_keep_trigger_and_quoted_images_as_text() {
    let own = vec![
        Segment::text("看"),
        image("a"),
        Segment::text("和"),
        image("b"),
    ];
    let quoted = vec![image("c"), Segment::text("d"), image("e")];
    let text = with_placeholders("看这个", 9, &own, Some((8, &quoted)));
    assert_eq!(
        text,
        "看这个 [图片#9:1] [图片#9:2] [引用图片#8:1] [引用图片#8:2]"
    );
    assert_eq!(
        with_placeholders("", 9, &own, None),
        "[图片#9:1] [图片#9:2]"
    );
    assert_eq!(
        with_placeholders("只有字", 9, &[Segment::text("z")], None),
        "只有字"
    );
}

#[test]
fn quoted_segments_reject_another_session() {
    let current = Source::Group { id: 7 };
    let other = ev(8, Source::Group { id: 9 }, vec![image("a")]);
    let friend = ev(8, Source::Friend { id: 4 }, vec![image("a")]);
    let same = ev(8, current, vec![image("a")]);
    assert!(quoted_segments(current, None, Some(&other)).is_none());
    assert!(quoted_segments(current, None, Some(&friend)).is_none());
    assert!(quoted_segments(Source::Friend { id: 4 }, None, Some(&other)).is_none());
    assert_eq!(
        quoted_segments(current, None, Some(&same)).unwrap().len(),
        1
    );
    assert!(quoted_segments(current, Some("[]"), Some(&other))
        .unwrap()
        .is_empty());
    assert!(quoted_segments(current, Some("nope"), None).is_none());
    let stored = serde_json::to_string(&vec![image("a")]).unwrap();
    let borrowed = quoted_segments(current, Some(&stored), Some(&other)).unwrap();
    let text = with_placeholders("看", 1, &[], Some((8, borrowed.as_slice())));
    assert_eq!(text, "看 [引用图片#8:1]");
}

#[tokio::test]
async fn quoted_message_in_another_session_yields_no_placeholders() {
    let hub = Hub::new().row(Some(9), None, 8, vec![image("a")]).live(ev(
        8,
        Source::Group { id: 9 },
        vec![image("a")],
    ));
    let here = scope(Source::Group { id: 7 });
    assert!(segments_in_session(&hub, &here, 8).await.is_none());
    assert_eq!(hub.fetches.load(Ordering::SeqCst), 0);
    let own = vec![image("b")];
    let text = with_placeholders("看", 3, &own, None);
    assert_eq!(text, "看 [图片#3:1]");
    assert!(!text.contains("引用图片"));
}

#[test]
fn view_image_is_absent_unless_vision_and_a_metered_cap() {
    let off = AgentConfig::from_value(&json!({"vision_enabled": false}));
    assert!(!offered(&off));
    assert!(!crate::command::enabled_builtins(&off).view_image);
    let official = AgentConfig::from_value(&json!({"vision_enabled": true}));
    assert!(offered(&official));
    assert!(crate::command::enabled_builtins(&official).view_image);
    let mut custom = AgentConfig::from_value(&json!({
        "vision_enabled": true,
        "base_url": "https://example.test/v1",
        "model": "other"
    }));
    assert!(!offered(&custom));
    assert!(!crate::command::enabled_builtins(&custom).view_image);
    custom.image_token_upper_bound = Some(10);
    assert!(offered(&custom));
    let shown = tools::resident_tools(
        &[],
        BuiltinTools {
            view_image: true,
            ..BuiltinTools::OFF
        },
    );
    assert!(shown.iter().any(|spec| spec.wire_name() == WIRE));
    assert!(shown.iter().any(|spec| spec.name() == NAME));
    let hidden = tools::resident_tools(&[], BuiltinTools::OFF);
    assert!(hidden.iter().all(|spec| spec.wire_name() != WIRE));
    let catalog = tools::instructions_with_catalog("人设", &shown, true);
    assert!(catalog.contains(NAME));
    assert!(catalog.contains("每次最多 3 张"));
    assert!(!tools::instructions_with_catalog("人设", &hidden, true).contains(NAME));
    let params = parameters();
    assert_eq!(params["properties"]["ids"]["maxItems"], 3);
}

#[derive(Clone)]
struct Script {
    hits: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Value>>>,
    steps: Arc<Vec<Value>>,
}

async fn spawn(steps: Vec<Value>) -> (String, Script) {
    let script = Script {
        hits: Arc::new(AtomicUsize::new(0)),
        bodies: Arc::new(Mutex::new(Vec::new())),
        steps: Arc::new(steps),
    };
    let app = Router::new()
        .route("/responses", post(handle))
        .with_state(script.clone_bits());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shared = script.clone_bits();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for _ in 0..50 {
        if TcpStream::connect(addr).await.is_ok() {
            return (format!("http://{addr}"), shared);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("mock llm did not accept");
}

impl Script {
    fn clone_bits(&self) -> Self {
        Self {
            hits: self.hits.clone(),
            bodies: self.bodies.clone(),
            steps: self.steps.clone(),
        }
    }
}

async fn handle(State(script): State<Script>, Json(body): Json<Value>) -> Json<Value> {
    let n = script.hits.fetch_add(1, Ordering::SeqCst);
    script.bodies.lock().unwrap().push(body);
    Json(script.steps.get(n).cloned().unwrap_or_else(done))
}

fn done() -> Value {
    json!({
        "output_text": "好",
        "output": [{"type": "message", "content": [{"type": "output_text", "text": "好"}]}]
    })
}

fn call(n: usize, ids: Value) -> Value {
    json!({
        "output": [{
            "type": "function_call",
            "call_id": format!("c{n}"),
            "name": WIRE,
            "arguments": serde_json::to_string(&json!({"ids": ids})).unwrap()
        }]
    })
}

fn client(base: &str, limit: u64, image_cap: u64, max_image: u64) -> LlmClient {
    let mut cfg = AgentConfig::from_value(&json!({
        "model_context_limit": limit,
        "image_token_upper_bound": image_cap,
        "max_image_bytes": max_image,
        "base_url": base,
        "model": "local-model"
    }));
    cfg.base_url = base.into();
    cfg.timeout_secs = 60;
    LlmClient::new(&cfg, reqwest::Client::new(), "k".into())
}

fn messages() -> Vec<ChatMessage> {
    vec![ChatMessage {
        role: Role::User,
        content: "看图".into(),
        images: Vec::new(),
    }]
}

async fn run(
    hub: &Hub,
    who: &MemoCallScope,
    steps: Vec<Value>,
    limit: u64,
    image_cap: u64,
    max_image: u64,
) -> (Vec<Value>, ToolReply) {
    let (base, script) = spawn(steps).await;
    let llm = client(&base, limit, image_cap, max_image);
    let pass = ToolPass {
        max_rounds: 8,
        scope: Some(who),
        memory: None,
        memory_unused_secs: 1,
        memory_max_entries: 1,
    };
    let reply = tools::run_tool_loop(
        &llm,
        &messages(),
        &[],
        BuiltinTools {
            view_image: true,
            ..BuiltinTools::OFF
        },
        &pass,
        hub,
    )
    .await
    .reply
    .unwrap();
    let bodies = script.bodies.lock().unwrap().clone();
    (bodies, reply)
}

fn outputs(body: &Value) -> Vec<String> {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| item["output"].as_str().unwrap_or("").to_string())
        .collect()
}

fn image_urls(body: &Value) -> Vec<String> {
    let mut urls = Vec::new();
    let Some(input) = body["input"].as_array() else {
        return urls;
    };
    for item in input {
        let Some(parts) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            if part["type"] == "input_image" {
                urls.push(part["image_url"].as_str().unwrap_or("").to_string());
            }
        }
    }
    urls
}

#[tokio::test]
async fn same_group_placeholder_is_returned_as_an_input_image() {
    let url = "https://example.test/a.jpg";
    let hub = Hub::new()
        .row(Some(7), None, 42, vec![Segment::text("字"), image(url)])
        .media(url, jpeg());
    let (bodies, reply) = run(
        &hub,
        &scope(Source::Group { id: 7 }),
        vec![call(0, json!(["42:1", "[图片#42:1]", "[引用图片#42:1]"]))],
        500_000,
        1024,
        5 * 1024 * 1024,
    )
    .await;
    assert_eq!(reply, ToolReply::Text("好".into()));
    assert!(bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == WIRE));
    let urls = image_urls(&bodies[1]);
    assert_eq!(urls.len(), 3);
    assert!(urls
        .iter()
        .all(|url| url.starts_with("data:image/jpeg;base64,")));
    let note = bodies[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["role"] == "user" && item["content"].is_array())
        .unwrap();
    assert_eq!(
        note["content"][0]["text"],
        "view_image 返回的图片：42:1 42:1 42:1"
    );
    assert_eq!(outputs(&bodies[1])[0], "已返回：42:1 42:1 42:1");
    assert_eq!(hub.fetches.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn cross_session_ids_are_refused_without_a_fetch() {
    let url = "https://example.test/a.jpg";
    let hub = Hub::new()
        .row(Some(8), None, 200, vec![image(url)])
        .row(None, Some(9), 300, vec![image(url)])
        .row(Some(7), None, 400, vec![image(url)])
        .live(ev(200, Source::Group { id: 8 }, vec![image(url)]))
        .live(ev(300, Source::Friend { id: 9 }, vec![image(url)]))
        .live(ev(400, Source::Group { id: 7 }, vec![image(url)]))
        .live(ev(500, Source::Group { id: 8 }, vec![image(url)]))
        .media(url, jpeg());
    let (bodies, _) = run(
        &hub,
        &scope(Source::Group { id: 7 }),
        vec![
            call(0, json!(["200:1"])),
            call(1, json!(["500:1"])),
            call(2, json!(["nope"])),
            call(3, json!(["1:1", "2:1", "3:1", "4:1"])),
        ],
        500_000,
        1024,
        5 * 1024 * 1024,
    )
    .await;
    let last = outputs(bodies.last().unwrap());
    assert!(last
        .iter()
        .any(|line| line.contains("不在当前会话或已过期")));
    assert!(last.iter().any(|line| line == "编号格式不对"));
    assert!(last.iter().any(|line| line == "一次最多 3 张"));
    assert!(image_urls(bodies.last().unwrap()).is_empty());
    assert_eq!(hub.fetches.load(Ordering::SeqCst), 0);

    let (bodies, _) = run(
        &hub,
        &scope(Source::Friend { id: 2 }),
        vec![call(0, json!(["300:1"])), call(1, json!(["400:1"]))],
        500_000,
        1024,
        5 * 1024 * 1024,
    )
    .await;
    let last = outputs(bodies.last().unwrap());
    assert!(last
        .iter()
        .all(|line| line.contains("不在当前会话或已过期")));
    assert!(image_urls(bodies.last().unwrap()).is_empty());
    assert_eq!(hub.fetches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_expired_index_and_turn_cap_are_readable() {
    let url = "https://example.test/a.jpg";
    let hub = Hub::new()
        .row(Some(7), None, 42, vec![image(url)])
        .media(url, jpeg());
    let (bodies, _) = run(
        &hub,
        &scope(Source::Group { id: 7 }),
        vec![
            call(0, json!(["42:2"])),
            call(1, json!(["77:1"])),
            call(2, json!(["42:1", "42:1", "42:1"])),
            call(3, json!(["42:1", "42:1", "42:1"])),
            call(4, json!(["42:1"])),
        ],
        500_000,
        1024,
        5 * 1024 * 1024,
    )
    .await;
    let last = outputs(bodies.last().unwrap());
    assert!(last.iter().any(|line| line == "这条消息没有第 2 张图"));
    assert!(last
        .iter()
        .any(|line| line.contains("不在当前会话或已过期")));
    assert!(last.iter().any(|line| line == "本轮看图已达上限"));
    let sent = image_urls(bodies.last().unwrap());
    assert_eq!(sent.len(), 6);
    assert!(sent
        .iter()
        .all(|url| url.starts_with("data:image/jpeg;base64,")));
}

#[tokio::test]
async fn refresh_uses_a_matching_message_and_reports_expiry() {
    let old = "https://example.test/old.jpg";
    let fresh = "https://example.test/new.jpg";
    let foreign = "https://example.test/no.jpg";
    let hub = Hub::new()
        .row(Some(7), None, 42, vec![image(old)])
        .row(Some(7), None, 43, vec![image(old)])
        .live(ev(42, Source::Group { id: 7 }, vec![image(fresh)]))
        .live(ev(43, Source::Group { id: 8 }, vec![image(foreign)]))
        .media(fresh, jpeg());
    let (bodies, _) = run(
        &hub,
        &scope(Source::Group { id: 7 }),
        vec![call(0, json!(["42:1"])), call(1, json!(["43:1"]))],
        500_000,
        1024,
        5 * 1024 * 1024,
    )
    .await;
    let urls = image_urls(bodies.last().unwrap());
    assert_eq!(urls.len(), 1);
    assert!(urls[0].starts_with("data:image/jpeg;base64,"));
    let last = outputs(bodies.last().unwrap());
    assert!(last
        .iter()
        .any(|line| line.contains("图片取不到（可能已过期）")));
    let fetched = hub.fetched.lock().unwrap().clone();
    assert!(fetched.contains(&old.to_string()));
    assert!(fetched.contains(&fresh.to_string()));
    assert!(!fetched.contains(&foreign.to_string()));
}

#[tokio::test]
async fn size_and_format_tips_replace_the_image() {
    let big = "https://example.test/big.jpg";
    let odd = "https://example.test/odd.jpg";
    let hub = Hub::new()
        .row(Some(7), None, 1, vec![image(big)])
        .row(Some(7), None, 2, vec![image(odd)])
        .media(big, b"\xff\xd8\xff\x00\x00".to_vec())
        .media(odd, b"nope".to_vec());
    let (bodies, _) = run(
        &hub,
        &scope(Source::Group { id: 7 }),
        vec![call(0, json!(["1:1"])), call(1, json!(["2:1"]))],
        500_000,
        1024,
        4,
    )
    .await;
    let last = outputs(bodies.last().unwrap());
    assert!(last.iter().any(|line| line.contains("太大")));
    assert!(last.iter().any(|line| line.contains("格式")));
    assert!(image_urls(bodies.last().unwrap()).is_empty());
}

#[tokio::test]
async fn over_budget_attach_drops_the_image() {
    let url = "https://example.test/a.jpg";
    let hub = Hub::new()
        .row(Some(7), None, 42, vec![image(url)])
        .media(url, jpeg());
    let (bodies, reply) = run(
        &hub,
        &scope(Source::Group { id: 7 }),
        vec![call(0, json!(["42:1"]))],
        50_000,
        100_000,
        5 * 1024 * 1024,
    )
    .await;
    assert_eq!(reply, ToolReply::Text("好".into()));
    assert_eq!(outputs(&bodies[1]), vec!["超出上下文预算".to_string()]);
    assert!(image_urls(&bodies[1]).is_empty());
}

#[tokio::test]
async fn hidden_tool_is_missing_from_the_request_defs() {
    let (base, script) = spawn(vec![done()]).await;
    let llm = client(&base, 500_000, 1024, 1024);
    let who = scope(Source::Group { id: 7 });
    let pass = ToolPass {
        max_rounds: 1,
        scope: Some(&who),
        memory: None,
        memory_unused_secs: 1,
        memory_max_entries: 1,
    };
    let hub = Hub::new();
    tools::run_tool_loop(&llm, &messages(), &[], BuiltinTools::OFF, &pass, &hub)
        .await
        .reply
        .unwrap();
    let tools = &script.bodies.lock().unwrap()[0]["tools"];
    assert!(tools
        .as_array()
        .unwrap()
        .iter()
        .all(|tool| tool["name"] != WIRE));
}
