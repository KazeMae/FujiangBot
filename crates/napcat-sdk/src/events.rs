use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cq;
use crate::structs::RecvSegment;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sender {
    #[serde(default)]
    pub user_id: i64,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub card: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    MetaLifecycleConnect,
    MetaLifecycleEnable,
    MetaLifecycleDisable,
    MetaHeartbeat,
    MessagePrivateFriend,
    MessagePrivateGroup,
    MessageGroupNormal,
    MessageSentPrivateFriend,
    MessageSentPrivateGroup,
    MessageSentGroupNormal,
    RequestFriend,
    RequestGroupAdd,
    RequestGroupInvite,
    NoticeBotOffline,
    NoticeFriendAdd,
    NoticeFriendRecall,
    NoticeGroupAdminSet,
    NoticeGroupAdminUnset,
    NoticeGroupBan,
    NoticeGroupLiftBan,
    NoticeGroupCard,
    NoticeGroupDecreaseLeave,
    NoticeGroupDecreaseKick,
    NoticeGroupDecreaseKickMe,
    NoticeGroupIncreaseApprove,
    NoticeGroupIncreaseInvite,
    NoticeEssenceAdd,
    NoticeEssenceDelete,
    NoticeGroupRecall,
    NoticeGroupUpload,
    NoticeGroupMsgEmojiLike,
    NoticeNotifyPoke,
    NoticeNotifyTitle,
    NoticeNotifyGroupName,
    NoticeNotifyInputStatus,
    NoticeNotifyProfileLike,
    Unknown,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MetaLifecycleConnect => "meta_event.lifecycle.connect",
            Self::MetaLifecycleEnable => "meta_event.lifecycle.enable",
            Self::MetaLifecycleDisable => "meta_event.lifecycle.disable",
            Self::MetaHeartbeat => "meta_event.heartbeat",
            Self::MessagePrivateFriend => "message.private.friend",
            Self::MessagePrivateGroup => "message.private.group",
            Self::MessageGroupNormal => "message.group.normal",
            Self::MessageSentPrivateFriend => "message_sent.private.friend",
            Self::MessageSentPrivateGroup => "message_sent.private.group",
            Self::MessageSentGroupNormal => "message_sent.group.normal",
            Self::RequestFriend => "request.friend",
            Self::RequestGroupAdd => "request.group.add",
            Self::RequestGroupInvite => "request.group.invite",
            Self::NoticeBotOffline => "notice.bot_offline",
            Self::NoticeFriendAdd => "notice.friend_add",
            Self::NoticeFriendRecall => "notice.friend_recall",
            Self::NoticeGroupAdminSet => "notice.group_admin.set",
            Self::NoticeGroupAdminUnset => "notice.group_admin.unset",
            Self::NoticeGroupBan => "notice.group_ban.ban",
            Self::NoticeGroupLiftBan => "notice.group_ban.lift_ban",
            Self::NoticeGroupCard => "notice.group_card",
            Self::NoticeGroupDecreaseLeave => "notice.group_decrease.leave",
            Self::NoticeGroupDecreaseKick => "notice.group_decrease.kick",
            Self::NoticeGroupDecreaseKickMe => "notice.group_decrease.kick_me",
            Self::NoticeGroupIncreaseApprove => "notice.group_increase.approve",
            Self::NoticeGroupIncreaseInvite => "notice.group_increase.invite",
            Self::NoticeEssenceAdd => "notice.essence.add",
            Self::NoticeEssenceDelete => "notice.essence.delete",
            Self::NoticeGroupRecall => "notice.group_recall",
            Self::NoticeGroupUpload => "notice.group_upload",
            Self::NoticeGroupMsgEmojiLike => "notice.group_msg_emoji_like",
            Self::NoticeNotifyPoke => "notice.notify.poke",
            Self::NoticeNotifyTitle => "notice.notify.title",
            Self::NoticeNotifyGroupName => "notice.notify.group_name",
            Self::NoticeNotifyInputStatus => "notice.notify.input_status",
            Self::NoticeNotifyProfileLike => "notice.notify.profile_like",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Event {
    pub kind: EventKind,
    pub raw: Value,
}

impl Event {
    pub fn parse(mut raw: Value) -> Self {
        normalize_message(&mut raw);
        let kind = classify(&raw);
        Self { kind, raw }
    }

    pub fn post_type(&self) -> &str {
        self.raw
            .get("post_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    pub fn message_id(&self) -> Option<i64> {
        self.raw.get("message_id").and_then(|v| v.as_i64())
    }

    pub fn user_id(&self) -> Option<i64> {
        self.raw.get("user_id").and_then(|v| v.as_i64())
    }

    pub fn group_id(&self) -> Option<i64> {
        self.raw.get("group_id").and_then(|v| v.as_i64())
    }

    pub fn self_id(&self) -> Option<i64> {
        self.raw.get("self_id").and_then(|v| v.as_i64())
    }

    pub fn time(&self) -> Option<i64> {
        self.raw.get("time").and_then(|v| v.as_i64())
    }

    pub fn sender(&self) -> Sender {
        serde_json::from_value(self.raw.get("sender").cloned().unwrap_or(Value::Null)).unwrap_or(
            Sender {
                user_id: self.user_id().unwrap_or(0),
                nickname: None,
                card: None,
                role: None,
            },
        )
    }

    pub fn message_segments(&self) -> Vec<RecvSegment> {
        match self.raw.get("message") {
            Some(Value::Array(arr)) => arr.iter().map(RecvSegment::from_value).collect(),
            Some(Value::String(s)) => cq::cq_to_array(s)
                .iter()
                .map(RecvSegment::from_value)
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn raw_text(&self) -> String {
        self.message_segments()
            .into_iter()
            .filter_map(|s| match s {
                RecvSegment::Text { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn is_message(&self) -> bool {
        self.post_type() == "message"
    }
}

fn normalize_message(raw: &mut Value) {
    let post = raw.get("post_type").and_then(|v| v.as_str()).unwrap_or("");
    if post != "message" && post != "message_sent" {
        return;
    }
    let format = raw
        .get("message_format")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if format == "string" {
        if let Some(Value::String(s)) = raw.get("message").cloned() {
            raw["message"] = Value::Array(cq::cq_to_array(&s));
            raw["message_format"] = Value::String("array".into());
        }
    }
    if let Some(Value::String(s)) = raw.get("raw_message").cloned() {
        raw["raw_message"] = Value::String(cq::cq_unescape(&s));
    }
}

fn classify(raw: &Value) -> EventKind {
    let post = raw.get("post_type").and_then(|v| v.as_str()).unwrap_or("");
    match post {
        "meta_event" => match raw.get("meta_event_type").and_then(|v| v.as_str()) {
            Some("heartbeat") => EventKind::MetaHeartbeat,
            Some("lifecycle") => match raw.get("sub_type").and_then(|v| v.as_str()) {
                Some("connect") => EventKind::MetaLifecycleConnect,
                Some("enable") => EventKind::MetaLifecycleEnable,
                Some("disable") => EventKind::MetaLifecycleDisable,
                _ => EventKind::Unknown,
            },
            _ => EventKind::Unknown,
        },
        "message" => classify_message(raw, false),
        "message_sent" => classify_message(raw, true),
        "request" => match raw.get("request_type").and_then(|v| v.as_str()) {
            Some("friend") => EventKind::RequestFriend,
            Some("group") => match raw.get("sub_type").and_then(|v| v.as_str()) {
                Some("add") => EventKind::RequestGroupAdd,
                Some("invite") => EventKind::RequestGroupInvite,
                _ => EventKind::Unknown,
            },
            _ => EventKind::Unknown,
        },
        "notice" => classify_notice(raw),
        _ => EventKind::Unknown,
    }
}

fn classify_message(raw: &Value, sent: bool) -> EventKind {
    match raw.get("message_type").and_then(|v| v.as_str()) {
        Some("private") => match raw.get("sub_type").and_then(|v| v.as_str()) {
            Some("group") if sent => EventKind::MessageSentPrivateGroup,
            Some("friend") if sent => EventKind::MessageSentPrivateFriend,
            Some("group") => EventKind::MessagePrivateGroup,
            _ => EventKind::MessagePrivateFriend,
        },
        Some("group") => {
            if sent {
                EventKind::MessageSentGroupNormal
            } else {
                EventKind::MessageGroupNormal
            }
        }
        _ => EventKind::Unknown,
    }
}

fn classify_notice(raw: &Value) -> EventKind {
    match raw.get("notice_type").and_then(|v| v.as_str()) {
        Some("bot_offline") => EventKind::NoticeBotOffline,
        Some("friend_add") => EventKind::NoticeFriendAdd,
        Some("friend_recall") => EventKind::NoticeFriendRecall,
        Some("group_admin") => match raw.get("sub_type").and_then(|v| v.as_str()) {
            Some("unset") => EventKind::NoticeGroupAdminUnset,
            _ => EventKind::NoticeGroupAdminSet,
        },
        Some("group_ban") => match raw.get("sub_type").and_then(|v| v.as_str()) {
            Some("lift_ban") => EventKind::NoticeGroupLiftBan,
            _ => EventKind::NoticeGroupBan,
        },
        Some("group_card") => EventKind::NoticeGroupCard,
        Some("group_decrease") => match raw.get("sub_type").and_then(|v| v.as_str()) {
            Some("kick") => EventKind::NoticeGroupDecreaseKick,
            Some("kick_me") => EventKind::NoticeGroupDecreaseKickMe,
            _ => EventKind::NoticeGroupDecreaseLeave,
        },
        Some("group_increase") => match raw.get("sub_type").and_then(|v| v.as_str()) {
            Some("invite") => EventKind::NoticeGroupIncreaseInvite,
            _ => EventKind::NoticeGroupIncreaseApprove,
        },
        Some("essence") => match raw.get("sub_type").and_then(|v| v.as_str()) {
            Some("delete") => EventKind::NoticeEssenceDelete,
            _ => EventKind::NoticeEssenceAdd,
        },
        Some("group_recall") => EventKind::NoticeGroupRecall,
        Some("group_upload") => EventKind::NoticeGroupUpload,
        Some("group_msg_emoji_like") => EventKind::NoticeGroupMsgEmojiLike,
        Some("notify") => match raw.get("sub_type").and_then(|v| v.as_str()) {
            Some("poke") => EventKind::NoticeNotifyPoke,
            Some("title") => EventKind::NoticeNotifyTitle,
            Some("group_name") => EventKind::NoticeNotifyGroupName,
            Some("input_status") => EventKind::NoticeNotifyInputStatus,
            Some("profile_like") => EventKind::NoticeNotifyProfileLike,
            _ => EventKind::Unknown,
        },
        _ => EventKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_group_text() {
        let ev = Event::parse(json!({
            "post_type": "message",
            "message_type": "group",
            "sub_type": "normal",
            "message_id": 1,
            "group_id": 100,
            "user_id": 2,
            "self_id": 9,
            "time": 1,
            "message_format": "array",
            "message": [{"type":"text","data":{"text":".help"}}],
            "sender": {"user_id": 2, "nickname": "a"}
        }));
        assert_eq!(ev.kind, EventKind::MessageGroupNormal);
        assert_eq!(ev.raw_text(), ".help");
        assert_eq!(ev.group_id(), Some(100));
    }

    #[test]
    fn parse_string_format() {
        let ev = Event::parse(json!({
            "post_type": "message",
            "message_type": "private",
            "sub_type": "friend",
            "message_format": "string",
            "message": "hi[CQ:at,qq=1]",
            "user_id": 3
        }));
        assert_eq!(ev.kind, EventKind::MessagePrivateFriend);
        assert_eq!(ev.message_segments().len(), 2);
    }
}
