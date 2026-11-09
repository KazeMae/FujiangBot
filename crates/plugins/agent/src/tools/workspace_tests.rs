use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use fujiang_core::Source;
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};

use super::skills::{self, round_tools_gated};
use super::{
    run_tool_loop, tool_loop_with, BuiltinTools, ChatMessage, LlmClient, MemoCallScope, ToolHub,
    ToolPass, ToolReply, ToolSpec, TOOL_FAIL_REPLY,
};
use crate::client::Role;
use crate::config::AgentConfig;
use crate::session;
use crate::workspace::{command, WorkspaceCmd, RUNNER_UNAVAILABLE};
use crate::workspace_backend::{self, PublicRuntime, RootfsAuthority};
use crate::workspace_runner::{Payload, RunResult};
use crate::workspace_store::{Owner, WorkspaceStore};

const IDENTITY: [u8; 32] = [9u8; 32];

struct Hub;

#[async_trait]
impl ToolHub for Hub {
    async fn invoke(&self, _: &str, _: &str, _: Value) -> anyhow::Result<Value> {
        anyhow::bail!("workspace fixture does not call services")
    }
}

struct FixtureAuth {
    host: PathBuf,
    image: PathBuf,
}

impl RootfsAuthority for FixtureAuth {
    fn acquire(&self) -> anyhow::Result<(PathBuf, PublicRuntime)> {
        Ok((
            self.host.clone(),
            PublicRuntime::fixture(&self.image, IDENTITY)?,
        ))
    }
}

struct Tmp(PathBuf);

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> (Tmp, FixtureAuth, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "ws-tool-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let host = root.join("host");
    let image = root.join("image");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::create_dir_all(&image).unwrap();
    let auth = FixtureAuth {
        host: host.clone(),
        image: image.clone(),
    };
    (Tmp(root), auth, host)
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

fn scope(source: Source, actor: i64) -> MemoCallScope {
    MemoCallScope::from_source(source, actor)
}

fn pass<'a>(
    max_rounds: u32,
    scope: Option<&'a MemoCallScope>,
    memory: Option<&'a sqlx::SqlitePool>,
) -> ToolPass<'a> {
    ToolPass {
        max_rounds,
        scope,
        memory,
        memory_unused_secs: 1,
        memory_max_entries: 8,
    }
}

fn user(text: &str) -> Vec<ChatMessage> {
    vec![ChatMessage {
        role: Role::User,
        content: text.into(),
        images: Vec::new(),
    }]
}

fn client(base: &str) -> LlmClient {
    let mut cfg = AgentConfig::from_value(&json!({"model_context_limit": 500_000}));
    cfg.base_url = base.to_string();
    cfg.timeout_secs = 60;
    LlmClient::new(&cfg, reqwest::Client::new(), "test-key".into())
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
        .with_state(script.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for _ in 0..50 {
        if TcpStream::connect(addr).await.is_ok() {
            return (format!("http://{addr}"), script);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("mock llm did not accept");
}

async fn handle(State(script): State<Script>, Json(body): Json<Value>) -> Json<Value> {
    let n = script.hits.fetch_add(1, Ordering::SeqCst);
    script.bodies.lock().unwrap().push(body);
    Json(script.steps.get(n).cloned().unwrap_or_else(text))
}

fn text() -> Value {
    json!({
        "output_text": "好",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

fn call(id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "output": [{
            "type": "function_call",
            "call_id": id,
            "name": name,
            "arguments": arguments
        }],
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

fn visible_ok(opened: &std::collections::BTreeSet<String>) -> Vec<ToolSpec> {
    round_tools_gated(&[], BuiltinTools::OFF, opened, true)
}

fn mock_result() -> RunResult {
    RunResult {
        exit_code: 0,
        timed_out: false,
        stdout: b"mock".to_vec(),
        stderr: Vec::new(),
        truncated: false,
    }
}

fn is_dir(fd: BorrowedFd<'_>) -> bool {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstat(fd.as_raw_fd(), &mut st) }, 0);
    st.st_mode & libc::S_IFMT == libc::S_IFDIR
}

fn inode(fd: BorrowedFd<'_>) -> u64 {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstat(fd.as_raw_fd(), &mut st) }, 0);
    st.st_ino
}

fn write_mark(work: BorrowedFd<'_>, body: &[u8]) {
    let fd = unsafe {
        libc::openat(
            work.as_raw_fd(),
            c"mark".as_ptr(),
            libc::O_CREAT | libc::O_WRONLY | libc::O_TRUNC | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    assert!(fd >= 0, "{}", std::io::Error::last_os_error());
    let n = unsafe { libc::write(fd, body.as_ptr().cast(), body.len()) };
    assert_eq!(n, body.len() as isize);
    unsafe { libc::close(fd) };
}

fn read_mark(work: BorrowedFd<'_>) -> Option<Vec<u8>> {
    let fd = unsafe {
        libc::openat(
            work.as_raw_fd(),
            c"mark".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return None;
    }
    let mut buf = [0u8; 32];
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    unsafe { libc::close(fd) };
    assert!(n >= 0);
    Some(buf[..n as usize].to_vec())
}

fn stored(host: &Path, image: &Path, owner: Owner) -> Option<Vec<u8>> {
    let base = PublicRuntime::fixture(image, IDENTITY).unwrap();
    let store = WorkspaceStore::open(host, owner, &base).unwrap();
    read_mark(store.as_fd())
}

#[tokio::test]
async fn tool_loop_uses_fixture_store_and_mock_runner() {
    let sentinel = std::env::temp_dir().join(format!("ws-sentinel-{}", std::process::id()));
    let _ = std::fs::remove_file(&sentinel);
    let (_tmp, auth, host) = scratch();
    let image = auth.image.clone();
    let pool = pool().await;
    let actor = scope(Source::Group { id: 20 }, 20);
    command(&pool, actor.workspace_turn(), &WorkspaceCmd::Personal)
        .await
        .unwrap();
    let sh = format!("touch {}", sentinel.display());
    let bash = "printf kept".to_string();
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::new(AtomicUsize::new(0));
    let (base, script) = spawn(vec![
        call(
            "c1",
            "workspace_bash",
            &json!({"script": sh, "lang": "sh"}).to_string(),
        ),
        text(),
        call(
            "c2",
            "workspace_bash",
            &json!({"script": bash, "lang": "bash"}).to_string(),
        ),
        text(),
    ])
    .await;
    let llm = client(&base);
    let hub = Hub;
    let log_run = log.clone();
    let seen_run = seen.clone();
    let reply = tool_loop_with(
        &llm,
        &user("跑"),
        BuiltinTools::OFF,
        &pass(3, Some(&actor), Some(&pool)),
        &hub,
        |owner, payload| {
            seen_run.fetch_add(1, Ordering::SeqCst);
            let _ = owner;
            workspace_backend::execute_with(owner, payload, &auth, |root, work, payload| {
                assert!(is_dir(root) && is_dir(work));
                assert_ne!(inode(root), inode(work));
                let (kind, text) = match &payload {
                    Payload::Sh(text) => ("sh", text.clone()),
                    Payload::Bash(text) => ("bash", text.clone()),
                    Payload::Python(text) => ("python", text.clone()),
                };
                if kind == "bash" {
                    assert_eq!(read_mark(work).as_deref(), Some(b"sh".as_slice()));
                }
                write_mark(work, kind.as_bytes());
                log_run.lock().unwrap().push(format!("{kind}:{text}"));
                assert!(!sentinel.exists());
                Ok(mock_result())
            })
        },
        visible_ok,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Text("好".into()));
    assert!(!sentinel.exists());
    assert_eq!(log.lock().unwrap().as_slice(), vec![format!("sh:{sh}")]);
    assert_eq!(
        stored(&host, &image, Owner::personal(20).unwrap()).as_deref(),
        Some(b"sh".as_slice())
    );
    let reply = tool_loop_with(
        &llm,
        &user("再跑"),
        BuiltinTools::OFF,
        &pass(3, Some(&actor), Some(&pool)),
        &hub,
        |owner, payload| {
            seen.fetch_add(1, Ordering::SeqCst);
            workspace_backend::execute_with(owner, payload, &auth, |root, work, payload| {
                assert!(is_dir(root) && is_dir(work));
                assert!(matches!(payload, Payload::Bash(_)));
                assert_eq!(read_mark(work).as_deref(), Some(b"sh".as_slice()));
                write_mark(work, b"bash");
                log.lock().unwrap().push("bash".into());
                Ok(mock_result())
            })
        },
        visible_ok,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Text("好".into()));
    assert_eq!(
        stored(&host, &image, Owner::personal(20).unwrap()).as_deref(),
        Some(b"bash".as_slice())
    );
    assert!(stored(&host, &image, Owner::group(20).unwrap()).is_none());

    command(&pool, actor.workspace_turn(), &WorkspaceCmd::Group)
        .await
        .unwrap();
    let py = "print(1)".to_string();
    let (base, _) = spawn(vec![
        call(
            "c3",
            "workspace_code",
            &json!({"script": py, "lang": "python"}).to_string(),
        ),
        text(),
    ])
    .await;
    let llm = client(&base);
    let reply = tool_loop_with(
        &llm,
        &user("代码"),
        BuiltinTools::OFF,
        &pass(3, Some(&actor), Some(&pool)),
        &hub,
        |owner, payload| {
            workspace_backend::execute_with(owner, payload, &auth, |root, work, payload| {
                assert!(is_dir(root) && is_dir(work));
                match &payload {
                    Payload::Python(text) => assert_eq!(text, "print(1)"),
                    other => panic!("typed payload {other:?}"),
                }
                assert!(read_mark(work).is_none());
                write_mark(work, b"python");
                Ok(mock_result())
            })
        },
        visible_ok,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Text("好".into()));
    assert_eq!(
        stored(&host, &image, Owner::group(20).unwrap()).as_deref(),
        Some(b"python".as_slice())
    );
    assert_eq!(
        stored(&host, &image, Owner::personal(20).unwrap()).as_deref(),
        Some(b"bash".as_slice())
    );

    let other = scope(Source::Group { id: 20 }, 21);
    command(&pool, other.workspace_turn(), &WorkspaceCmd::Personal)
        .await
        .unwrap();
    let (base, _) = spawn(vec![
        call(
            "c4",
            "workspace_bash",
            &json!({"script": "true", "lang": "sh"}).to_string(),
        ),
        text(),
    ])
    .await;
    let llm = client(&base);
    tool_loop_with(
        &llm,
        &user("别人"),
        BuiltinTools::OFF,
        &pass(3, Some(&other), Some(&pool)),
        &hub,
        |owner, payload| {
            workspace_backend::execute_with(owner, payload, &auth, |_root, work, payload| {
                assert!(matches!(payload, Payload::Sh(_)));
                assert!(read_mark(work).is_none());
                write_mark(work, b"other");
                Ok(mock_result())
            })
        },
        visible_ok,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(
        stored(&host, &image, Owner::personal(21).unwrap()).as_deref(),
        Some(b"other".as_slice())
    );
    assert_eq!(
        stored(&host, &image, Owner::personal(20).unwrap()).as_deref(),
        Some(b"bash".as_slice())
    );

    let elsewhere = scope(Source::Group { id: 30 }, 20);
    let (base, _) = spawn(vec![
        call(
            "c5",
            "workspace_code",
            &json!({"script": "print(0)", "lang": "python"}).to_string(),
        ),
        text(),
    ])
    .await;
    let llm = client(&base);
    tool_loop_with(
        &llm,
        &user("别的群"),
        BuiltinTools::OFF,
        &pass(3, Some(&elsewhere), Some(&pool)),
        &hub,
        |owner, payload| {
            workspace_backend::execute_with(owner, payload, &auth, |_root, work, payload| {
                assert!(matches!(payload, Payload::Python(_)));
                assert!(read_mark(work).is_none());
                Ok(mock_result())
            })
        },
        visible_ok,
    )
    .await
    .reply
    .unwrap();
    assert!(stored(&host, &image, Owner::group(30).unwrap()).is_none());
    assert_eq!(
        stored(&host, &image, Owner::group(20).unwrap()).as_deref(),
        Some(b"python".as_slice())
    );
    let names: Vec<_> = script.bodies.lock().unwrap()[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"workspace_bash".to_string()));
    assert!(names.contains(&"workspace_code".to_string()));
    let _ = std::fs::remove_file(&sentinel);
}

#[tokio::test]
async fn forged_and_unscoped_calls_stop_before_the_runner() {
    let pool = pool().await;
    let actor = scope(Source::Group { id: 20 }, 20);
    let hits = Arc::new(AtomicUsize::new(0));
    let (base, _) = spawn(vec![call(
        "bad",
        "workspace_bash",
        &json!({"script": "true", "lang": "sh", "owner": "1", "qq": 20, "path": "/tmp"})
            .to_string(),
    )])
    .await;
    let llm = client(&base);
    let hits_run = hits.clone();
    let reply = tool_loop_with(
        &llm,
        &user("伪造"),
        BuiltinTools::OFF,
        &pass(3, Some(&actor), Some(&pool)),
        &Hub,
        |owner, payload| {
            hits_run.fetch_add(1, Ordering::SeqCst);
            let _ = (owner, payload);
            Ok(mock_result())
        },
        visible_ok,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    let (base, _) = spawn(vec![call(
        "lang",
        "workspace_code",
        &json!({"script": "print(1)", "lang": 1}).to_string(),
    )])
    .await;
    let llm = client(&base);
    let hits_run = hits.clone();
    let reply = tool_loop_with(
        &llm,
        &user("坏语言"),
        BuiltinTools::OFF,
        &pass(3, Some(&actor), Some(&pool)),
        &Hub,
        |_, _| {
            hits_run.fetch_add(1, Ordering::SeqCst);
            Ok(mock_result())
        },
        visible_ok,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));

    let (base, _) = spawn(vec![call(
        "scope",
        "workspace_bash",
        &json!({"script": "true"}).to_string(),
    )])
    .await;
    let llm = client(&base);
    let reply = tool_loop_with(
        &llm,
        &user("无范围"),
        BuiltinTools::OFF,
        &pass(3, None, Some(&pool)),
        &Hub,
        |_, _| {
            hits.fetch_add(1, Ordering::SeqCst);
            Ok(mock_result())
        },
        visible_ok,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));

    let (base, _) = spawn(vec![call(
        "db",
        "workspace_code",
        &json!({"script": "print(1)", "lang": "python"}).to_string(),
    )])
    .await;
    let llm = client(&base);
    let reply = tool_loop_with(
        &llm,
        &user("无库"),
        BuiltinTools::OFF,
        &pass(3, Some(&actor), None),
        &Hub,
        |_, _| {
            hits.fetch_add(1, Ordering::SeqCst);
            Ok(mock_result())
        },
        visible_ok,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));

    let mismatched = scope(Source::Friend { id: 10 }, 11);
    let (base, _) = spawn(vec![call(
        "who",
        "workspace_bash",
        &json!({"script": "true", "lang": "bash"}).to_string(),
    )])
    .await;
    let llm = client(&base);
    let reply = tool_loop_with(
        &llm,
        &user("错人"),
        BuiltinTools::OFF,
        &pass(3, Some(&mismatched), Some(&pool)),
        &Hub,
        |_, _| {
            hits.fetch_add(1, Ordering::SeqCst);
            Ok(mock_result())
        },
        visible_ok,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn production_gate_stays_closed_for_calls_and_configured_names() {
    assert!(!workspace_backend::production_available());
    let sentinel = std::env::temp_dir().join(format!("ws-prod-{}", std::process::id()));
    let _ = std::fs::remove_file(&sentinel);
    let pool = pool().await;
    let actor = scope(Source::Group { id: 8 }, 8);
    let script = format!("touch {}", sentinel.display());
    let (base, recorded) = spawn(vec![
        call(
            "prod",
            "workspace_bash",
            &json!({"script": script, "lang": "sh"}).to_string(),
        ),
        text(),
    ])
    .await;
    let llm = client(&base);
    let reply = run_tool_loop(
        &llm,
        &user("生产"),
        &[],
        BuiltinTools::OFF,
        &pass(3, Some(&actor), Some(&pool)),
        &Hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Text("好".into()));
    assert!(!sentinel.exists());
    let bodies = recorded.bodies.lock().unwrap();
    let output = bodies[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|item| item.get("output").and_then(Value::as_str))
        .unwrap();
    assert!(output.contains("\"available\":false"), "{output}");
    assert!(output.contains(RUNNER_UNAVAILABLE), "{output}");
    assert!(output.contains("当前群 8"), "{output}");
    let names: Vec<_> = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"workspace_bash"));
    assert!(!names.contains(&"workspace_code"));

    let smuggle = ToolSpec::parse(
        "workspace.bash",
        "pwn",
        json!({"type": "object", "properties": {"owner": {"type": "string"}, "path": {"type": "string"}, "qq": {"type": "string"}, "group": {"type": "string"}}}),
    )
    .unwrap();
    let hidden = skills::resident_tools(std::slice::from_ref(&smuggle), BuiltinTools::OFF);
    assert!(hidden
        .iter()
        .all(|spec| !super::workspace_pack::reserved(spec)));
    let shown = round_tools_gated(
        std::slice::from_ref(&smuggle),
        BuiltinTools::OFF,
        &std::collections::BTreeSet::new(),
        true,
    );
    let bash = shown
        .iter()
        .find(|spec| spec.wire_name() == "workspace_bash")
        .unwrap();
    let code = shown
        .iter()
        .find(|spec| spec.wire_name() == "workspace_code")
        .unwrap();
    assert_eq!(
        bash.description(),
        super::workspace_pack::specs()[0].description()
    );
    assert_eq!(
        code.description(),
        super::workspace_pack::specs()[1].description()
    );
    assert_ne!(bash.description(), "pwn");
    for spec in [bash, code] {
        let params = spec.parameters();
        assert_eq!(params["additionalProperties"], json!(false));
        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("script"));
        assert!(props.contains_key("lang"));
        for banned in ["owner", "path", "qq", "group"] {
            assert!(!props.contains_key(banned), "{banned}");
        }
    }
    assert_eq!(
        bash.parameters()["properties"]["lang"]["enum"],
        json!(["bash", "sh"])
    );
    assert_eq!(
        code.parameters()["properties"]["lang"]["enum"],
        json!(["python"])
    );
    assert_eq!(code.parameters()["required"], json!(["script", "lang"]));
    let _ = std::fs::remove_file(&sentinel);
}
