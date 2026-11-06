use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::{json, Value};

use super::super::skills::{self, round_tools, CLOSED_SKILL, UNKNOWN_SKILL};
use super::super::{
    assemble_tools, resident_tools, run_tool_loop, BuiltinTools, ToolPass, ToolReply, ToolSpec,
    TOOL_CAP_REPLY, TOOL_FAIL_REPLY,
};
use super::{one_call_with, saying, spawn_app, test_client, user, Catch, Kind, Rec, Script};

fn wires(tools: &[ToolSpec]) -> Vec<&str> {
    tools.iter().map(ToolSpec::wire_name).collect()
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

fn outputs(body: &Value) -> Vec<String> {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| item["output"].as_str().unwrap().to_string())
        .collect()
}

fn exec_off(
    wire: &str,
    args: &Value,
    now: &BTreeSet<String>,
    opened: &mut BTreeSet<String>,
) -> Option<skills::SkillOut> {
    skills::exec(wire, args, BuiltinTools::OFF, now, opened)
}

fn ready(step: Option<skills::SkillOut>) -> Result<Value, &'static str> {
    match step {
        Some(skills::SkillOut::Ready(result)) => result,
        Some(skills::SkillOut::Qq) => panic!("qq"),
        Some(skills::SkillOut::Schedule) => panic!("schedule"),
        None => panic!("none"),
    }
}

fn script_of(steps: Vec<Value>) -> Script {
    let mut script = Script::new(Kind::Text);
    script.steps = Arc::new(steps);
    script
}

#[test]
fn resident_meta_tools_stay_on_and_pack_tools_stay_closed() {
    assert!(assemble_tools(&[], BuiltinTools::OFF).is_empty());
    let off = resident_tools(&[], BuiltinTools::OFF);
    assert_eq!(wires(&off), vec!["skills_list", "skills_open"]);
    assert_eq!(off[0].name(), "skills.list");
    assert_eq!(off[0].service(), "skills");
    assert_eq!(off[0].method(), "list");
    assert_eq!(off[1].name(), "skills.open");
    assert_eq!(off[1].method(), "open");
    assert!(off[1].parameters()["properties"].get("name").is_some());
    assert!(off.iter().all(|spec| spec.name() != "ping.pong"));

    let on = resident_tools(&[], BuiltinTools::ON);
    assert_eq!(on.len(), 11);
    assert_eq!(on[0].name(), "rank.list");
    assert_eq!(on[on.len() - 2].wire_name(), "skills_list");
    assert_eq!(on.last().unwrap().wire_name(), "skills_open");
    assert!(on.iter().any(|spec| spec.name() == "memo.list"));
    assert!(on.iter().all(|spec| spec.name() != "memo.add"));
    assert!(on.iter().all(|spec| spec.name() != "ping.pong"));
    assert!(on
        .iter()
        .all(|spec| !spec.name().starts_with("fun.") && !spec.name().starts_with("contest.")));

    let custom = ToolSpec::parse("skills.list", "别的说明", json!({})).unwrap();
    let smuggle = ToolSpec::parse("ping.pong", "偷偷", json!({})).unwrap();
    let hidden = round_tools(
        &[custom, smuggle.clone()],
        BuiltinTools::OFF,
        &BTreeSet::new(),
    );
    assert_eq!(wires(&hidden), vec!["skills_list", "skills_open"]);
    assert!(hidden[0].description().contains("技能包"));
    assert!(!hidden
        .iter()
        .any(|spec| spec.description().contains("偷偷")));
    assert!(!hidden
        .iter()
        .any(|spec| spec.description().contains("别的说明")));

    let mut opened = BTreeSet::from(["ping".to_string()]);
    let shown = round_tools(std::slice::from_ref(&smuggle), BuiltinTools::OFF, &opened);
    assert_eq!(
        wires(&shown),
        vec!["skills_list", "skills_open", "ping_pong"]
    );
    let pong = shown
        .iter()
        .find(|spec| spec.wire_name() == "ping_pong")
        .unwrap();
    assert_eq!(pong.name(), "ping.pong");
    assert_eq!(pong.service(), "ping");
    assert_eq!(pong.method(), "pong");
    assert!(!pong.description().contains("偷偷"));
    opened.insert("ping".to_string());
    let again = round_tools(&[], BuiltinTools::OFF, &opened);
    assert_eq!(
        again
            .iter()
            .filter(|spec| spec.wire_name() == "ping_pong")
            .count(),
        1
    );
}

#[test]
fn list_open_unknown_and_closed_dispatch() {
    let mut opened = BTreeSet::new();
    let now = opened.clone();
    let listed = ready(exec_off("skills_list", &json!({}), &now, &mut opened)).unwrap();
    assert!(opened.is_empty());
    let row = &listed[0];
    assert_eq!(row["name"], "ping");
    assert!(!row["description"].as_str().unwrap().is_empty());
    assert!(row.get("parameters").is_none());
    assert!(row.get("tools").is_none());
    assert_eq!(row.as_object().unwrap().len(), 2);
    let raw = serde_json::to_string(&listed).unwrap();
    assert!(!raw.contains("ping.pong"));
    assert!(!raw.contains("ping_pong"));
    assert!(!raw.contains("parameters"));

    for bad in [
        json!({"name": "nope"}),
        json!({}),
        json!({"name": "  "}),
        json!({"name": 1}),
        json!("ping"),
        json!(null),
    ] {
        let now = opened.clone();
        let err = ready(exec_off("skills_open", &bad, &now, &mut opened)).unwrap_err();
        assert_eq!(err, UNKNOWN_SKILL);
        assert!(opened.is_empty());
    }

    let now = opened.clone();
    let closed = ready(exec_off("ping_pong", &json!({}), &now, &mut opened)).unwrap_err();
    assert_eq!(closed, CLOSED_SKILL);
    assert!(opened.is_empty());

    let now = opened.clone();
    let opened_ok = ready(exec_off(
        "skills_open",
        &json!({"name": " ping "}),
        &now,
        &mut opened,
    ))
    .unwrap();
    assert_eq!(opened_ok, json!({"name": "ping", "opened": true}));
    let now = opened.clone();
    let again = ready(exec_off(
        "skills_open",
        &json!({"name": "ping"}),
        &now,
        &mut opened,
    ))
    .unwrap();
    assert_eq!(again, opened_ok);
    assert_eq!(
        opened.iter().map(String::as_str).collect::<Vec<_>>(),
        ["ping"]
    );

    let now = opened.clone();
    let pong = ready(exec_off("ping_pong", &json!({}), &now, &mut opened)).unwrap();
    assert_eq!(pong, json!({"pong": true}));

    let mut sink = BTreeSet::new();
    assert!(exec_off("rank_list", &json!({}), &BTreeSet::new(), &mut sink).is_none());
}

#[tokio::test]
async fn list_then_open_then_pong_stays_inside_one_turn() {
    let script = script_of(vec![
        one_call_with("call_list", "skills_list", "{}"),
        json!({
            "output": [
                {
                    "type": "function_call",
                    "call_id": "open_a",
                    "name": "skills_open",
                    "arguments": "{\"name\":\"ping\"}"
                },
                {
                    "type": "function_call",
                    "call_id": "open_b",
                    "name": "skills_open",
                    "arguments": "{\"name\":\" ping \"}"
                }
            ]
        }),
        one_call_with("call_pong", "ping_pong", "{}"),
        saying("好"),
    ]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let pass = ToolPass::rounds(3);
    let reply = run_tool_loop(
        &client,
        &user("打开占位技能"),
        &[],
        BuiltinTools::OFF,
        &pass,
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Text("好".into()));
    assert!(hub.log.lock().unwrap().is_empty());
    assert_eq!(script.hits.load(Ordering::SeqCst), 4);
    let bodies = script.bodies.lock().unwrap().clone();
    assert_eq!(
        tool_names(&bodies[0]),
        vec!["skills_list".to_string(), "skills_open".to_string()]
    );
    assert!(!tool_names(&bodies[1])
        .iter()
        .any(|name| name == "ping_pong"));
    assert_eq!(
        tool_names(&bodies[2])
            .iter()
            .filter(|name| *name == "ping_pong")
            .count(),
        1
    );
    assert_eq!(
        tool_names(&bodies[3])
            .iter()
            .filter(|name| *name == "ping_pong")
            .count(),
        1
    );
    let listed: Value = serde_json::from_str(&outputs(&bodies[2])[0]).unwrap();
    assert_eq!(listed[0]["name"], "ping");
    assert!(listed[0].get("parameters").is_none());
    assert_eq!(outputs(&bodies[2])[1], r#"{"name":"ping","opened":true}"#);
    assert_eq!(outputs(&bodies[2])[1], outputs(&bodies[2])[2]);
    assert_eq!(outputs(&bodies[3]).last().unwrap(), r#"{"pong":true}"#);
}

#[tokio::test(flavor = "current_thread")]
async fn opened_pack_does_not_leak_into_the_next_turn() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let first = script_of(vec![
        one_call_with("open_1", "skills_open", r#"{"name":"ping"}"#),
        one_call_with("pong_1", "ping_pong", "{}"),
        saying("好"),
    ]);
    let base = spawn_app(first.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let pass = ToolPass::rounds(3);
    let reply = run_tool_loop(
        &client,
        &user("先打开"),
        &[],
        BuiltinTools::OFF,
        &pass,
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Text("好".into()));
    assert!(tool_names(&first.bodies.lock().unwrap()[1])
        .iter()
        .any(|name| name == "ping_pong"));

    let second = script_of(vec![
        one_call_with("pong_2", "ping_pong", "{}"),
        saying("不该到"),
    ]);
    let base = spawn_app(second.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(
        &client,
        &user("下一句"),
        &[],
        BuiltinTools::OFF,
        &pass,
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert!(hub.log.lock().unwrap().is_empty());
    assert_eq!(second.hits.load(Ordering::SeqCst), 1);
    assert!(!tool_names(&second.bodies.lock().unwrap()[0])
        .iter()
        .any(|name| name == "ping_pong"));
    assert!(catch.has(CLOSED_SKILL));
    assert!(!catch.has("agent tool unknown"));
}

#[tokio::test(flavor = "current_thread")]
async fn same_response_cannot_call_the_tool_it_just_opened() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let script = script_of(vec![
        json!({
            "output": [
                {
                    "type": "function_call",
                    "call_id": "open_same",
                    "name": "skills_open",
                    "arguments": "{\"name\":\"ping\"}"
                },
                {
                    "type": "function_call",
                    "call_id": "pong_same",
                    "name": "ping_pong",
                    "arguments": "{}"
                }
            ]
        }),
        saying("不该到"),
    ]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(
        &client,
        &user("同一回合"),
        &[],
        BuiltinTools::OFF,
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
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_skill_fails_the_turn() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let script = script_of(vec![
        one_call_with("open_x", "skills_open", r#"{"name":"nope"}"#),
        saying("不该到"),
    ]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(
        &client,
        &user("没有这个包"),
        &[],
        BuiltinTools::OFF,
        &ToolPass::rounds(3),
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert!(hub.log.lock().unwrap().is_empty());
    assert_eq!(script.hits.load(Ordering::SeqCst), 1);
    assert!(catch.has(UNKNOWN_SKILL));
    assert!(!catch.has(CLOSED_SKILL));
}

#[tokio::test]
async fn skill_wire_names_go_out_on_a_text_turn() {
    let script = Script::new(Kind::Text);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(
        &client,
        &user("在吗"),
        &[],
        BuiltinTools::OFF,
        &ToolPass::rounds(3),
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Text("本周有两场".into()));
    assert!(hub.log.lock().unwrap().is_empty());
    assert_eq!(script.hits.load(Ordering::SeqCst), 1);
    let body = &script.bodies.lock().unwrap()[0];
    assert_eq!(
        tool_names(body),
        vec!["skills_list".to_string(), "skills_open".to_string()]
    );
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "skills_list");
    assert!(!body["tools"][0]["description"].as_str().unwrap().is_empty());
    assert_eq!(body["tools"][1]["name"], "skills_open");
    assert!(body["tools"][1]["parameters"]["properties"]
        .get("name")
        .is_some());
    assert!(body.get("previous_response_id").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn opening_counts_toward_max_tool_rounds() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let script = script_of(vec![
        one_call_with("open_cap", "skills_open", r#"{"name":"ping"}"#),
        one_call_with("pong_cap", "ping_pong", "{}"),
        saying("不该到"),
    ]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = run_tool_loop(
        &client,
        &user("只准一轮"),
        &[],
        BuiltinTools::OFF,
        &ToolPass::rounds(1),
        &hub,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_CAP_REPLY));
    assert!(hub.log.lock().unwrap().is_empty());
    assert_eq!(script.hits.load(Ordering::SeqCst), 2);
    assert!(catch.has("agent tool round cap"));
    assert!(tool_names(&script.bodies.lock().unwrap()[1])
        .iter()
        .any(|name| name == "ping_pong"));
}

fn pack_names(builtins: BuiltinTools) -> Vec<String> {
    let mut opened = BTreeSet::new();
    let now = opened.clone();
    let listed = ready(skills::exec(
        "skills_list",
        &json!({}),
        builtins,
        &now,
        &mut opened,
    ))
    .unwrap();
    listed
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn fun_and_contest_packs_follow_the_flags() {
    assert_eq!(
        pack_names(BuiltinTools::OFF),
        vec!["ping", "qq.read", "schedule"]
    );
    assert_eq!(
        pack_names(BuiltinTools::ON),
        vec!["ping", "qq.read", "schedule", "fun", "contest"]
    );
    assert_eq!(
        pack_names(BuiltinTools {
            fun: true,
            ..BuiltinTools::OFF
        }),
        vec!["ping", "qq.read", "schedule", "fun"]
    );
    assert_eq!(
        pack_names(BuiltinTools {
            contest: true,
            ..BuiltinTools::OFF
        }),
        vec!["ping", "qq.read", "schedule", "contest"]
    );

    let mut opened = BTreeSet::new();
    let now = opened.clone();
    let unknown = ready(skills::exec(
        "skills_open",
        &json!({"name": "fun"}),
        BuiltinTools::OFF,
        &now,
        &mut opened,
    ))
    .unwrap_err();
    assert_eq!(unknown, UNKNOWN_SKILL);
    assert!(opened.is_empty());
    let unknown = ready(skills::exec(
        "skills_open",
        &json!({"name": "contest"}),
        BuiltinTools {
            fun: true,
            ..BuiltinTools::OFF
        },
        &now,
        &mut opened,
    ))
    .unwrap_err();
    assert_eq!(unknown, UNKNOWN_SKILL);
    assert!(opened.is_empty());

    let closed = ready(skills::exec(
        "fun_tags",
        &json!({}),
        BuiltinTools::ON,
        &now,
        &mut opened,
    ))
    .unwrap_err();
    assert_eq!(closed, CLOSED_SKILL);
    let closed = ready(skills::exec(
        "contest_upcoming",
        &json!({"oj": "Codeforces"}),
        BuiltinTools::ON,
        &now,
        &mut opened,
    ))
    .unwrap_err();
    assert_eq!(closed, CLOSED_SKILL);
    assert!(opened.is_empty());

    let forced = BTreeSet::from(["fun".to_string(), "contest".to_string()]);
    let hidden = round_tools(&[], BuiltinTools::OFF, &forced);
    assert!(hidden
        .iter()
        .all(|spec| { !spec.name().starts_with("fun.") && !spec.name().starts_with("contest.") }));
    let blocked = ready(skills::exec(
        "fun_search",
        &json!({"q": "猫"}),
        BuiltinTools::OFF,
        &forced,
        &mut BTreeSet::new(),
    ))
    .unwrap_err();
    assert_eq!(blocked, CLOSED_SKILL);
    let blocked = ready(skills::exec(
        "contest_today",
        &json!({}),
        BuiltinTools {
            fun: true,
            ..BuiltinTools::OFF
        },
        &forced,
        &mut BTreeSet::new(),
    ))
    .unwrap_err();
    assert_eq!(blocked, CLOSED_SKILL);

    let mut opened = BTreeSet::new();
    let now = opened.clone();
    let opened_fun = ready(skills::exec(
        "skills_open",
        &json!({"name": " fun "}),
        BuiltinTools::ON,
        &now,
        &mut opened,
    ))
    .unwrap();
    assert_eq!(opened_fun, json!({"name": "fun", "opened": true}));
    let now = opened.clone();
    assert!(skills::exec(
        "fun_search",
        &json!({"q": "猫"}),
        BuiltinTools::ON,
        &now,
        &mut opened
    )
    .is_none());
    let now = opened.clone();
    let opened_contest = ready(skills::exec(
        "skills_open",
        &json!({"name": "contest"}),
        BuiltinTools::ON,
        &now,
        &mut opened,
    ))
    .unwrap();
    assert_eq!(opened_contest, json!({"name": "contest", "opened": true}));
    let now = opened.clone();
    assert!(skills::exec(
        "contest_today",
        &json!({}),
        BuiltinTools::ON,
        &now,
        &mut opened
    )
    .is_none());

    let mut sink = BTreeSet::new();
    let listed = ready(skills::exec(
        "skills_list",
        &json!({}),
        BuiltinTools::ON,
        &BTreeSet::new(),
        &mut sink,
    ))
    .unwrap();
    for name in ["fun", "contest"] {
        let row = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap();
        assert_eq!(row.as_object().unwrap().len(), 2);
        assert!(!row["description"].as_str().unwrap().is_empty());
    }
    let raw = serde_json::to_string(&listed).unwrap();
    assert!(!raw.contains("fun.tags"));
    assert!(!raw.contains("fun_tags"));
    assert!(!raw.contains("contest.upcoming"));
    assert!(!raw.contains("contest_upcoming"));
}
