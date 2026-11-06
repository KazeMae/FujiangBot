use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fujiang_core::Source;
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use super::super::schedule_pack::{PACK, PACK_DESC};
use super::super::skills::{self, round_tools, CLOSED_SKILL};
use super::super::{
    run_tool_loop, schedule_tick, BuiltinTools, MemoCallScope, ToolHub, ToolPass, ToolReply,
    ToolSpec, TOOL_FAIL_REPLY,
};
use super::{one_call_with, saying, spawn_app, test_client, user};
use crate::schedule::{self, ensure_schema};

struct Api {
    calls: Mutex<Vec<(String, Value)>>,
    replies: Mutex<VecDeque<Result<Value, String>>>,
}

impl Api {
    fn script(replies: Vec<Result<Value, &'static str>>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|item| item.map_err(str::to_string))
                    .collect(),
            ),
        }
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ToolHub for Api {
    async fn invoke(&self, _service: &str, _method: &str, _args: Value) -> anyhow::Result<Value> {
        Ok(Value::Null)
    }

    async fn onebot(&self, action: &str, params: Value) -> anyhow::Result<Value> {
        self.calls
            .lock()
            .unwrap()
            .push((action.to_string(), params));
        match self.replies.lock().unwrap().pop_front() {
            Some(Ok(value)) => Ok(value),
            Some(Err(err)) => anyhow::bail!("{err}"),
            None => anyhow::bail!("no scripted reply"),
        }
    }
}

async fn pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    ensure_schema(&pool).await.unwrap();
    pool
}

fn scope(user_id: i64) -> MemoCallScope {
    MemoCallScope::from_source(Source::Group { id: 3 }, user_id)
}

#[test]
fn schedule_pack_stays_closed_until_opened() {
    let closed = round_tools(&[], BuiltinTools::OFF, &BTreeSet::new());
    assert!(closed
        .iter()
        .all(|spec| !spec.name().starts_with("schedule.")));
    let mut opened = BTreeSet::new();
    let now = opened.clone();
    let blocked = skills::exec(
        "schedule_create",
        &json!({}),
        BuiltinTools::OFF,
        &now,
        &mut opened,
    );
    match blocked {
        Some(skills::SkillOut::Ready(Err(tag))) => assert_eq!(tag, CLOSED_SKILL),
        other => panic!("{other:?}"),
    }
    let listed = match skills::exec(
        "skills_list",
        &json!({}),
        BuiltinTools::OFF,
        &now,
        &mut opened,
    ) {
        Some(skills::SkillOut::Ready(Ok(value))) => value,
        other => panic!("{other:?}"),
    };
    let row = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == PACK)
        .unwrap();
    assert_eq!(row["description"], PACK_DESC);
    assert_eq!(row.as_object().unwrap().len(), 2);
    let raw = serde_json::to_string(&listed).unwrap();
    assert!(!raw.contains("schedule.list"));
    assert!(!raw.contains("schedule_list"));
    assert!(!raw.contains("send_private_msg"));
    skills::exec(
        "skills_open",
        &json!({"name": " schedule "}),
        BuiltinTools::OFF,
        &now,
        &mut opened,
    );
    let smuggle = ToolSpec::parse("schedule.list", "偷偷", json!({})).unwrap();
    let shown = round_tools(std::slice::from_ref(&smuggle), BuiltinTools::OFF, &opened);
    assert_eq!(
        shown
            .iter()
            .filter(|spec| spec.wire_name() == "schedule_list")
            .count(),
        1
    );
    assert!(!shown.iter().any(|spec| spec.description().contains("偷偷")));
}

#[tokio::test]
async fn tool_loop_injects_the_speaker_and_rejects_a_forged_peer() {
    let db = pool().await;
    let who = scope(7);
    let script = super::Script::new(super::Kind::Text);
    let mut steps = script;
    steps.steps = Arc::new(vec![
        one_call_with("open", "skills_open", "{\"name\":\"schedule\"}"),
        one_call_with(
            "make",
            "schedule_create",
            "{\"kind\":\"private\",\"at\":2000000000,\"payload\":\"hi\",\"creator_qq\":999}",
        ),
        saying("好"),
    ]);
    let base = spawn_app(steps).await;
    let client = test_client(&base);
    let pass = ToolPass {
        max_rounds: 3,
        scope: Some(&who),
        memory: Some(&db),
        memory_unused_secs: 1,
        memory_max_entries: 1,
    };
    let reply = run_tool_loop(
        &client,
        &user("记一下"),
        &[],
        BuiltinTools::OFF,
        &pass,
        &Api::script(vec![]),
    )
    .await;
    assert!(matches!(reply.reply, Ok(ToolReply::Text(_))));
    let job = schedule::load(&db, 1).await.unwrap().unwrap();
    assert_eq!(job.creator_qq, 7);
    assert_eq!(job.user_id, Some(7));

    let mut forged = super::Script::new(super::Kind::Text);
    forged.steps = Arc::new(vec![
        one_call_with("open", "skills_open", "{\"name\":\"schedule\"}"),
        one_call_with(
            "make",
            "schedule_create",
            "{\"kind\":\"private\",\"user_id\":8,\"at\":2000000000,\"payload\":\"no\",\"creator_qq\":8}",
        ),
    ]);
    let base = spawn_app(forged).await;
    let client = test_client(&base);
    let fresh = pool().await;
    let pass = ToolPass {
        memory: Some(&fresh),
        ..pass
    };
    let reply = run_tool_loop(
        &client,
        &user("记一下"),
        &[],
        BuiltinTools::OFF,
        &pass,
        &Api::script(vec![]),
    )
    .await;
    assert!(matches!(
        reply.reply,
        Ok(ToolReply::Degraded(TOOL_FAIL_REPLY))
    ));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_jobs")
        .fetch_one(&fresh)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn fire_rechecks_membership_and_does_not_read_private_chats() {
    let db = pool().await;
    let now = 1_700_000_000_i64;
    sqlx::query(
        "INSERT INTO scheduled_jobs
         (creator_qq, \"at\", action, kind, group_id, payload, enabled, created_at, next_run_at)
         VALUES (7, ?1, 'send_text', 'group', 42, '群里', 1, ?1, ?1)",
    )
    .bind(now)
    .execute(&db)
    .await
    .unwrap();
    let denied = Api::script(vec![Ok(json!({"user_id": 1, "nickname": "other"}))]);
    schedule_tick(&db, &denied, now).await.unwrap();
    assert_eq!(denied.calls().len(), 1);
    assert_eq!(denied.calls()[0].0, "get_group_member_info");
    assert_eq!(denied.calls()[0].1["user_id"], 7);
    assert_eq!(denied.calls()[0].1["group_id"], 42);
    let job = schedule::load(&db, 1).await.unwrap().unwrap();
    assert_eq!(job.enabled, 0);
    assert_eq!(job.last_error.as_deref(), Some("denied"));

    sqlx::query(
        "INSERT INTO scheduled_jobs
         (creator_qq, \"at\", action, kind, group_id, payload, enabled, created_at, next_run_at)
         VALUES (7, ?1, 'send_text', 'group', 42, '可以', 1, ?1, ?1)",
    )
    .bind(now)
    .execute(&db)
    .await
    .unwrap();
    let allowed = Api::script(vec![
        Ok(json!({"user_id": 7})),
        Ok(json!({"message_id": 3})),
    ]);
    schedule_tick(&db, &allowed, now).await.unwrap();
    let calls = allowed.calls();
    let names: Vec<&str> = calls.iter().map(|call| call.0.as_str()).collect();
    assert_eq!(names, vec!["get_group_member_info", "send_group_msg"]);
    assert_eq!(allowed.calls()[1].1["group_id"], 42);
    assert_eq!(allowed.calls()[1].1["message"][0]["data"]["text"], "可以");
    assert_eq!(schedule::load(&db, 2).await.unwrap().unwrap().enabled, 0);

    sqlx::query(
        "INSERT INTO scheduled_jobs
         (creator_qq, \"at\", action, kind, user_id, payload, enabled, created_at, next_run_at)
         VALUES (7, ?1, 'send_text', 'private', 7, '自己', 1, ?1, ?1)",
    )
    .bind(now)
    .execute(&db)
    .await
    .unwrap();
    let private = Api::script(vec![Ok(json!({"message_id": 4}))]);
    schedule_tick(&db, &private, now).await.unwrap();
    assert_eq!(private.calls().len(), 1);
    assert_eq!(private.calls()[0].0, "send_private_msg");
    assert_eq!(private.calls()[0].1["user_id"], 7);
    assert_eq!(private.calls()[0].1["message"][0]["data"]["text"], "自己");
    for (action, _) in private.calls() {
        assert_ne!(action, "get_friend_msg_history");
        assert_ne!(action, "get_msg");
        assert_ne!(action, "get_group_member_info");
    }
}
