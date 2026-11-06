use std::collections::HashMap;

use serde_json::{json, Map, Value};

use super::{MemoCallScope, ToolHub};

pub(crate) const PACK: &str = "qq.read";
pub(crate) const PACK_DESC: &str = "只读 QQ。打开之后可以查看发言人所在的群，以及他自己和 bot 的私聊。不能发消息，也不能踢人、禁言或读取凭据。";
pub(crate) const DENIED: &str = "agent qq denied";

pub(crate) struct ReadTool {
    pub name: &'static str,
    pub wire: &'static str,
    pub description: &'static str,
}

const READ_TOOLS: &[ReadTool] = &[
    ReadTool {
        name: "qq.get_group_info",
        wire: "qq_get_group_info",
        description: "读取一个群的资料。数字参数 group_id。发言人不是该群成员时这次调用失败。",
    },
    ReadTool {
        name: "qq.get_group_member_info",
        wire: "qq_get_group_member_info",
        description: "读取一名群成员的资料。数字参数 group_id 和 user_id。发言人不是该群成员时这次调用失败。",
    },
    ReadTool {
        name: "qq.get_group_member_list",
        wire: "qq_get_group_member_list",
        description: "列出一个群的成员。数字参数 group_id。发言人不是该群成员时这次调用失败。",
    },
    ReadTool {
        name: "qq.get_msg",
        wire: "qq_get_msg",
        description: "按消息号读取一条消息。数字参数 message_id。先判断它属于哪个群或哪段私聊。对不上发言人的范围时这次调用失败。",
    },
    ReadTool {
        name: "qq.get_group_msg_history",
        wire: "qq_get_group_msg_history",
        description: "读取一个群的消息记录。数字参数 group_id。可选数字 message_seq 和 count。发言人不是该群成员时这次调用失败。",
    },
    ReadTool {
        name: "qq.get_friend_msg_history",
        wire: "qq_get_friend_msg_history",
        description: "读取发言人自己和 bot 的私聊记录。可选数字 user_id、message_seq 和 count。user_id 必须是发言人，省略时按发言人查询。别人的私聊这次调用失败。",
    },
    ReadTool {
        name: "qq.get_essence_msg_list",
        wire: "qq_get_essence_msg_list",
        description: "列出一个群的精华消息。数字参数 group_id。发言人不是该群成员时这次调用失败。",
    },
];

pub(crate) fn read_tools() -> &'static [ReadTool] {
    READ_TOOLS
}

pub(crate) fn is_read_tool(wire: &str) -> bool {
    READ_TOOLS.iter().any(|tool| tool.wire == wire)
}

pub(crate) fn parameters(wire: &str) -> Value {
    let keys: &[&str] = match wire {
        "qq_get_group_info" | "qq_get_group_member_list" | "qq_get_essence_msg_list" => {
            &["group_id"]
        }
        "qq_get_group_member_info" => &["group_id", "user_id"],
        "qq_get_msg" => &["message_id"],
        "qq_get_group_msg_history" => &["group_id", "message_seq", "count"],
        "qq_get_friend_msg_history" => &["user_id", "message_seq", "count"],
        _ => &[],
    };
    let mut properties = Map::new();
    for key in keys {
        properties.insert((*key).to_string(), json!({"type": "integer"}));
    }
    json!({"type": "object", "properties": properties})
}

#[derive(Debug)]
pub(crate) enum QqFail {
    Denied,
    Down(anyhow::Error),
}

pub(crate) async fn handle<H: ToolHub + ?Sized>(
    hub: &H,
    scope: Option<&MemoCallScope>,
    wire: &str,
    args: &Value,
    members: &mut HashMap<i64, bool>,
) -> Result<Value, QqFail> {
    let Some(speaker) = scope.map(|item| item.user_id).filter(|id| *id > 0) else {
        return Err(QqFail::Denied);
    };
    match wire {
        "qq_get_group_info" => {
            group_action(hub, speaker, "get_group_info", args, members, false).await
        }
        "qq_get_group_member_list" => {
            group_action(hub, speaker, "get_group_member_list", args, members, false).await
        }
        "qq_get_essence_msg_list" => {
            group_action(hub, speaker, "get_essence_msg_list", args, members, false).await
        }
        "qq_get_group_member_info" => {
            group_action(hub, speaker, "get_group_member_info", args, members, true).await
        }
        "qq_get_group_msg_history" => group_history(hub, speaker, args, members).await,
        "qq_get_friend_msg_history" => friend_history(hub, speaker, args).await,
        "qq_get_msg" => read_msg(hub, speaker, args, members).await,
        _ => Err(QqFail::Denied),
    }
}

async fn group_action<H: ToolHub + ?Sized>(
    hub: &H,
    speaker: i64,
    action: &str,
    args: &Value,
    members: &mut HashMap<i64, bool>,
    with_user: bool,
) -> Result<Value, QqFail> {
    let group_id = require_id(args, "group_id")?;
    let mut params = Map::new();
    params.insert("group_id".to_string(), Value::from(group_id));
    if with_user {
        params.insert(
            "user_id".to_string(),
            Value::from(require_id(args, "user_id")?),
        );
    }
    call_group(
        hub,
        speaker,
        action,
        group_id,
        Value::Object(params),
        members,
    )
    .await
}

async fn group_history<H: ToolHub + ?Sized>(
    hub: &H,
    speaker: i64,
    args: &Value,
    members: &mut HashMap<i64, bool>,
) -> Result<Value, QqFail> {
    let group_id = require_id(args, "group_id")?;
    let mut params = Map::new();
    params.insert("group_id".to_string(), Value::from(group_id));
    copy_nonneg(args, &mut params, "message_seq");
    copy_nonneg(args, &mut params, "count");
    let value = call_group(
        hub,
        speaker,
        "get_group_msg_history",
        group_id,
        Value::Object(params),
        members,
    )
    .await?;
    if !group_history_ok(&value, group_id) {
        return Err(QqFail::Denied);
    }
    Ok(value)
}

async fn friend_history<H: ToolHub + ?Sized>(
    hub: &H,
    speaker: i64,
    args: &Value,
) -> Result<Value, QqFail> {
    match args.get("user_id") {
        None | Some(Value::Null) => {}
        Some(value) => {
            if id_of(value) != Some(speaker) {
                return Err(QqFail::Denied);
            }
        }
    }
    let mut params = Map::new();
    params.insert("user_id".to_string(), Value::from(speaker));
    copy_nonneg(args, &mut params, "message_seq");
    copy_nonneg(args, &mut params, "count");
    let value = hub
        .onebot("get_friend_msg_history", Value::Object(params))
        .await
        .map_err(QqFail::Down)?;
    if !friend_history_ok(&value, speaker) {
        return Err(QqFail::Denied);
    }
    Ok(value)
}

async fn read_msg<H: ToolHub + ?Sized>(
    hub: &H,
    speaker: i64,
    args: &Value,
    members: &mut HashMap<i64, bool>,
) -> Result<Value, QqFail> {
    let message_id = require_id(args, "message_id")?;
    let msg = hub
        .onebot("get_msg", json!({"message_id": message_id}))
        .await
        .map_err(QqFail::Down)?;
    match chat_of(&msg) {
        Some(Chat::Group(group_id)) => {
            if !speaker_in_group(hub, speaker, group_id, members).await {
                return Err(QqFail::Denied);
            }
            Ok(msg)
        }
        Some(Chat::Private) if private_ok(&msg, speaker) => Ok(msg),
        _ => Err(QqFail::Denied),
    }
}

async fn call_group<H: ToolHub + ?Sized>(
    hub: &H,
    speaker: i64,
    action: &str,
    group_id: i64,
    params: Value,
    members: &mut HashMap<i64, bool>,
) -> Result<Value, QqFail> {
    if !speaker_in_group(hub, speaker, group_id, members).await {
        return Err(QqFail::Denied);
    }
    hub.onebot(action, params).await.map_err(QqFail::Down)
}

pub(crate) async fn membership<H: ToolHub + ?Sized>(
    hub: &H,
    speaker: i64,
    group_id: i64,
) -> anyhow::Result<bool> {
    let value = hub
        .onebot(
            "get_group_member_info",
            json!({"group_id": group_id, "user_id": speaker}),
        )
        .await?;
    Ok(value.get("user_id").and_then(id_of) == Some(speaker))
}

async fn speaker_in_group<H: ToolHub + ?Sized>(
    hub: &H,
    speaker: i64,
    group_id: i64,
    members: &mut HashMap<i64, bool>,
) -> bool {
    if let Some(hit) = members.get(&group_id) {
        return *hit;
    }
    let ok = membership(hub, speaker, group_id).await.unwrap_or(false);
    members.insert(group_id, ok);
    ok
}

fn require_id(args: &Value, key: &str) -> Result<i64, QqFail> {
    args.get(key).and_then(id_of).ok_or(QqFail::Denied)
}

fn copy_nonneg(args: &Value, params: &mut Map<String, Value>, key: &str) {
    let Some(value) = args.get(key) else {
        return;
    };
    let Some(number) = value.as_i64().filter(|item| *item >= 0) else {
        return;
    };
    params.insert(key.to_string(), Value::from(number));
}

fn id_of(value: &Value) -> Option<i64> {
    let id = value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))?;
    (id > 0).then_some(id)
}

enum Chat {
    Group(i64),
    Private,
}

fn chat_of(msg: &Value) -> Option<Chat> {
    let kind = msg.get("message_type").and_then(Value::as_str);
    let group_id = msg.get("group_id").and_then(id_of);
    match (kind, group_id) {
        (Some("group"), Some(id)) => Some(Chat::Group(id)),
        (Some("private"), None) => Some(Chat::Private),
        (None, Some(id)) => Some(Chat::Group(id)),
        (None, None) if has_person(msg) => Some(Chat::Private),
        _ => None,
    }
}

fn has_person(msg: &Value) -> bool {
    msg.get("user_id").and_then(id_of).is_some()
        || msg.get("target_id").and_then(id_of).is_some()
        || msg
            .get("sender")
            .and_then(|sender| sender.get("user_id"))
            .and_then(id_of)
            .is_some()
}

fn private_ok(msg: &Value, speaker: i64) -> bool {
    let self_id = msg.get("self_id").and_then(id_of);
    let ids = [
        msg.get("user_id").and_then(id_of),
        msg.get("sender")
            .and_then(|sender| sender.get("user_id"))
            .and_then(id_of),
        msg.get("target_id").and_then(id_of),
    ];
    let mut saw_speaker = false;
    for id in ids.into_iter().flatten() {
        if id == speaker {
            saw_speaker = true;
            continue;
        }
        if self_id == Some(id) {
            continue;
        }
        return false;
    }
    saw_speaker
}

fn group_history_ok(value: &Value, group_id: i64) -> bool {
    messages_ok(value, |msg| {
        if msg.get("message_type").and_then(Value::as_str) == Some("private") {
            return false;
        }
        match msg.get("group_id").and_then(id_of) {
            Some(id) => id == group_id,
            None => true,
        }
    })
}

fn friend_history_ok(value: &Value, speaker: i64) -> bool {
    messages_ok(value, |msg| {
        if msg.get("message_type").and_then(Value::as_str) == Some("group") {
            return false;
        }
        if msg.get("group_id").and_then(id_of).is_some() {
            return false;
        }
        match msg.get("target_id").and_then(id_of) {
            Some(target) => target == speaker,
            None => true,
        }
    })
}

fn messages_ok(value: &Value, mut one: impl FnMut(&Value) -> bool) -> bool {
    match value.get("messages") {
        Some(Value::Array(messages)) => messages.iter().all(one),
        Some(_) => false,
        None => one(value),
    }
}
