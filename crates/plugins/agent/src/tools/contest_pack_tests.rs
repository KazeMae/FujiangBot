use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::json;

use super::super::skills::{round_tools, CLOSED_SKILL};
use super::super::{
    assemble_tools, resident_tools, run_tool_loop, BuiltinTools, ToolPass, ToolReply, ToolSpec,
    TOOL_FAIL_REPLY,
};
use super::{one_call_with, saying, spawn_app, test_client, user, Catch, Kind, Rec, Script};

fn contest_on() -> BuiltinTools {
    BuiltinTools {
        contest: true,
        ..BuiltinTools::OFF
    }
}

fn script_of(steps: Vec<serde_json::Value>) -> Script {
    let mut script = Script::new(Kind::Text);
    script.steps = Arc::new(steps);
    script
}

fn tool_names(body: &serde_json::Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn contest_tools_stay_out_until_the_pack_is_open() {
    assert!(assemble_tools(&[], BuiltinTools::ON)
        .iter()
        .all(|spec| spec.service() != "contest"));
    assert!(resident_names(BuiltinTools::ON)
        .iter()
        .all(|name| !name.starts_with("contest.")));
    let opened = BTreeSet::from(["contest".to_string()]);
    let hidden = round_tools(&[], BuiltinTools::OFF, &opened);
    assert!(hidden.iter().all(|spec| spec.service() != "contest"));
    let smuggle = ToolSpec::parse("contest.upcoming", "偷偷", json!({})).unwrap();
    let shown = round_tools(std::slice::from_ref(&smuggle), contest_on(), &opened);
    let upcoming = shown
        .iter()
        .find(|spec| spec.name() == "contest.upcoming")
        .unwrap();
    assert_eq!(upcoming.wire_name(), "contest_upcoming");
    assert_eq!(upcoming.method(), "upcoming");
    assert!(!upcoming.description().contains("偷偷"));
    assert!(upcoming.parameters().get("required").is_none());
    assert_eq!(
        upcoming.parameters(),
        &json!({"type": "object", "properties": {"oj": {"type": "string"}}})
    );
    let today = shown
        .iter()
        .find(|spec| spec.name() == "contest.today")
        .unwrap();
    assert_eq!(today.wire_name(), "contest_today");
    assert_eq!(today.parameters()["properties"], json!({}));
    assert_eq!(
        shown
            .iter()
            .filter(|spec| spec.service() == "contest")
            .map(ToolSpec::name)
            .collect::<Vec<_>>(),
        vec!["contest.upcoming", "contest.today"]
    );
}

fn resident_names(builtins: BuiltinTools) -> Vec<String> {
    resident_tools(&[], builtins)
        .iter()
        .map(|spec| spec.name().to_string())
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn closed_contest_does_not_call_the_hub() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let script = script_of(vec![one_call_with(
        "call_up",
        "contest_upcoming",
        r#"{"oj":"Codeforces"}"#,
    )]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(
        &client,
        &user("比赛"),
        &[],
        BuiltinTools::ON,
        &ToolPass::rounds(3),
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert!(hub.log.lock().unwrap().is_empty());
    assert_eq!(script.hits.load(Ordering::SeqCst), 1);
    assert!(catch.has(CLOSED_SKILL));
    assert!(!catch.has("agent tool unknown"));
    assert!(!tool_names(&script.bodies.lock().unwrap()[0])
        .iter()
        .any(|name| name.starts_with("contest_")));
}

#[tokio::test]
async fn opened_contest_calls_the_hub_with_the_old_schema() {
    let script = script_of(vec![
        one_call_with("open", "skills_open", r#"{"name":"contest"}"#),
        json!({
            "output": [
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
        }),
        saying("今天有两场"),
    ]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(
        &client,
        &user("今天有比赛吗"),
        &[],
        contest_on(),
        &ToolPass::rounds(3),
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Text("今天有两场".into()));
    assert_eq!(
        hub.log.lock().unwrap().clone(),
        vec![
            (
                "contest".into(),
                "upcoming".into(),
                json!({"oj": "Codeforces"})
            ),
            ("contest".into(), "today".into(), json!({})),
        ]
    );
    let bodies = script.bodies.lock().unwrap().clone();
    assert_eq!(
        tool_names(&bodies[0]),
        vec!["skills_list".to_string(), "skills_open".to_string()]
    );
    assert_eq!(
        tool_names(&bodies[1]),
        vec![
            "skills_list".to_string(),
            "skills_open".to_string(),
            "contest_upcoming".to_string(),
            "contest_today".to_string(),
        ]
    );
    let upcoming = bodies[1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "contest_upcoming")
        .unwrap();
    assert_eq!(
        upcoming["parameters"],
        json!({"type": "object", "properties": {"oj": {"type": "string"}}})
    );
    assert_eq!(
        upcoming["description"],
        "列出即将开始的比赛。可选字符串参数 oj，例如 Codeforces 或 Atcoder。不传则返回全部。"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn opened_contest_today_error_uses_the_degraded_reply() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let script = script_of(vec![
        one_call_with("open", "skills_open", r#"{"name":"contest"}"#),
        one_call_with("call_today", "contest_today", "{}"),
    ]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec {
        log: std::sync::Mutex::new(Vec::new()),
        fail: true,
    };
    let reply = run_tool_loop(
        &client,
        &user("今天有比赛吗"),
        &[],
        contest_on(),
        &ToolPass::rounds(3),
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    let log = hub.log.lock().unwrap().clone();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].0, "contest");
    assert_eq!(log[0].1, "today");
    assert_eq!(script.hits.load(Ordering::SeqCst), 2);
    assert!(catch.has("agent tool"));
    assert!(!catch.has(CLOSED_SKILL));
}
