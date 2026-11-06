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
use crate::client::{ChatMessage, LlmClient, LlmError, Role};
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
    assert!(assemble_tools(&[], BuiltinTools::OFF).is_empty());

    let first = ToolSpec::parse("a.b_c", "one", json!({})).unwrap();
    let second = ToolSpec::parse("a_b.c", "two", json!({})).unwrap();
    assert_eq!(first.wire_name(), "a_b_c");
    let kept = assemble_tools(&[first, second], BuiltinTools::OFF);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].name(), "a.b_c");
    assert_eq!(kept[0].service(), "a");
    assert_eq!(kept[0].method(), "b_c");
}

#[test]
fn builtin_tools_follow_the_switches() {
    let on = assemble_tools(&[], BuiltinTools::ON);
    assert_eq!(
        on.iter().map(ToolSpec::wire_name).collect::<Vec<_>>(),
        vec![
            "rank_list",
            "rank_get",
            "contest_upcoming",
            "contest_today",
            "problem_today",
            "memo_list",
            "fun_tags",
            "fun_stars",
            "fun_learn",
            "fun_search",
            "memory_list",
            "memory_get",
            "memory_upsert",
            "memory_forget"
        ]
    );
    assert_eq!(on[0].service(), "rank");
    assert_eq!(on[0].method(), "list");
    assert_eq!(
        on[0].description(),
        "列出本群 Codeforces 榜上的队员，含 handle 和 rating。不需要参数。"
    );
    assert_eq!(on[0].parameters(), &default_parameters());
    assert_eq!(on[1].service(), "rank");
    assert_eq!(on[1].method(), "get");
    assert_eq!(on[1].wire_name(), "rank_get");
    assert_eq!(
        on[1].description(),
        "按 Codeforces handle 查一位已登记队员。可选字符串参数 handle。不传则这次查询失败。"
    );
    assert_eq!(
        on[1].parameters(),
        &json!({
            "type": "object",
            "properties": { "handle": { "type": "string" } }
        })
    );
    assert!(on[1].parameters().get("required").is_none());
    assert_eq!(on[2].service(), "contest");
    assert_eq!(on[2].method(), "upcoming");
    assert_eq!(
        on[2].description(),
        "列出即将开始的比赛。可选字符串参数 oj，例如 Codeforces 或 Atcoder。不传则返回全部。"
    );
    assert_eq!(
        on[2].parameters(),
        &json!({
            "type": "object",
            "properties": { "oj": { "type": "string" } }
        })
    );
    assert!(on[2].parameters().get("required").is_none());
    assert_eq!(on[3].method(), "today");
    assert_eq!(on[3].description(), "列出今天开始的比赛。不需要参数。");
    assert_eq!(on[3].parameters(), &default_parameters());
    assert_eq!(on[4].service(), "problem");
    assert_eq!(on[4].method(), "today");
    assert_eq!(on[4].wire_name(), "problem_today");
    assert_eq!(
        on[4].description(),
        "返回当前每日一题里已经保存的题目。不需要参数。还没保存时是空数组。"
    );
    assert_eq!(on[4].parameters(), &default_parameters());
    assert_eq!(on[5].service(), "memo");
    assert_eq!(on[5].method(), "list");
    assert_eq!(on[5].wire_name(), "memo_list");
    assert_eq!(
        on[5].description(),
        "列出当前对话已经记下的备忘。不需要参数。还没有备忘时是空数组。"
    );
    assert_eq!(on[5].parameters(), &default_parameters());
    assert!(on.iter().all(|spec| spec.name() != "memo.add"));

    let rank_only = assemble_tools(
        &[],
        BuiltinTools {
            rank: true,
            ..BuiltinTools::OFF
        },
    );
    assert_eq!(
        rank_only
            .iter()
            .map(ToolSpec::wire_name)
            .collect::<Vec<_>>(),
        vec!["rank_list", "rank_get"]
    );
    let contest_only = assemble_tools(
        &[],
        BuiltinTools {
            contest: true,
            ..BuiltinTools::OFF
        },
    );
    assert_eq!(
        contest_only
            .iter()
            .map(ToolSpec::wire_name)
            .collect::<Vec<_>>(),
        vec!["contest_upcoming", "contest_today"]
    );
    let problem_only = assemble_tools(
        &[],
        BuiltinTools {
            problem: true,
            ..BuiltinTools::OFF
        },
    );
    assert_eq!(
        problem_only
            .iter()
            .map(ToolSpec::wire_name)
            .collect::<Vec<_>>(),
        vec!["problem_today"]
    );
    assert!(assemble_tools(&[], BuiltinTools::OFF).is_empty());

    let extra = ToolSpec::parse("memo.search", "查备忘", json!({})).unwrap();
    let with_extra = assemble_tools(&[extra], BuiltinTools::ON);
    assert_eq!(with_extra[14].name(), "memo.search");
    assert_eq!(with_extra[14].wire_name(), "memo_search");

    let write = assemble_tools(
        &[],
        BuiltinTools {
            memo: true,
            memo_write: true,
            ..BuiltinTools::OFF
        },
    );
    assert_eq!(
        write.iter().map(ToolSpec::wire_name).collect::<Vec<_>>(),
        vec!["memo_list", "memo_add"]
    );
    assert_eq!(write[1].service(), "memo");
    assert_eq!(write[1].method(), "add");
    assert_eq!(
        write[1].parameters(),
        &json!({
            "type": "object",
            "properties": { "body": { "type": "string" } }
        })
    );
    let list_only = assemble_tools(
        &[],
        BuiltinTools {
            memo_write: true,
            ..BuiltinTools::OFF
        },
    );
    assert!(list_only.is_empty());

    let configured = ToolSpec::parse(
        "rank.list",
        "别的说明",
        json!({"type": "object", "properties": {"limit": {"type": "integer"}}}),
    )
    .unwrap();
    let kept_builtin = assemble_tools(std::slice::from_ref(&configured), BuiltinTools::ON);
    assert_eq!(kept_builtin.len(), 14);
    assert_eq!(kept_builtin[0].description(), on[0].description());
    assert_eq!(kept_builtin[0].parameters(), on[0].parameters());

    let kept_config = assemble_tools(&[configured], BuiltinTools::OFF);
    assert_eq!(kept_config.len(), 1);
    assert_eq!(kept_config[0].description(), "别的说明");
    assert_eq!(kept_config[0].service(), "rank");
    assert_eq!(kept_config[0].method(), "list");
}

#[test]
fn catalog_lists_logical_names_for_the_enabled_set() {
    let on = assemble_tools(&[], BuiltinTools::ON);
    let text = tools_command_text(&on);
    assert_eq!(
        text,
        "可用工具：\n\
rank.list：列出本群 Codeforces 榜上的队员，含 handle 和 rating。不需要参数。\n\
rank.get：按 Codeforces handle 查一位已登记队员。可选字符串参数 handle。不传则这次查询失败。\n\
contest.upcoming：列出即将开始的比赛。可选字符串参数 oj，例如 Codeforces 或 Atcoder。不传则返回全部。\n\
contest.today：列出今天开始的比赛。不需要参数。\n\
problem.today：返回当前每日一题里已经保存的题目。不需要参数。还没保存时是空数组。\n\
memo.list：列出当前对话已经记下的备忘。不需要参数。还没有备忘时是空数组。\n\
fun.tags：列出图库的 tag，含条数和别名。旧专辑也在这份清单里。不需要参数。还没有 tag 时是空数组。\n\
fun.stars：列出收藏夹的名称和链接。不需要参数。还没有收藏时是空数组。\n\
fun.learn：列出当前会话的学话。可选字符串 trigger，只返回这个触发词。群里看本群，私聊看全局。还没有时是空数组。\n\
fun.search：在 tag、收藏和当前会话的学话里按原文查找。字符串参数 q。q 为空时这次调用失败。\n\
memory.list：列出分层记忆的键。可选字符串 scope：account、group 或 user。不传时群里看账号、本群和当前发言人，私聊看账号和当前发言人。还没有时是空数组。\n\
memory.get：读取一条分层记忆。字符串 scope 和 key。没有这条时结果是 null。\n\
memory.upsert：写入或覆盖一条分层记忆。字符串 scope、key、value。value 最多 500 个字。user 只写当前发言人，group 只写当前群。\n\
memory.forget：删除分层记忆。字符串 scope 和 key。可选布尔 prefix 为 true 时按前缀删除。"
    );
    assert!(!text.contains('_'));
    assert_eq!(tools_command_text(&[]), "当前没有可用工具。");

    let shadowed = ToolSpec::parse("rank.list", "别的说明", json!({})).unwrap();
    let kept = assemble_tools(std::slice::from_ref(&shadowed), BuiltinTools::ON);
    let shown = tools_command_text(&kept);
    assert!(shown.contains("rank.list：列出本群"));
    assert!(!shown.contains("别的说明"));

    let custom = assemble_tools(&[shadowed], BuiltinTools::OFF);
    assert_eq!(
        tools_command_text(&custom),
        "可用工具：\nrank.list：别的说明"
    );

    let blank = ToolSpec::parse("memo.list", " ", json!({})).unwrap();
    assert_eq!(tools_command_text(&[blank]), "可用工具：\nmemo.list");

    assert_eq!(instructions_with_catalog("人设", &on, false), "人设");
    let injected = instructions_with_catalog("人设", &on, true);
    assert!(injected.starts_with("人设\n\n可用工具：\nrank.list："));
    assert!(!injected.contains('_'));
    assert_eq!(instructions_with_catalog("人设", &[], true), "人设");
    assert_eq!(
        instructions_with_catalog(" \n ", &custom, true),
        "可用工具：\nrank.list：别的说明"
    );
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
            images: Vec::new(),
        },
        ChatMessage {
            role: Role::User,
            content: "最近有比赛吗".into(),
            images: Vec::new(),
        },
    ];
    let reply = reply_of(&client, &messages, &tools, BuiltinTools::OFF, 3, &hub)
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
    let reply = reply_of(&client, &user("再查"), &tools, BuiltinTools::OFF, 1, &hub)
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
    let reply = reply_of(&client, &user("再查"), &tools, BuiltinTools::OFF, 0, &hub)
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
    let reply = reply_of(&client, &user("榜"), &tools, BuiltinTools::OFF, 3, &hub)
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
    let reply = reply_of(&client, &user("榜"), &tools, BuiltinTools::OFF, 3, &hub)
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
    let reply = reply_of(&client, &user("榜"), &tools, BuiltinTools::OFF, 3, &hub)
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
    let reply = reply_of(&client, &user("在吗"), &[], BuiltinTools::OFF, 3, &hub)
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
    let err = reply_of(&client, &user("榜"), &tools, BuiltinTools::OFF, 3, &hub)
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Decode(_)), "{err}");
    assert!(hub.log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn builtin_rank_and_contest_calls_return_the_model_text() {
    let script = Script::new(Kind::ThreeThenText);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = reply_of(
        &client,
        &user("今天有比赛吗，榜怎么样"),
        &[],
        BuiltinTools::ON,
        3,
        &hub,
    )
    .await
    .unwrap();
    assert_eq!(
        reply,
        ToolReply::Text("今天 Codeforces 一场。榜上 12 人。".into())
    );
    assert_eq!(
        hub.log.lock().unwrap().clone(),
        vec![
            ("rank".into(), "list".into(), json!({})),
            (
                "contest".into(),
                "upcoming".into(),
                json!({"oj": "Codeforces"})
            ),
            ("contest".into(), "today".into(), json!({})),
        ]
    );
    let bodies = script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    let names: Vec<_> = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "rank_list",
            "rank_get",
            "contest_upcoming",
            "contest_today",
            "problem_today",
            "memo_list",
            "fun_tags",
            "fun_stars",
            "fun_learn",
            "fun_search",
            "memory_list",
            "memory_get",
            "memory_upsert",
            "memory_forget"
        ]
    );
    assert_eq!(
        bodies[0]["tools"][0]["description"],
        "列出本群 Codeforces 榜上的队员，含 handle 和 rating。不需要参数。"
    );
    assert_eq!(bodies[1]["input"][2]["output"], r#"{"via":"rank.list"}"#);
    assert_eq!(
        bodies[1]["input"][4]["output"],
        r#"{"via":"contest.upcoming"}"#
    );
    assert_eq!(
        bodies[1]["input"][6]["output"],
        r#"{"via":"contest.today"}"#
    );
}

#[tokio::test(flavor = "current_thread")]
async fn builtin_contest_today_error_uses_the_degraded_reply() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let script = Script::new(Kind::TodayCall);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec {
        log: Mutex::new(Vec::new()),
        fail: true,
    };
    let reply = reply_of(
        &client,
        &user("今天有比赛吗"),
        &[],
        BuiltinTools::ON,
        3,
        &hub,
    )
    .await
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    let log = hub.log.lock().unwrap().clone();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].0, "contest");
    assert_eq!(log[0].1, "today");
    assert_eq!(script.hits.load(Ordering::SeqCst), 1);
    assert!(catch.has("agent tool"));
}

#[tokio::test]
async fn builtin_rank_get_and_problem_today_round_trip() {
    let script = Script::new(Kind::GetAndToday);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let reply = reply_of(
        &client,
        &user("tourist 多少分，今天的题是什么"),
        &[],
        BuiltinTools::ON,
        3,
        &hub,
    )
    .await
    .unwrap();
    assert_eq!(
        reply,
        ToolReply::Text("tourist 是 3000。今天有一题。".into())
    );
    assert_eq!(
        hub.log.lock().unwrap().clone(),
        vec![
            ("rank".into(), "get".into(), json!({"handle": "tourist"})),
            ("problem".into(), "today".into(), json!({})),
        ]
    );
    let bodies = script.bodies.lock().unwrap().clone();
    assert_eq!(bodies[1]["input"][2]["output"], r#"{"via":"rank.get"}"#);
    assert_eq!(
        bodies[1]["input"][4]["output"],
        r#"{"via":"problem.today"}"#
    );
}

#[tokio::test(flavor = "current_thread")]
async fn builtin_problem_today_error_uses_the_degraded_reply() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let script = Script::new(Kind::ProblemCall);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec {
        log: Mutex::new(Vec::new()),
        fail: true,
    };
    let reply = reply_of(&client, &user("今天的题"), &[], BuiltinTools::ON, 3, &hub)
        .await
        .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    let log = hub.log.lock().unwrap().clone();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].0, "problem");
    assert_eq!(log[0].1, "today");
    assert_eq!(script.hits.load(Ordering::SeqCst), 1);
    assert!(catch.has("agent tool"));
}

#[tokio::test]
async fn disabled_builtin_group_stays_out_of_the_request() {
    let script = Script::new(Kind::Text);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let hub = Rec::default();
    let extra = vec![ToolSpec::parse("memo.search", "查备忘", json!({})).unwrap()];
    let reply = reply_of(
        &client,
        &user("今天比赛"),
        &extra,
        BuiltinTools {
            contest: true,
            ..BuiltinTools::OFF
        },
        3,
        &hub,
    )
    .await
    .unwrap();
    assert_eq!(reply, ToolReply::Text("本周有两场".into()));
    let bodies = script.bodies.lock().unwrap();
    let names: Vec<_> = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "contest_upcoming".to_string(),
            "contest_today".to_string(),
            "memo_search".to_string(),
        ]
    );
    assert!(hub.log.lock().unwrap().is_empty());
}

async fn reply_of(
    client: &LlmClient,
    messages: &[ChatMessage],
    configured: &[ToolSpec],
    builtins: BuiltinTools,
    max_rounds: u32,
    hub: &Rec,
) -> Result<ToolReply, LlmError> {
    let pass = ToolPass::rounds(max_rounds);
    run_tool_loop(client, messages, configured, builtins, &pass, hub)
        .await
        .reply
}

#[tokio::test]
async fn spend_sums_token_rounds_and_still_counts_a_body_without_usage() {
    let script = Script::new(Kind::Metered);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let tools = vec![ToolSpec::parse("rank.list", "当前榜", json!({})).unwrap()];
    let hub = Rec::default();
    let pass = ToolPass::rounds(3);
    let turn = run_tool_loop(&client, &user("榜"), &tools, BuiltinTools::OFF, &pass, &hub).await;
    assert_eq!(turn.reply.unwrap(), ToolReply::Text("好".into()));
    assert_eq!(turn.calls, 2);
    assert_eq!(turn.in_tokens, 4);
    assert_eq!(turn.out_tokens, 1);

    let script = Script::new(Kind::MissingId);
    let base = spawn_app(script).await;
    let client = test_client(&base);
    let pass = ToolPass::rounds(3);
    let turn = run_tool_loop(&client, &user("榜"), &tools, BuiltinTools::OFF, &pass, &hub).await;
    assert!(matches!(turn.reply, Err(LlmError::Decode(_))));
    assert_eq!(turn.calls, 1);
    assert_eq!(turn.in_tokens, 0);
    assert_eq!(turn.out_tokens, 0);
}

fn user(text: &str) -> Vec<ChatMessage> {
    vec![ChatMessage {
        role: Role::User,
        content: text.into(),
        images: Vec::new(),
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
    ThreeThenText,
    TodayCall,
    GetAndToday,
    ProblemCall,
    Metered,
    MemoList,
    FunLearn,
    MemoryUpsert,
    MemoryBad,
}

#[derive(Clone)]
struct Script {
    pub(super) hits: Arc<AtomicUsize>,
    pub(super) bodies: Arc<Mutex<Vec<Value>>>,
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
        Kind::ThreeThenText if n == 0 => three_calls(),
        Kind::ThreeThenText => saying("今天 Codeforces 一场。榜上 12 人。"),
        Kind::TodayCall => one_call_with("call_today", "contest_today", "{}"),
        Kind::GetAndToday if n == 0 => get_and_today(),
        Kind::GetAndToday => saying("tourist 是 3000。今天有一题。"),
        Kind::ProblemCall => one_call_with("call_problem", "problem_today", "{}"),
        Kind::Metered if n == 0 => json!({
            "output": [{
                "type": "function_call",
                "call_id": "call_rank",
                "name": "rank_list",
                "arguments": "{}"
            }],
            "usage": {"input_tokens": 4, "output_tokens": 1, "total_tokens": 5}
        }),
        Kind::Metered => json!({
            "output_text": "好",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "好"}]
            }]
        }),
        Kind::MemoList if n == 0 => one_call_with("call_memo", "memo_list", "{}"),
        Kind::MemoList => saying("这个对话有一条备忘。"),
        Kind::FunLearn if n == 0 => {
            one_call_with("call_fun", "fun_learn", r#"{"group_id":99,"trigger":"猫"}"#)
        }
        Kind::FunLearn => saying("学话里有一条。"),
        Kind::MemoryUpsert if n == 0 => one_call_with(
            "call_mem",
            "memory_upsert",
            r#"{"scope":"user","user_id":1,"scope_id":"1","group_id":1,"key":"nick","value":"小明"}"#,
        ),
        Kind::MemoryUpsert => saying("记下了"),
        Kind::MemoryBad => one_call_with(
            "call_mem",
            "memory_upsert",
            r#"{"scope":"user","key":"","value":"x"}"#,
        ),
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
    saying("本周有两场")
}

fn saying(text: &str) -> Value {
    json!({
        "output_text": text,
        "output": [{
            "type": "message",
            "content": [{"type": "output_text", "text": "NO"}]
        }]
    })
}

fn three_calls() -> Value {
    json!({
        "output": [
            {
                "type": "function_call",
                "call_id": "call_rank",
                "name": "rank_list",
                "arguments": "{}"
            },
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
    })
}

fn get_and_today() -> Value {
    json!({
        "output": [
            {
                "type": "function_call",
                "call_id": "call_get",
                "name": "rank_get",
                "arguments": "{\"handle\":\"tourist\"}"
            },
            {
                "type": "function_call",
                "call_id": "call_problem",
                "name": "problem_today",
                "arguments": "{}"
            }
        ]
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
    pub(super) log: Mutex<Vec<(String, String, Value)>>,
    pub(super) fail: bool,
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

#[path = "fun_tests.rs"]
mod fun_tests;
#[path = "memo_tests.rs"]
mod memo_tests;
#[path = "memory_tests.rs"]
mod memory_tests;
