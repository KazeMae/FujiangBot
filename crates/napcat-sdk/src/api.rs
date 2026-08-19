//! Typed wrappers over OneBot 11 + NapCat actions.
//!
//! Thin `send(action, params)` bindings, matching `NCWebsocketApi.ts`.
//! Obscure actions return `serde_json::Value`; message I/O is typed.

use serde::Serialize;
use serde_json::{json, Value};

use crate::client::NapcatClient;
use crate::error::Result;
use crate::types::*;

macro_rules! api {
    ($fn:ident, $action:literal) => {
        pub async fn $fn(&self) -> Result<Value> {
            self.send($action, json!({})).await
        }
    };
    ($fn:ident, $action:literal, $ty:ty) => {
        pub async fn $fn(&self, params: $ty) -> Result<Value> {
            self.send($action, params).await
        }
    };
}

impl NapcatClient {
    pub async fn send_private_msg(&self, params: SendPrivateMsg) -> Result<SendResult> {
        self.send("send_private_msg", params).await
    }

    pub async fn send_group_msg(&self, params: SendGroupMsg) -> Result<SendResult> {
        self.send("send_group_msg", params).await
    }

    pub async fn send_msg(&self, params: SendMsg) -> Result<SendResult> {
        self.send("send_msg", params).await
    }

    pub async fn delete_msg(&self, message_id: i64) -> Result<Value> {
        self.send("delete_msg", MessageIdParam { message_id }).await
    }

    pub async fn get_msg(&self, message_id: i64) -> Result<Value> {
        self.send("get_msg", MessageIdParam { message_id }).await
    }

    pub async fn get_forward_msg(&self, id: impl Into<String>) -> Result<Value> {
        self.send("get_forward_msg", json!({ "id": id.into() }))
            .await
    }

    pub async fn send_like(&self, params: SendLike) -> Result<Value> {
        self.send("send_like", params).await
    }

    pub async fn set_group_kick(
        &self,
        group_id: i64,
        user_id: i64,
        reject_add_request: bool,
    ) -> Result<Value> {
        self.send(
            "set_group_kick",
            json!({
                "group_id": group_id,
                "user_id": user_id,
                "reject_add_request": reject_add_request
            }),
        )
        .await
    }

    pub async fn set_group_ban(&self, group_id: i64, user_id: i64, duration: i64) -> Result<Value> {
        self.send(
            "set_group_ban",
            json!({"group_id": group_id, "user_id": user_id, "duration": duration}),
        )
        .await
    }

    pub async fn set_group_whole_ban(&self, group_id: i64, enable: bool) -> Result<Value> {
        self.send(
            "set_group_whole_ban",
            json!({"group_id": group_id, "enable": enable}),
        )
        .await
    }

    pub async fn set_group_admin(
        &self,
        group_id: i64,
        user_id: i64,
        enable: bool,
    ) -> Result<Value> {
        self.send(
            "set_group_admin",
            json!({"group_id": group_id, "user_id": user_id, "enable": enable}),
        )
        .await
    }

    pub async fn set_group_card(&self, group_id: i64, user_id: i64, card: &str) -> Result<Value> {
        self.send(
            "set_group_card",
            json!({"group_id": group_id, "user_id": user_id, "card": card}),
        )
        .await
    }

    pub async fn set_group_name(&self, group_id: i64, group_name: &str) -> Result<Value> {
        self.send(
            "set_group_name",
            json!({"group_id": group_id, "group_name": group_name}),
        )
        .await
    }

    pub async fn set_group_leave(&self, group_id: i64, is_dismiss: bool) -> Result<Value> {
        self.send(
            "set_group_leave",
            json!({"group_id": group_id, "is_dismiss": is_dismiss}),
        )
        .await
    }

    pub async fn set_group_special_title(
        &self,
        group_id: i64,
        user_id: i64,
        special_title: &str,
    ) -> Result<Value> {
        self.send(
            "set_group_special_title",
            json!({
                "group_id": group_id,
                "user_id": user_id,
                "special_title": special_title
            }),
        )
        .await
    }

    pub async fn set_friend_add_request(&self, params: SetFriendAddRequest) -> Result<Value> {
        self.send("set_friend_add_request", params).await
    }

    pub async fn set_friend_remark(&self, user_id: i64, remark: &str) -> Result<Value> {
        self.send(
            "set_friend_remark",
            json!({"user_id": user_id, "remark": remark}),
        )
        .await
    }

    pub async fn set_group_add_request(&self, params: SetGroupAddRequest) -> Result<Value> {
        self.send("set_group_add_request", params).await
    }

    pub async fn get_login_info(&self) -> Result<LoginInfo> {
        self.send("get_login_info", json!({})).await
    }

    pub async fn get_stranger_info(&self, user_id: i64) -> Result<Value> {
        self.send("get_stranger_info", json!({"user_id": user_id}))
            .await
    }

    api!(get_friend_list, "get_friend_list");

    pub async fn get_group_info(&self, group_id: i64) -> Result<Value> {
        self.send("get_group_info", GroupId { group_id }).await
    }

    pub async fn get_group_list(&self) -> Result<Value> {
        self.send("get_group_list", json!({})).await
    }

    pub async fn get_group_member_info(&self, group_id: i64, user_id: i64) -> Result<Value> {
        self.send("get_group_member_info", GroupUser { group_id, user_id })
            .await
    }

    pub async fn get_group_member_list(&self, group_id: i64) -> Result<Value> {
        self.send("get_group_member_list", GroupId { group_id })
            .await
    }

    pub async fn get_group_honor_info(&self, group_id: i64, ty: &str) -> Result<Value> {
        self.send(
            "get_group_honor_info",
            json!({"group_id": group_id, "type": ty}),
        )
        .await
    }

    pub async fn get_cookies(&self, domain: &str) -> Result<Value> {
        self.send("get_cookies", json!({"domain": domain})).await
    }

    api!(get_csrf_token, "get_csrf_token");
    api!(get_credentials, "get_credentials");

    pub async fn get_record(&self, file: &str, out_format: &str) -> Result<FileResult> {
        self.send(
            "get_record",
            json!({"file": file, "out_format": out_format}),
        )
        .await
    }

    pub async fn get_image(&self, file: &str) -> Result<FileResult> {
        self.send("get_image", GetImage { file: file.into() }).await
    }

    api!(can_send_image, "can_send_image");
    api!(can_send_record, "can_send_record");
    api!(get_status, "get_status");
    api!(get_version_info, "get_version_info");
    api!(clean_cache, "clean_cache");
    api!(bot_exit, "bot_exit");

    // go-cqhttp
    pub async fn set_qq_profile(&self, params: Value) -> Result<Value> {
        self.send("set_qq_profile", params).await
    }

    api!(
        get_unidirectional_friend_list,
        "get_unidirectional_friend_list"
    );

    pub async fn delete_friend(&self, user_id: i64) -> Result<Value> {
        self.send("delete_friend", UserId { user_id }).await
    }

    pub async fn mark_msg_as_read(&self, message_id: i64) -> Result<Value> {
        self.send("mark_msg_as_read", MessageIdParam { message_id })
            .await
    }

    pub async fn send_group_forward_msg(&self, group_id: i64, messages: Value) -> Result<Value> {
        self.send(
            "send_group_forward_msg",
            json!({"group_id": group_id, "messages": messages}),
        )
        .await
    }

    pub async fn send_private_forward_msg(&self, user_id: i64, messages: Value) -> Result<Value> {
        self.send(
            "send_private_forward_msg",
            json!({"user_id": user_id, "messages": messages}),
        )
        .await
    }

    pub async fn get_group_msg_history(&self, params: Value) -> Result<Value> {
        self.send("get_group_msg_history", params).await
    }

    pub async fn ocr_image(&self, image: &str) -> Result<Value> {
        self.send("ocr_image", json!({"image": image})).await
    }

    api!(get_group_system_msg, "get_group_system_msg");

    pub async fn get_essence_msg_list(&self, group_id: i64) -> Result<Value> {
        self.send("get_essence_msg_list", GroupId { group_id })
            .await
    }

    pub async fn get_group_at_all_remain(&self, group_id: i64) -> Result<Value> {
        self.send("get_group_at_all_remain", GroupId { group_id })
            .await
    }

    pub async fn set_group_portrait(&self, group_id: i64, file: &str) -> Result<Value> {
        self.send(
            "set_group_portrait",
            json!({"group_id": group_id, "file": file}),
        )
        .await
    }

    pub async fn set_essence_msg(&self, message_id: i64) -> Result<Value> {
        self.send("set_essence_msg", MessageIdParam { message_id })
            .await
    }

    pub async fn delete_essence_msg(&self, message_id: i64) -> Result<Value> {
        self.send("delete_essence_msg", MessageIdParam { message_id })
            .await
    }

    pub async fn send_group_notice(&self, params: Value) -> Result<Value> {
        self.send("_send_group_notice", params).await
    }

    pub async fn get_group_notice(&self, group_id: i64) -> Result<Value> {
        self.send("_get_group_notice", GroupId { group_id }).await
    }

    pub async fn upload_group_file(&self, params: Value) -> Result<Value> {
        self.send("upload_group_file", params).await
    }

    pub async fn delete_group_file(&self, params: Value) -> Result<Value> {
        self.send("delete_group_file", params).await
    }

    pub async fn create_group_file_folder(&self, params: Value) -> Result<Value> {
        self.send("create_group_file_folder", params).await
    }

    pub async fn delete_group_folder(&self, params: Value) -> Result<Value> {
        self.send("delete_group_folder", params).await
    }

    pub async fn get_group_file_system_info(&self, group_id: i64) -> Result<Value> {
        self.send("get_group_file_system_info", GroupId { group_id })
            .await
    }

    pub async fn get_group_root_files(&self, group_id: i64) -> Result<Value> {
        self.send("get_group_root_files", GroupId { group_id })
            .await
    }

    pub async fn get_group_files_by_folder(&self, params: Value) -> Result<Value> {
        self.send("get_group_files_by_folder", params).await
    }

    pub async fn get_group_file_url(&self, params: Value) -> Result<Value> {
        self.send("get_group_file_url", params).await
    }

    pub async fn upload_private_file(&self, params: Value) -> Result<Value> {
        self.send("upload_private_file", params).await
    }

    pub async fn download_file(&self, params: Value) -> Result<Value> {
        self.send("download_file", params).await
    }

    pub async fn handle_quick_operation(&self, params: Value) -> Result<Value> {
        self.send(".handle_quick_operation", params).await
    }

    // NapCat
    pub async fn set_diy_online_status(&self, params: Value) -> Result<Value> {
        self.send("set_diy_online_status", params).await
    }

    pub async fn ark_share_peer(&self, params: Value) -> Result<Value> {
        self.send("ArkSharePeer", params).await
    }

    pub async fn ark_share_group(&self, params: Value) -> Result<Value> {
        self.send("ArkShareGroup", params).await
    }

    api!(get_robot_uin_range, "get_robot_uin_range");

    pub async fn set_online_status(&self, params: Value) -> Result<Value> {
        self.send("set_online_status", params).await
    }

    api!(get_friends_with_category, "get_friends_with_category");

    pub async fn set_qq_avatar(&self, file: &str) -> Result<Value> {
        self.send("set_qq_avatar", json!({"file": file})).await
    }

    pub async fn get_file(&self, file_id: &str) -> Result<FileResult> {
        self.send(
            "get_file",
            GetFile {
                file_id: file_id.into(),
            },
        )
        .await
    }

    pub async fn forward_friend_single_msg(&self, user_id: i64, message_id: i64) -> Result<Value> {
        self.send(
            "forward_friend_single_msg",
            json!({"user_id": user_id, "message_id": message_id}),
        )
        .await
    }

    pub async fn forward_group_single_msg(&self, group_id: i64, message_id: i64) -> Result<Value> {
        self.send(
            "forward_group_single_msg",
            json!({"group_id": group_id, "message_id": message_id}),
        )
        .await
    }

    pub async fn translate_en2zh(&self, words: &[String]) -> Result<Value> {
        self.send("translate_en2zh", json!({"words": words})).await
    }

    pub async fn set_msg_emoji_like(&self, message_id: i64, emoji_id: &str) -> Result<Value> {
        self.send(
            "set_msg_emoji_like",
            json!({"message_id": message_id, "emoji_id": emoji_id}),
        )
        .await
    }

    pub async fn send_forward_msg(&self, params: Value) -> Result<Value> {
        self.send("send_forward_msg", params).await
    }

    pub async fn mark_private_msg_as_read(&self, user_id: i64) -> Result<Value> {
        self.send("mark_private_msg_as_read", UserId { user_id })
            .await
    }

    pub async fn mark_group_msg_as_read(&self, group_id: i64) -> Result<Value> {
        self.send("mark_group_msg_as_read", GroupId { group_id })
            .await
    }

    pub async fn get_friend_msg_history(&self, params: Value) -> Result<Value> {
        self.send("get_friend_msg_history", params).await
    }

    pub async fn create_collection(&self, params: Value) -> Result<Value> {
        self.send("create_collection", params).await
    }

    pub async fn get_collection_list(&self, params: Value) -> Result<Value> {
        self.send("get_collection_list", params).await
    }

    pub async fn set_self_longnick(&self, longnick: &str) -> Result<Value> {
        self.send("set_self_longnick", json!({"longNick": longnick}))
            .await
    }

    api!(get_recent_contact, "get_recent_contact");
    api!(mark_all_as_read, "_mark_all_as_read");
    api!(get_profile_like, "get_profile_like");
    api!(fetch_custom_face, "fetch_custom_face");

    pub async fn fetch_emoji_like(&self, params: Value) -> Result<Value> {
        self.send("fetch_emoji_like", params).await
    }

    pub async fn set_input_status(&self, params: Value) -> Result<Value> {
        self.send("set_input_status", params).await
    }

    pub async fn get_group_info_ex(&self, group_id: i64) -> Result<Value> {
        self.send("get_group_info_ex", GroupId { group_id }).await
    }

    pub async fn get_group_detail_info(&self, group_id: i64) -> Result<Value> {
        self.send("get_group_detail_info", GroupId { group_id })
            .await
    }

    pub async fn get_group_ignore_add_request(&self, group_id: i64) -> Result<Value> {
        self.send("get_group_ignore_add_request", GroupId { group_id })
            .await
    }

    pub async fn del_group_notice(&self, params: Value) -> Result<Value> {
        self.send("_del_group_notice", params).await
    }

    pub async fn friend_poke(&self, user_id: i64) -> Result<Value> {
        self.send("friend_poke", UserId { user_id }).await
    }

    pub async fn group_poke(&self, group_id: i64, user_id: i64) -> Result<Value> {
        self.send("group_poke", GroupUser { group_id, user_id })
            .await
    }

    api!(nc_get_packet_status, "nc_get_packet_status");

    pub async fn nc_get_user_status(&self, user_id: i64) -> Result<Value> {
        self.send("nc_get_user_status", UserId { user_id }).await
    }

    api!(nc_get_rkey, "nc_get_rkey");

    pub async fn get_group_shut_list(&self, group_id: i64) -> Result<Value> {
        self.send("get_group_shut_list", GroupId { group_id }).await
    }

    pub async fn move_group_file(&self, params: Value) -> Result<Value> {
        self.send("move_group_file", params).await
    }

    pub async fn trans_group_file(&self, params: Value) -> Result<Value> {
        self.send("trans_group_file", params).await
    }

    pub async fn rename_group_file(&self, params: Value) -> Result<Value> {
        self.send("rename_group_file", params).await
    }

    pub async fn get_group_ignored_notifies(&self, group_id: i64) -> Result<Value> {
        self.send("get_group_ignored_notifies", GroupId { group_id })
            .await
    }

    pub async fn set_group_sign(&self, group_id: i64) -> Result<Value> {
        self.send("set_group_sign", GroupId { group_id }).await
    }

    pub async fn send_packet(&self, params: Value) -> Result<Value> {
        self.send("send_packet", params).await
    }

    pub async fn get_mini_app_ark(&self, params: Value) -> Result<Value> {
        self.send("get_mini_app_ark", params).await
    }

    pub async fn get_ai_record(&self, params: Value) -> Result<Value> {
        self.send("get_ai_record", params).await
    }

    pub async fn get_ai_characters(&self, params: Value) -> Result<Value> {
        self.send("get_ai_characters", params).await
    }

    pub async fn send_group_ai_record(&self, params: Value) -> Result<Value> {
        self.send("send_group_ai_record", params).await
    }

    api!(get_clientkey, "get_clientkey");

    pub async fn send_poke(&self, params: Value) -> Result<Value> {
        self.send("send_poke", params).await
    }

    pub async fn set_group_kick_members(&self, params: Value) -> Result<Value> {
        self.send("set_group_kick_members", params).await
    }

    pub async fn set_group_robot_add_option(&self, params: Value) -> Result<Value> {
        self.send("set_group_robot_add_option", params).await
    }

    pub async fn set_group_add_option(&self, params: Value) -> Result<Value> {
        self.send("set_group_add_option", params).await
    }

    pub async fn set_group_search(&self, params: Value) -> Result<Value> {
        self.send("set_group_search", params).await
    }

    api!(
        get_doubt_friends_add_request,
        "get_doubt_friends_add_request"
    );

    pub async fn set_doubt_friends_add_request(&self, params: Value) -> Result<Value> {
        self.send("set_doubt_friends_add_request", params).await
    }

    api!(get_rkey, "get_rkey");
    api!(get_rkey_server, "get_rkey_server");

    /// Escape hatch for actions not given a named wrapper.
    pub async fn call<P: Serialize>(&self, action: &str, params: P) -> Result<Value> {
        self.send(action, params).await
    }
}
