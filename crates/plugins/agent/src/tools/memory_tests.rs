use fujiang_core::Source;
use serde_json::{json, Value};
use sqlx::SqlitePool;

use super::super::{
    assemble_tools, bind_memory_scope, run_tool_loop, BuiltinTools, MemoCallScope, ToolPass,
    ToolReply, ToolSpec, TOOL_FAIL_REPLY,
};
use super::{spawn_app, test_client, user, Catch, Kind, Rec, Script};
use crate::command::enabled_builtins;
use crate::config::AgentConfig;
use crate::layered_memory::call;
use crate::session;

async fn pool() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    session::ensure_schema(&pool).await.unwrap();
    pool
}

fn memory_only(write: bool) -> BuiltinTools {
    BuiltinTools {
        memory: true,
        memory_write: write,
        ..BuiltinTools::OFF
    }
}

fn pass<'a>(scope: &'a MemoCallScope, pool: Option<&'a SqlitePool>) -> ToolPass<'a> {
    ToolPass {
        max_rounds: 3,
        scope: Some(scope),
        memory: pool,
    }
}

#[test]
fn memory_tools_follow_the_flags_and_describe_the_parameters() {
    let on = AgentConfig::from_value(&json!({}));
    let specs = assemble_tools(&[], enabled_builtins(&on));
    let names: Vec<_> = specs.iter().map(ToolSpec::name).collect::<Vec<_>>();
    assert!(names.contains(&"memory.list"));
    assert!(names.contains(&"memory.get"));
    assert!(names.contains(&"memory.upsert"));
    assert!(names.contains(&"memory.forget"));
    let upsert = specs
        .iter()
        .find(|spec| spec.name() == "memory.upsert")
        .unwrap();
    assert_eq!(upsert.wire_name(), "memory_upsert");
    assert_eq!(upsert.service(), "memory");
    assert_eq!(upsert.method(), "upsert");
    assert!(upsert.parameters()["properties"].get("value").is_some());
    let forget = specs
        .iter()
        .find(|spec| spec.name() == "memory.forget")
        .unwrap();
    assert!(forget.parameters()["properties"].get("prefix").is_some());

    let read = AgentConfig::from_value(&json!({"enable_memory_write_tools": false}));
    let read_specs = assemble_tools(&[], enabled_builtins(&read));
    let names: Vec<_> = read_specs.iter().map(ToolSpec::name).collect::<Vec<_>>();
    assert!(names.contains(&"memory.list"));
    assert!(names.contains(&"memory.get"));
    assert!(!names.contains(&"memory.upsert"));
    assert!(!names.contains(&"memory.forget"));

    let off = AgentConfig::from_value(&json!({
        "enable_memory_tools": false,
        "enable_memory_write_tools": true
    }));
    assert!(assemble_tools(&[], enabled_builtins(&off))
        .iter()
        .all(|spec| spec.service() != "memory"));
    let reads = assemble_tools(&[], memory_only(false));
    assert_eq!(reads.len(), 2);
    assert!(reads
        .iter()
        .all(|spec| spec.method() == "list" || spec.method() == "get"));
}

#[test]
fn host_overwrites_the_speaker_and_the_group() {
    let group = MemoCallScope::from_source(Source::Group { id: 10086 }, 123456);
    let bound = bind_memory_scope(
        "memory",
        Some(&group),
        json!({
            "scope": "user",
            "user_id": 1,
            "scope_id": "1",
            "group_id": 1,
            "source": "private",
            "key": "nick",
            "value": "小明"
        }),
    );
    assert_eq!(bound["user_id"], 123456);
    assert_eq!(bound["group_id"], 10086);
    assert_eq!(bound["source"], "group:10086");
    assert!(bound.get("scope_id").is_none());
    assert_eq!(bound["key"], "nick");

    let private = MemoCallScope::from_source(Source::Friend { id: 80 }, 555);
    let bound = bind_memory_scope(
        "memory",
        Some(&private),
        json!({"scope": "user", "user_id": 1, "group_id": 9, "scope_id": "9"}),
    );
    assert_eq!(bound["user_id"], 555);
    assert!(bound.get("group_id").is_none());
    assert_eq!(bound["source"], "private");
    assert!(bound.get("scope_id").is_none());

    let blank = bind_memory_scope("memory", Some(&group), json!([]));
    assert_eq!(blank["user_id"], 123456);
    assert_eq!(blank["group_id"], 10086);
    assert_eq!(
        bind_memory_scope("rank", Some(&group), json!({"user_id": 1})),
        json!({"user_id": 1})
    );
    assert_eq!(
        bind_memory_scope("memory", None, json!({"user_id": 1})),
        json!({"user_id": 1})
    );
}

#[tokio::test]
async fn bound_group_and_account_scopes_ignore_a_forged_id() {
    let pool = pool().await;
    let scope = MemoCallScope::from_source(Source::Group { id: 10086 }, 123456);
    let group = bind_memory_scope(
        "memory",
        Some(&scope),
        json!({"scope": "group", "group_id": 1, "scope_id": "1", "key": "rule", "value": "别刷屏"}),
    );
    let row = call(&pool, "upsert", &group, 5).await.unwrap();
    assert_eq!(row["scope"], "group");
    assert_eq!(row["scope_id"], "10086");
    assert_eq!(row["source"], "group:10086");

    let account = bind_memory_scope(
        "memory",
        Some(&scope),
        json!({"scope": "account", "scope_id": "nope", "key": "tone", "value": "短"}),
    );
    let row = call(&pool, "upsert", &account, 6).await.unwrap();
    assert_eq!(row["scope"], "account");
    assert_eq!(row["scope_id"], "");

    let listed = call(
        &pool,
        "list",
        &bind_memory_scope("memory", Some(&scope), json!({})),
        7,
    )
    .await
    .unwrap();
    let keys: Vec<_> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["scope"].as_str().unwrap(),
                row["scope_id"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(keys.contains(&("group", "10086")));
    assert!(keys.contains(&("account", "")));
    assert!(!keys.iter().any(|row| row.1 == "1" || row.1 == "nope"));
}

async fn spoken(
    scope: MemoCallScope,
    pool: &SqlitePool,
) -> (ToolReply, Vec<(String, String, Value)>) {
    let script = Script::new(Kind::MemoryUpsert);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let turn = run_tool_loop(
        &client,
        &user("记住我"),
        &[],
        memory_only(true),
        &pass(&scope, Some(pool)),
        &hub,
    )
    .await;
    assert_eq!(script.hits.load(std::sync::atomic::Ordering::SeqCst), 2);
    let log = hub.log.lock().unwrap().clone();
    (turn.reply.unwrap(), log)
}

#[tokio::test]
async fn group_message_upserts_the_speaker_not_a_forged_qq() {
    let pool = pool().await;
    let scope = MemoCallScope::from_source(Source::Group { id: 10086 }, 123456);
    let (reply, log) = spoken(scope, &pool).await;
    assert_eq!(reply, ToolReply::Text("记下了".into()));
    assert!(log.is_empty());
    let row: (String, String, String, String, i64) = sqlx::query_as(
        "SELECT scope_kind, scope_id, value, source, updated_at FROM layered_memories",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, "user");
    assert_eq!(row.1, "123456");
    assert_eq!(row.2, "小明");
    assert_eq!(row.3, "group:10086");
    assert!(row.4 > 1_000_000_000);
    let forged: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM layered_memories WHERE scope_id = '1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(forged, 0);
}

#[tokio::test]
async fn private_message_upserts_the_speaker() {
    let pool = pool().await;
    let scope = MemoCallScope::from_source(Source::Friend { id: 80 }, 555);
    let (reply, log) = spoken(scope, &pool).await;
    assert_eq!(reply, ToolReply::Text("记下了".into()));
    assert!(log.is_empty());
    let row: (String, String, String) =
        sqlx::query_as("SELECT scope_kind, scope_id, source FROM layered_memories")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, ("user".into(), "555".into(), "private".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn memory_failures_degrade_without_calling_the_hub() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let db = pool().await;
    let scope = MemoCallScope::from_source(Source::Group { id: 1 }, 2);
    let script = Script::new(Kind::MemoryBad);
    let base = spawn_app(script).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(
        &client,
        &user("记住"),
        &[],
        memory_only(true),
        &pass(&scope, Some(&db)),
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert!(hub.log.lock().unwrap().is_empty());
    assert!(catch.has("agent tool"));

    let script = Script::new(Kind::MemoryUpsert);
    let base = spawn_app(script).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let closed = ToolPass {
        max_rounds: 3,
        scope: Some(&scope),
        memory: None,
    };
    let reply = run_tool_loop(
        &client,
        &user("记住"),
        &[],
        memory_only(true),
        &closed,
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));

    let script = Script::new(Kind::MemoryUpsert);
    let base = spawn_app(script).await;
    let client = test_client(&base);
    let open = pool().await;
    let noscope = ToolPass {
        max_rounds: 3,
        scope: None,
        memory: Some(&open),
    };
    let reply = run_tool_loop(
        &client,
        &user("记住"),
        &[],
        memory_only(true),
        &noscope,
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM layered_memories")
        .fetch_one(&open)
        .await
        .unwrap();
    assert_eq!(n, 0);
}
