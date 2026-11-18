use serde_json::{json, Value};

use super::*;
use crate::config::AgentConfig;

fn client() -> LlmClient {
    let mut cfg = AgentConfig::from_value(&json!({
        "model": "local-model",
        "context_budget": 80_000,
        "model_context_limit": 80_000
    }));
    cfg.base_url = "http://127.0.0.1:9".into();
    LlmClient::new(&cfg, reqwest::Client::new(), "k".into())
}

fn user(text: &str) -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content: text.into(),
        images: Vec::new(),
    }
}

fn tool<'a>(params: &'a Value) -> FunctionTool<'a> {
    FunctionTool {
        name: "rank_list",
        description: "榜",
        parameters: params,
    }
}

fn secret_fixture() -> String {
    json!({
        "id": "resp_1",
        "status": "completed",
        "output_text": "",
        "output": [
            {
                "type": "reasoning",
                "id": "rs_1",
                "status": "completed",
                "summary": [{"type": "summary_text", "text": "ignore"}],
                "encrypted_content": "ciphertext-not-for-input",
                "content": [
                    {"type": "reasoning_text", "text": "SECRET_COT_SHOULD_NOT_REACH_QQ first"}
                ]
            },
            {
                "type": "function_call",
                "id": "fc_1",
                "status": "completed",
                "call_id": "call_a",
                "name": "rank_list",
                "arguments": "{\"limit\":2}"
            }
        ]
    })
    .to_string()
}

#[test]
fn parse_and_echo_official_reasoning_without_unsupported_fields() {
    let parsed = parse_model_output(&secret_fixture()).unwrap();
    let ModelOutput::Calls {
        reasoning_texts,
        calls,
    } = parsed
    else {
        panic!("expected calls");
    };
    assert_eq!(
        reasoning_texts,
        vec!["SECRET_COT_SHOULD_NOT_REACH_QQ first".to_string()]
    );
    assert_eq!(calls[0].call_id, "call_a");
    assert_eq!(calls[0].name, "rank_list");
    let prior = PriorCall::from_call(&calls[0], r#"{"via":"rank.list"}"#)
        .with_reasoning_texts(reasoning_texts);
    let body = rendered_request(&client(), &[user("查榜")], &[prior], &[]);
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 4);
    assert_eq!(input[0]["role"], "user");
    assert_eq!(input[1]["type"], "reasoning");
    let keys: Vec<&str> = input[1]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, vec!["content", "type"]);
    assert_eq!(input[1]["content"][0]["type"], "reasoning_text");
    assert_eq!(
        input[1]["content"][0]["text"],
        "SECRET_COT_SHOULD_NOT_REACH_QQ first"
    );
    assert!(input[1].get("summary").is_none());
    assert!(input[1].get("encrypted_content").is_none());
    assert!(input[1].get("id").is_none());
    assert!(input[1].get("status").is_none());
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_a");
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_a");
}

#[test]
fn missing_optional_fields_and_plain_text_content_still_parse() {
    let body = json!({
        "output": [
            {
                "type": "reasoning",
                "content": [
                    {"text": "part-a"},
                    {"type": "reasoning_text", "text": "part-b"},
                    "part-c",
                    {"type": "summary_text", "text": "skip-me"}
                ]
            },
            {
                "type": "reasoning",
                "content": "part-d"
            },
            {
                "type": "function_call",
                "call_id": "c1",
                "name": "rank_list"
            }
        ]
    })
    .to_string();
    let ModelOutput::Calls {
        reasoning_texts,
        calls,
    } = parse_model_output(&body).unwrap()
    else {
        panic!("expected calls");
    };
    assert_eq!(
        reasoning_texts,
        vec!["part-a", "part-b", "part-c", "part-d"]
    );
    assert_eq!(calls[0].arguments, "{}");
}

#[test]
fn multi_tool_keeps_one_reasoning_then_adjacent_call_output_pairs() {
    let first = PriorCall::bare("c1", "rank_list", "{}", r#"{"a":1}"#)
        .with_reasoning_texts(vec!["think-1".into(), "think-2".into()]);
    let second = PriorCall::bare("c2", "memo_list", "{}", r#"{"b":2}"#);
    let body = rendered_request(&client(), &[user("查")], &[first, second], &[]);
    let input = body["input"].as_array().unwrap();
    let types: Vec<&str> = input
        .iter()
        .map(|item| {
            item.get("type")
                .and_then(Value::as_str)
                .or_else(|| item.get("role").and_then(Value::as_str))
                .unwrap()
        })
        .collect();
    assert_eq!(
        types,
        [
            "user",
            "reasoning",
            "function_call",
            "function_call_output",
            "function_call",
            "function_call_output"
        ]
    );
    assert_eq!(input[1]["content"].as_array().unwrap().len(), 2);
    assert_eq!(input[2]["call_id"], "c1");
    assert_eq!(input[3]["call_id"], "c1");
    assert_eq!(input[4]["call_id"], "c2");
    assert_eq!(input[5]["call_id"], "c2");
    assert!(input[4].get("content").is_none());
}

#[test]
fn mixed_reasoning_and_message_does_not_show_secret_to_qq() {
    let body = json!({
        "output": [
            {
                "type": "reasoning",
                "content": [{"type": "reasoning_text", "text": "SECRET_COT_SHOULD_NOT_REACH_QQ hidden"}]
            },
            {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "今天还行"}]
            }
        ]
    });
    let text = text_from_response(&body).unwrap();
    assert_eq!(text, "今天还行");
    assert!(!text.contains("SECRET_COT_SHOULD_NOT_REACH_QQ"));
    let parsed = parse_model_output(&body.to_string()).unwrap();
    assert_eq!(parsed, ModelOutput::Text("今天还行".into()));
}

#[test]
fn reasoning_only_is_truncated_not_shown() {
    let body = json!({
        "output": [{
            "type": "reasoning",
            "content": [{"type": "reasoning_text", "text": "SECRET_COT_SHOULD_NOT_REACH_QQ only"}]
        }]
    });
    let err = text_from_response(&body).unwrap_err();
    assert!(matches!(err, LlmError::Truncated(_)));
    let err = parse_model_output(&body.to_string()).unwrap_err();
    assert!(matches!(err, LlmError::Truncated(_)));
}

#[test]
fn echoed_reasoning_counts_in_the_request_budget() {
    let params = json!({"type": "object", "properties": {}});
    let tool = tool(&params);
    let messages = [user("hi")];
    let bare = PriorCall::bare("c", "rank_list", "{}", "ok");
    let without = client()
        .request_bytes(
            &messages,
            std::slice::from_ref(&bare),
            std::slice::from_ref(&tool),
        )
        .unwrap();
    let secret = "SECRET_COT_SHOULD_NOT_REACH_QQ budget-marker-XXXXXXXX";
    let with = PriorCall::bare("c", "rank_list", "{}", "ok")
        .with_reasoning_texts(vec![secret.to_string()]);
    let grown = client()
        .request_bytes(
            &messages,
            std::slice::from_ref(&with),
            std::slice::from_ref(&tool),
        )
        .unwrap();
    let added = grown.saturating_sub(without);
    assert!(
        added >= u64::try_from(secret.len()).unwrap(),
        "budget grew by {added}, secret is {}",
        secret.len()
    );
    let body = rendered_request(&client(), &messages, std::slice::from_ref(&with), &[]);
    assert!(body.to_string().contains(secret));
}

#[test]
fn empty_reasoning_is_not_emitted() {
    let prior = PriorCall::bare("c", "rank_list", "{}", "ok").with_reasoning_texts(Vec::new());
    let body = rendered_request(&client(), &[user("hi")], &[prior], &[]);
    let input = body["input"].as_array().unwrap();
    assert_eq!(input[1]["type"], "function_call");
    assert!(input
        .iter()
        .all(|item| item.get("type") != Some(&json!("reasoning"))));
}
