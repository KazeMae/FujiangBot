use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::structs::SendSegment;

#[derive(Debug, Clone, Serialize)]
pub struct ApiRequest<P> {
    pub action: String,
    pub params: P,
    pub echo: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiResponse {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub retcode: i64,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub wording: Option<String>,
    #[serde(default)]
    pub echo: Option<String>,
}

impl ApiResponse {
    pub fn is_ok(&self) -> bool {
        self.retcode == 0 || self.status == "ok"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SendPrivateMsg {
    pub user_id: i64,
    pub message: Vec<SendSegment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_escape: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SendGroupMsg {
    pub group_id: i64,
    pub message: Vec<SendSegment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_escape: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMsg {
    pub message_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<i64>,
    pub message: Vec<SendSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageIdParam {
    pub message_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendLike {
    pub user_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub times: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupUser {
    pub group_id: i64,
    pub user_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupId {
    pub group_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserId {
    pub user_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GetImage {
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GetFile {
    pub file_id: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SendResult {
    #[serde(default)]
    pub message_id: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileResult {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub file_size: Option<Value>,
    #[serde(default)]
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LoginInfo {
    #[serde(default)]
    pub user_id: i64,
    #[serde(default)]
    pub nickname: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SetFriendAddRequest {
    pub flag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approve: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remark: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SetGroupAddRequest {
    pub flag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub sub_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approve: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
