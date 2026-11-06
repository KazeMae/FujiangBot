use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fujiang_core::Source;
use serde_json::{json, Value};

use super::super::qq_read::{self, QqFail, DENIED, PACK, PACK_DESC};
use super::super::skills::{self, round_tools, SkillOut, CLOSED_SKILL};
use super::super::{
    resident_tools, run_tool_loop, BuiltinTools, MemoCallScope, ToolHub, ToolPass, ToolReply,
    ToolSpec, TOOL_FAIL_REPLY,
};
use super::{one_call_with, saying, spawn_app, test_client, user, Catch, Kind, Script};

const DANGER: &[&str] = &[
    "send_private_msg",
    "send_group_msg",
    "send_msg",
    "delete_msg",
    "set_group_kick",
    "set_group_kick_members",
    "set_group_ban",
    "set_group_whole_ban",
    "set_group_admin",
    "set_group_leave",
    "get_cookies",
    "get_credentials",
    "get_csrf_token",
    "clean_cache",
    "bot_exit",
    "get_stranger_info",
];

const QQ_WIRES: &[&str] = &[
    "qq_get_group_info",
    "qq_get_group_member_info",
    "qq_get_group_member_list",
    "qq_get_msg",
    "qq_get_group_msg_history",
    "qq_get_friend_msg_history",
    "qq_get_essence_msg_list",
];

struct Api {
    calls: Mutex<Vec<(String, Value)>>,
    replies: Mutex<VecDeque<Result<Value, String>>>,
    service: Mutex<Vec<(String, String, Value)>>,
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
            service: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ToolHub for Api {
    async fn invoke(&self, service: &str, method: &str, args: Value) -> anyhow::Result<Value> {
        self.service
            .lock()
            .unwrap()
            .push((service.to_string(), method.to_string(), args));
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

fn scope(group: i64, user: i64) -> MemoCallScope {
    MemoCallScope::from_source(Source::Group { id: group }, user)
}

fn pass(scope: &MemoCallScope) -> ToolPass<'_> {
    ToolPass {
        max_rounds: 3,
        scope: Some(scope),
        memory: None,
        memory_unused_secs: 1,
        memory_max_entries: 1,
    }
}

async fn call(
    api: &Api,
    who: Option<&MemoCallScope>,
    wire: &str,
    args: Value,
    members: &mut HashMap<i64, bool>,
) -> Result<Value, QqFail> {
    qq_read::handle(api, who, wire, &args, members).await
}

fn denied(result: Result<Value, QqFail>) {
    match result {
        Err(QqFail::Denied) => {}
        other => panic!("{other:?}"),
    }
}

fn wires(tools: &[ToolSpec]) -> Vec<&str> {
    tools.iter().map(ToolSpec::wire_name).collect()
}

fn ready(step: Option<SkillOut>) -> Result<Value, &'static str> {
    match step {
        Some(SkillOut::Ready(result)) => result,
        other => panic!("{other:?}"),
    }
}

fn script_of(steps: Vec<Value>) -> Script {
    let mut script = Script::new(Kind::Text);
    script.steps = Arc::new(steps);
    script
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

#[test]
fn tools_stay_closed_until_opened_and_dangerous_actions_stay_out() {
    let resident = resident_tools(&[], BuiltinTools::OFF);
    assert!(resident.iter().all(|spec| !spec.name().starts_with("qq.")));
    let closed = round_tools(&[], BuiltinTools::OFF, &BTreeSet::new());
    assert!(wires(&closed).iter().all(|wire| !wire.starts_with("qq_")));

    let mut opened = BTreeSet::new();
    let now = opened.clone();
    let blocked = ready(skills::exec(
        "qq_get_group_info",
        &json!({"group_id": 7}),
        BuiltinTools::OFF,
        &now,
        &mut opened,
    ))
    .unwrap_err();
    assert_eq!(blocked, CLOSED_SKILL);
    assert!(opened.is_empty());
    for action in DANGER {
        assert!(skills::exec(action, &json!({}), BuiltinTools::OFF, &now, &mut opened).is_none());
    }

    let mut sink = BTreeSet::new();
    let listed = ready(skills::exec(
        "skills_list",
        &json!({}),
        BuiltinTools::OFF,
        &opened,
        &mut sink,
    ))
    .unwrap();
    assert_eq!(listed[0]["name"], "ping");
    assert_eq!(listed[1]["name"], PACK);
    assert_eq!(listed[1]["description"], PACK_DESC);
    assert_eq!(listed[1].as_object().unwrap().len(), 2);
    let raw = serde_json::to_string(&listed).unwrap();
    assert!(!raw.contains("qq.get_"));
    assert!(!raw.contains("qq_get_"));
    for action in DANGER {
        assert!(!raw.contains(action), "{action}");
    }

    let now = opened.clone();
    let opened_ok = ready(skills::exec(
        "skills_open",
        &json!({"name": " qq.read "}),
        BuiltinTools::OFF,
        &now,
        &mut opened,
    ))
    .unwrap();
    assert_eq!(opened_ok, json!({"name": "qq.read", "opened": true}));

    let smuggle = ToolSpec::parse("qq.get_group_info", "偷偷", json!({})).unwrap();
    let shown = round_tools(std::slice::from_ref(&smuggle), BuiltinTools::OFF, &opened);
    let names = wires(&shown);
    for wire in QQ_WIRES {
        assert_eq!(names.iter().filter(|item| *item == wire).count(), 1);
    }
    let info = shown
        .iter()
        .find(|spec| spec.wire_name() == "qq_get_group_info")
        .unwrap();
    assert_eq!(info.name(), "qq.get_group_info");
    assert!(!info.description().contains("偷偷"));
    assert!(info.parameters()["properties"].get("group_id").is_some());
    assert!(shown.iter().all(|spec| {
        DANGER.iter().all(|action| {
            !spec.name().contains(action)
                && !spec.wire_name().contains(action)
                && !spec.description().contains(action)
        })
    }));
}

#[tokio::test]
async fn same_group_read_is_allowed_and_membership_is_cached() {
    let who = scope(100, 42);
    let api = Api::script(vec![
        Ok(json!({"user_id": "42", "nickname": "me"})),
        Ok(json!({"group_id": 7, "group_name": "g"})),
        Ok(json!({"group_id": 7, "group_name": "g"})),
        Ok(json!({"user_id": 5, "card": "x"})),
    ]);
    let mut members = HashMap::new();
    let got = call(
        &api,
        Some(&who),
        "qq_get_group_info",
        json!({"group_id": "7", "action": "set_group_kick", "user_id": 99}),
        &mut members,
    )
    .await
    .unwrap();
    assert_eq!(got["group_name"], "g");
    let again = call(
        &api,
        Some(&who),
        "qq_get_group_info",
        json!({"group_id": 7}),
        &mut members,
    )
    .await
    .unwrap();
    assert_eq!(again, got);
    let member = call(
        &api,
        Some(&who),
        "qq_get_group_member_info",
        json!({"group_id": 7, "user_id": 5}),
        &mut members,
    )
    .await
    .unwrap();
    assert_eq!(member["card"], "x");

    let list_api = Api::script(vec![
        Ok(json!({"user_id": 42})),
        Ok(json!([{"user_id": 42}, {"user_id": 5}])),
    ]);
    let mut list_members = HashMap::new();
    let listed = call(
        &list_api,
        Some(&who),
        "qq_get_group_member_list",
        json!({"group_id": 8}),
        &mut list_members,
    )
    .await
    .unwrap();
    assert_eq!(listed[0]["user_id"], 42);
    let essence_api = Api::script(vec![
        Ok(json!({"user_id": 42})),
        Ok(json!([{"message_id": 4}])),
    ]);
    let mut essence_members = HashMap::new();
    let essence = call(
        &essence_api,
        Some(&who),
        "qq_get_essence_msg_list",
        json!({"group_id": 8}),
        &mut essence_members,
    )
    .await
    .unwrap();
    assert_eq!(essence[0]["message_id"], 4);

    let history_api = Api::script(vec![
        Ok(json!({"user_id": 42})),
        Ok(json!({"messages": [{"message_id": 9}, {"group_id": 8, "raw_message": "在"}]})),
    ]);
    let mut history_members = HashMap::new();
    let history = call(
        &history_api,
        Some(&who),
        "qq_get_group_msg_history",
        json!({"group_id": 8, "count": 5, "message_seq": 0, "user_id": 99}),
        &mut history_members,
    )
    .await
    .unwrap();
    assert_eq!(history["messages"][0]["message_id"], 9);

    let mismatch = Api::script(vec![
        Ok(json!({"user_id": 42})),
        Ok(json!({"messages": [{"group_id": 8}, {"group_id": 9, "raw_message": "别人的群"}]})),
    ]);
    let mut mismatch_members = HashMap::new();
    denied(
        call(
            &mismatch,
            Some(&who),
            "qq_get_group_msg_history",
            json!({"group_id": 8}),
            &mut mismatch_members,
        )
        .await,
    );
    assert_eq!(mismatch.calls()[1].0, "get_group_msg_history");

    let calls = api.calls();
    assert_eq!(calls[0].0, "get_group_member_info");
    assert_eq!(calls[0].1, json!({"group_id": 7, "user_id": 42}));
    assert_eq!(calls[1], ("get_group_info".into(), json!({"group_id": 7})));
    assert_eq!(calls[2], ("get_group_info".into(), json!({"group_id": 7})));
    assert_eq!(
        calls[3],
        (
            "get_group_member_info".into(),
            json!({"group_id": 7, "user_id": 5})
        )
    );
    assert_eq!(calls.len(), 4);
    assert_eq!(list_api.calls()[0].1, json!({"group_id": 8, "user_id": 42}));
    assert_eq!(list_api.calls()[1].0, "get_group_member_list");
    assert!(api.service.lock().unwrap().is_empty());
    assert_eq!(
        history_api.calls()[1].1,
        json!({"group_id": 8, "message_seq": 0, "count": 5})
    );
    assert_eq!(essence_api.calls()[1].0, "get_essence_msg_list");
}

#[tokio::test]
async fn someone_outside_the_group_is_rejected_before_the_read() {
    let who = scope(100, 42);
    for reply in [
        Ok(json!({"user_id": 10001, "nickname": "bot"})),
        Err("not a member"),
        Ok(json!({})),
    ] {
        let api = Api::script(vec![reply, Ok(json!({"group_name": "secret"}))]);
        let mut members = HashMap::new();
        denied(
            call(
                &api,
                Some(&who),
                "qq_get_group_info",
                json!({"group_id": 7}),
                &mut members,
            )
            .await,
        );
        assert_eq!(api.calls().len(), 1);
        assert_eq!(api.calls()[0].0, "get_group_member_info");
        assert_eq!(api.calls()[0].1["user_id"], 42);
        let mut again = HashMap::new();
        again.insert(7, false);
        let cached = Api::script(vec![Ok(json!({"group_name": "secret"}))]);
        denied(
            call(
                &cached,
                Some(&who),
                "qq_get_group_member_list",
                json!({"group_id": 7}),
                &mut again,
            )
            .await,
        );
        assert!(cached.calls().is_empty());
    }

    let api = Api::script(vec![Ok(json!({"group_name": "secret"}))]);
    let mut members = HashMap::new();
    denied(
        call(
            &api,
            Some(&who),
            "qq_get_group_info",
            json!({"group_id": " "}),
            &mut members,
        )
        .await,
    );
    denied(
        call(
            &api,
            None,
            "qq_get_group_info",
            json!({"group_id": 7}),
            &mut members,
        )
        .await,
    );
    let zero = scope(100, 0);
    denied(
        call(
            &api,
            Some(&zero),
            "qq_get_essence_msg_list",
            json!({"group_id": 7}),
            &mut members,
        )
        .await,
    );
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn private_history_is_only_the_speakers_own_dm() {
    let who = scope(100, 42);
    let api = Api::script(vec![Ok(
        json!({"messages": [{"message_type": "private", "user_id": 42, "raw_message": "我"}, {"user_id": 1, "raw_message": "bot"}]}),
    )]);
    let mut members = HashMap::new();
    let own = call(
        &api,
        Some(&who),
        "qq_get_friend_msg_history",
        json!({"count": 2}),
        &mut members,
    )
    .await
    .unwrap();
    assert_eq!(own["messages"][0]["raw_message"], "我");
    assert_eq!(
        api.calls()[0],
        (
            "get_friend_msg_history".into(),
            json!({"user_id": 42, "count": 2})
        )
    );

    let named = Api::script(vec![Ok(json!({"messages": []}))]);
    let got = call(
        &named,
        Some(&who),
        "qq_get_friend_msg_history",
        json!({"user_id": "42", "message_seq": 3}),
        &mut members,
    )
    .await
    .unwrap();
    assert_eq!(got["messages"], json!([]));
    assert_eq!(named.calls()[0].1, json!({"user_id": 42, "message_seq": 3}));

    for args in [
        json!({"user_id": 99}),
        json!({"user_id": "nope"}),
        json!({"user_id": 0}),
    ] {
        let blocked = Api::script(vec![Ok(json!({"messages": [{"raw_message": "别人"}]}))]);
        denied(
            call(
                &blocked,
                Some(&who),
                "qq_get_friend_msg_history",
                args,
                &mut members,
            )
            .await,
        );
        assert!(blocked.calls().is_empty());
    }

    let leaked = Api::script(vec![Ok(
        json!({"messages": [{"message_type": "group", "group_id": 7, "raw_message": "群"}]}),
    )]);
    denied(
        call(
            &leaked,
            Some(&who),
            "qq_get_friend_msg_history",
            json!({}),
            &mut members,
        )
        .await,
    );
    assert_eq!(leaked.calls().len(), 1);
    assert!(leaked.calls()[0].1["raw_message"].is_null());
}

#[tokio::test]
async fn get_msg_uses_the_chat_it_resolves_to() {
    let who = scope(100, 42);
    let private_self = Api::script(vec![Ok(
        json!({"message_type": "private", "user_id": 42, "sender": {"user_id": 42}, "raw_message": "自己"}),
    )]);
    let mut members = HashMap::new();
    let own = call(
        &private_self,
        Some(&who),
        "qq_get_msg",
        json!({"message_id": 11}),
        &mut members,
    )
    .await
    .unwrap();
    assert_eq!(own["raw_message"], "自己");
    assert_eq!(
        private_self.calls()[0],
        ("get_msg".into(), json!({"message_id": 11}))
    );

    let sent = Api::script(vec![Ok(json!({
        "message_type": "private",
        "self_id": 1,
        "user_id": 1,
        "sender": {"user_id": 1},
        "target_id": 42,
        "raw_message": "bot 发出"
    }))]);
    let bot_sent = call(
        &sent,
        Some(&who),
        "qq_get_msg",
        json!({"message_id": 12}),
        &mut members,
    )
    .await
    .unwrap();
    assert_eq!(bot_sent["raw_message"], "bot 发出");

    let other = Api::script(vec![Ok(
        json!({"message_type": "private", "user_id": 99, "sender": {"user_id": 99}, "raw_message": "别人"}),
    )]);
    denied(
        call(
            &other,
            Some(&who),
            "qq_get_msg",
            json!({"message_id": 13}),
            &mut members,
        )
        .await,
    );
    assert_eq!(other.calls().len(), 1);
    assert_eq!(other.calls()[0].0, "get_msg");

    let group = Api::script(vec![
        Ok(json!({"message_type": "group", "group_id": 8, "user_id": 5, "raw_message": "群里"})),
        Ok(json!({"user_id": 42})),
    ]);
    let mut group_members = HashMap::new();
    let msg = call(
        &group,
        Some(&who),
        "qq_get_msg",
        json!({"message_id": 14}),
        &mut group_members,
    )
    .await
    .unwrap();
    assert_eq!(msg["raw_message"], "群里");
    assert_eq!(group.calls()[1].0, "get_group_member_info");
    assert_eq!(group.calls()[1].1, json!({"group_id": 8, "user_id": 42}));
    let info = Api::script(vec![Ok(json!({"group_id": 8, "group_name": "cached"}))]);
    let cached = call(
        &info,
        Some(&who),
        "qq_get_group_info",
        json!({"group_id": 8}),
        &mut group_members,
    )
    .await
    .unwrap();
    assert_eq!(cached["group_name"], "cached");
    assert_eq!(info.calls().len(), 1);
    assert_eq!(info.calls()[0].0, "get_group_info");

    let outside = Api::script(vec![
        Ok(json!({"message_type": "group", "group_id": 9, "raw_message": "外面"})),
        Err("absent"),
    ]);
    let mut outside_members = HashMap::new();
    denied(
        call(
            &outside,
            Some(&who),
            "qq_get_msg",
            json!({"message_id": 15}),
            &mut outside_members,
        )
        .await,
    );
    assert_eq!(
        outside
            .calls()
            .into_iter()
            .map(|(action, _)| action)
            .collect::<Vec<_>>(),
        vec!["get_msg".to_string(), "get_group_member_info".to_string()]
    );

    let both = Api::script(vec![Ok(
        json!({"message_type": "private", "group_id": 8, "user_id": 42, "raw_message": "对不上"}),
    )]);
    denied(
        call(
            &both,
            Some(&who),
            "qq_get_msg",
            json!({"message_id": 16}),
            &mut members,
        )
        .await,
    );
    assert_eq!(both.calls().len(), 1);
}

#[tokio::test]
async fn the_loop_reads_with_the_speaker_and_does_not_call_service_hub() {
    let who = scope(100, 42);
    let api = Api::script(vec![
        Ok(json!({"user_id": 42})),
        Ok(json!({"group_id": 7, "group_name": "g"})),
    ]);
    let script = script_of(vec![
        one_call_with("open_qq", "skills_open", r#"{"name":"qq.read"}"#),
        one_call_with(
            "read_qq",
            "qq_get_group_info",
            r#"{"group_id":7,"user_id":99}"#,
        ),
        saying("好"),
    ]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let reply = run_tool_loop(
        &client,
        &user("看这个群"),
        &[],
        BuiltinTools::OFF,
        &pass(&who),
        &api,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Text("好".into()));
    assert!(api.service.lock().unwrap().is_empty());
    assert_eq!(api.calls()[0].1, json!({"group_id": 7, "user_id": 42}));
    assert_eq!(api.calls()[1].0, "get_group_info");
    let bodies = script.bodies.lock().unwrap().clone();
    assert!(!tool_names(&bodies[0])
        .iter()
        .any(|name| name.starts_with("qq_")));
    assert!(tool_names(&bodies[1])
        .iter()
        .any(|name| name == "qq_get_group_info"));
    assert_eq!(
        outputs(&bodies[2]).last().unwrap(),
        r#"{"group_id":7,"group_name":"g"}"#
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_closed_or_forbidden_read_fails_the_turn_without_the_payload() {
    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let who = scope(100, 42);
    let closed = Api::script(vec![Ok(json!({"group_name": "secret"}))]);
    let script = script_of(vec![
        one_call_with("early", "qq_get_group_info", r#"{"group_id":7}"#),
        saying("不该到"),
    ]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let reply = run_tool_loop(
        &client,
        &user("没打开"),
        &[],
        BuiltinTools::OFF,
        &pass(&who),
        &closed,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert!(closed.calls().is_empty());
    assert!(catch.has(CLOSED_SKILL));
    drop(_guard);

    let catch = Catch::default();
    let _guard = tracing::subscriber::set_default(catch.clone());
    let denied_api = Api::script(vec![
        Ok(json!({"user_id": 10001})),
        Ok(json!({"group_name": "secret"})),
    ]);
    let script = script_of(vec![
        one_call_with("open_deny", "skills_open", r#"{"name":"qq.read"}"#),
        one_call_with("read_deny", "qq_get_group_info", r#"{"group_id":7}"#),
        saying("不该到"),
    ]);
    let base = spawn_app(script).await;
    let client = test_client(&base);
    let reply = run_tool_loop(
        &client,
        &user("不在群里"),
        &[],
        BuiltinTools::OFF,
        &pass(&who),
        &denied_api,
    )
    .await
    .reply
    .unwrap();
    assert_eq!(reply, ToolReply::Degraded(TOOL_FAIL_REPLY));
    assert_eq!(denied_api.calls().len(), 1);
    assert_eq!(denied_api.calls()[0].0, "get_group_member_info");
    assert!(catch.has(DENIED));
    assert!(denied_api.service.lock().unwrap().is_empty());
}
