use std::collections::BTreeSet;
use std::sync::Arc;

use fujiang_core::Source;
use serde_json::json;

use super::super::skills::{round_tools, CLOSED_SKILL};
use super::super::{
    assemble_tools, bind_fun_scope, resident_tools, run_tool_loop, BuiltinTools, MemoCallScope,
    ToolPass, ToolReply, ToolSpec, TOOL_FAIL_REPLY,
};
use super::{one_call_with, saying, spawn_app, test_client, user, Catch, Kind, Rec, Script};

const WRITE_NAMES: &[&str] = &[
    "delete", "del", "retire", "add", "set", "merge", "alias", "attach", "detach", "album",
];

fn fun_on() -> BuiltinTools {
    BuiltinTools {
        fun: true,
        ..BuiltinTools::OFF
    }
}

#[test]
fn fun_reads_register_and_writes_stay_out() {
    let resident = resident_tools(&[], BuiltinTools::ON);
    assert!(resident.iter().all(|spec| spec.service() != "fun"));
    assert!(assemble_tools(&[], fun_on()).is_empty());

    let opened = BTreeSet::from(["fun".to_string()]);
    let hidden = round_tools(&[], BuiltinTools::OFF, &opened);
    assert!(hidden.iter().all(|spec| spec.service() != "fun"));

    let smuggle = ToolSpec::parse("fun.tags", "偷偷", json!({})).unwrap();
    let closed = round_tools(std::slice::from_ref(&smuggle), fun_on(), &BTreeSet::new());
    assert!(closed.iter().all(|spec| spec.service() != "fun"));

    let on = round_tools(std::slice::from_ref(&smuggle), fun_on(), &opened);
    let names: Vec<_> = on
        .iter()
        .filter(|spec| spec.service() == "fun")
        .map(ToolSpec::name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["fun.tags", "fun.stars", "fun.learn", "fun.search"]
    );
    assert_eq!(
        on.iter()
            .filter(|spec| spec.wire_name() == "fun_tags")
            .count(),
        1
    );
    for spec in on.iter().filter(|spec| spec.service() == "fun") {
        for banned in WRITE_NAMES {
            assert!(
                !spec.wire_name().contains(banned),
                "{} contains {banned}",
                spec.wire_name()
            );
        }
        assert!(!spec.description().contains("偷偷"));
    }
    let learn = on.iter().find(|spec| spec.name() == "fun.learn").unwrap();
    assert_eq!(learn.service(), "fun");
    assert_eq!(learn.method(), "learn");
    assert_eq!(learn.wire_name(), "fun_learn");
    assert!(learn.parameters().get("required").is_none());
    assert_eq!(
        learn.parameters(),
        &json!({"type": "object", "properties": {"trigger": {"type": "string"}}})
    );
    let search = on.iter().find(|spec| spec.name() == "fun.search").unwrap();
    assert_eq!(search.method(), "search");
    assert_eq!(
        search.parameters(),
        &json!({"type": "object", "properties": {"q": {"type": "string"}}})
    );
    let tags = on.iter().find(|spec| spec.name() == "fun.tags").unwrap();
    assert_eq!(tags.parameters()["properties"], json!({}));
    let stars = on.iter().find(|spec| spec.name() == "fun.stars").unwrap();
    assert_eq!(stars.parameters()["properties"], json!({}));
}

#[test]
fn fun_scope_overwrites_the_group_and_strips_a_private_chat() {
    let group = MemoCallScope::from_source(Source::Group { id: 7 }, 3);
    let bound = bind_fun_scope(
        "fun",
        "learn",
        Some(&group),
        json!({"group_id": 99, "trigger": "猫"}),
    );
    assert_eq!(bound, json!({"group_id": 7, "trigger": "猫"}));
    let searched = bind_fun_scope("fun", "search", Some(&group), json!({"q": "猫"}));
    assert_eq!(searched, json!({"group_id": 7, "q": "猫"}));
    assert_eq!(
        bind_fun_scope("fun", "tags", Some(&group), json!({"group_id": 99})),
        json!({"group_id": 99})
    );

    let private = MemoCallScope::from_source(Source::Friend { id: 4 }, 9);
    let stripped = bind_fun_scope(
        "fun",
        "search",
        Some(&private),
        json!({"group_id": 7, "q": "猫"}),
    );
    assert_eq!(stripped, json!({"q": "猫"}));
    assert_eq!(
        bind_fun_scope(
            "fun",
            "learn",
            None,
            json!({"group_id": 7, "trigger": "猫"})
        ),
        json!({"group_id": 7, "trigger": "猫"})
    );
    assert_eq!(
        bind_fun_scope("rank", "list", Some(&group), json!({})),
        json!({})
    );
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

#[tokio::test(flavor = "current_thread")]
async fn closed_fun_does_not_call_the_hub() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let script = script_of(vec![one_call_with("call_fun", "fun_tags", "{}")]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(
        &client,
        &user("图库"),
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
    assert_eq!(script.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(catch.has(CLOSED_SKILL));
    assert!(!catch.has("agent tool unknown"));
    assert!(!tool_names(&script.bodies.lock().unwrap()[0])
        .iter()
        .any(|name| name.starts_with("fun_")));
}

#[tokio::test(flavor = "current_thread")]
async fn fun_learn_fills_the_current_group() {
    let script = script_of(vec![
        one_call_with("open", "skills_open", r#"{"name":"fun"}"#),
        one_call_with("call_fun", "fun_learn", r#"{"group_id":99,"trigger":"猫"}"#),
        one_call_with("call_search", "fun_search", r#"{"group_id":99,"q":"猫"}"#),
        saying("学话里有一条。"),
    ]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let scope = MemoCallScope::from_source(Source::Group { id: 7 }, 3);
    let pass = ToolPass {
        max_rounds: 3,
        scope: Some(&scope),
        memory: None,
        memory_unused_secs: 0,
        memory_max_entries: 0,
    };
    let reply = run_tool_loop(&client, &user("学话里有什么"), &[], fun_on(), &pass, &hub)
        .await
        .reply
        .unwrap();
    assert_eq!(reply, ToolReply::Text("学话里有一条。".into()));
    assert_eq!(
        hub.log.lock().unwrap().clone(),
        vec![
            (
                "fun".into(),
                "learn".into(),
                json!({"group_id": 7, "trigger": "猫"})
            ),
            (
                "fun".into(),
                "search".into(),
                json!({"group_id": 7, "q": "猫"})
            ),
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
            "fun_tags".to_string(),
            "fun_stars".to_string(),
            "fun_learn".to_string(),
            "fun_search".to_string(),
        ]
    );
    let learn = bodies[1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "fun_learn")
        .unwrap();
    assert_eq!(
        learn["parameters"],
        json!({"type": "object", "properties": {"trigger": {"type": "string"}}})
    );
    let search = bodies[2]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "fun_search")
        .unwrap();
    assert_eq!(
        search["parameters"],
        json!({"type": "object", "properties": {"q": {"type": "string"}}})
    );
}
