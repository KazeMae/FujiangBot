use fujiang_core::Source;
use serde_json::json;

use super::super::{
    assemble_tools, bind_fun_scope, run_tool_loop, BuiltinTools, MemoCallScope, ToolPass,
    ToolReply, ToolSpec,
};
use super::{spawn_app, test_client, user, Kind, Rec, Script};

const WRITE_NAMES: &[&str] = &[
    "delete", "del", "retire", "add", "set", "merge", "alias", "attach", "detach", "album",
];

#[test]
fn fun_reads_register_and_writes_stay_out() {
    let on = assemble_tools(&[], BuiltinTools::ON);
    let names: Vec<_> = on.iter().map(ToolSpec::name).collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "rank.list",
            "rank.get",
            "contest.upcoming",
            "contest.today",
            "problem.today",
            "memo.list",
            "fun.tags",
            "fun.stars",
            "fun.learn",
            "fun.search",
            "memory.list",
            "memory.get",
            "memory.upsert",
            "memory.forget",
            "memory.consolidate",
        ]
    );
    for name in on
        .iter()
        .filter(|spec| spec.service() == "fun")
        .map(ToolSpec::wire_name)
    {
        for banned in WRITE_NAMES {
            assert!(!name.contains(banned), "{name} contains {banned}");
        }
    }
    let learn = on.iter().find(|spec| spec.name() == "fun.learn").unwrap();
    assert_eq!(learn.service(), "fun");
    assert_eq!(learn.method(), "learn");
    assert_eq!(learn.wire_name(), "fun_learn");
    assert!(learn.parameters().get("required").is_none());
    let search = on.iter().find(|spec| spec.name() == "fun.search").unwrap();
    assert_eq!(search.method(), "search");
    assert_eq!(
        search.parameters(),
        &json!({"type": "object", "properties": {"q": {"type": "string"}}})
    );

    let reads_off = assemble_tools(
        &[],
        BuiltinTools {
            fun: false,
            ..BuiltinTools::ON
        },
    );
    assert!(reads_off.iter().all(|spec| spec.service() != "fun"));
    let write_flag_only = assemble_tools(
        &[],
        BuiltinTools {
            memo_write: true,
            fun: true,
            ..BuiltinTools::OFF
        },
    );
    assert_eq!(
        write_flag_only
            .iter()
            .map(ToolSpec::name)
            .collect::<Vec<_>>(),
        vec!["fun.tags", "fun.stars", "fun.learn", "fun.search"]
    );
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

#[tokio::test(flavor = "current_thread")]
async fn fun_learn_fills_the_current_group() {
    let script = Script::new(Kind::FunLearn);
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
    let fun_only = BuiltinTools {
        fun: true,
        ..BuiltinTools::OFF
    };
    let reply = run_tool_loop(&client, &user("学话里有什么"), &[], fun_only, &pass, &hub)
        .await
        .reply
        .unwrap();
    assert_eq!(reply, ToolReply::Text("学话里有一条。".into()));
    assert_eq!(
        hub.log.lock().unwrap().clone(),
        vec![(
            "fun".into(),
            "learn".into(),
            json!({"group_id": 7, "trigger": "猫"})
        )]
    );
    let names: Vec<_> = script.bodies.lock().unwrap()[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "fun_tags".to_string(),
            "fun_stars".to_string(),
            "fun_learn".to_string(),
            "fun_search".to_string(),
        ]
    );
}
