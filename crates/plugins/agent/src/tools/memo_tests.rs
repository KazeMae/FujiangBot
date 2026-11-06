use std::sync::atomic::Ordering;
use std::sync::Mutex;

use fujiang_core::Source;
use serde_json::json;

use super::super::{
    bind_memo_scope, run_tool_loop, BuiltinTools, MemoCallScope, ToolPass, ToolReply,
    TOOL_FAIL_REPLY,
};
use super::{spawn_app, test_client, user, Catch, Kind, Rec, Script};

#[test]
fn memo_scope_replaces_source_and_keeps_the_body() {
    let scope = MemoCallScope::from_source(Source::Group { id: 7 }, 3);
    let bound = bind_memo_scope(
        "memo",
        Some(&scope),
        json!({
            "source_kind": "friend",
            "source_id": 1,
            "session": "private:1",
            "body": "记下"
        }),
    );
    assert_eq!(
        bound,
        json!({
            "source_kind": "group",
            "source_id": 7,
            "session": "group:7",
            "user_id": 3,
            "body": "记下"
        })
    );
    assert_eq!(
        bind_memo_scope("rank", Some(&scope), json!({"handle": "tourist"})),
        json!({"handle": "tourist"})
    );
    let blank = bind_memo_scope("memo", Some(&scope), json!([]));
    assert_eq!(blank["source_kind"], "group");
    assert_eq!(blank["user_id"], 3);
}

#[tokio::test(flavor = "current_thread")]
async fn memo_list_fills_the_current_chat_and_a_missing_service_degrades() {
    let script = Script::new(Kind::MemoList);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let scope = MemoCallScope::from_source(Source::Friend { id: 4 }, 9);
    let pass = ToolPass {
        max_rounds: 3,
        scope: Some(&scope),
        memory: None,
        memory_unused_secs: 0,
        memory_max_entries: 0,
    };
    let memo_only = BuiltinTools {
        memo: true,
        ..BuiltinTools::OFF
    };
    let reply = run_tool_loop(&client, &user("备忘里有什么"), &[], memo_only, &pass, &hub)
        .await
        .reply
        .unwrap();
    assert_eq!(reply, ToolReply::Text("这个对话有一条备忘。".into()));
    assert_eq!(
        hub.log.lock().unwrap().clone(),
        vec![(
            "memo".into(),
            "list".into(),
            json!({
                "source_kind": "friend",
                "source_id": 4,
                "session": "private:4",
                "user_id": 9
            })
        )]
    );
    let names: Vec<_> = script.bodies.lock().unwrap()[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["memo_list".to_string()]);

    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let script = Script::new(Kind::MemoList);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec {
        log: Mutex::new(Vec::new()),
        fail: true,
    };
    let reply = run_tool_loop(&client, &user("备忘里有什么"), &[], memo_only, &pass, &hub)
        .await
        .reply
        .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert_eq!(hub.log.lock().unwrap().len(), 1);
    assert_eq!(hub.log.lock().unwrap()[0].0, "memo");
    assert_eq!(script.hits.load(Ordering::SeqCst), 1);
    assert!(catch.has("agent tool"));
}
