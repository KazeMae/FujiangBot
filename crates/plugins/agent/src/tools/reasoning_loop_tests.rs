use serde_json::json;

use super::*;

#[tokio::test]
async fn tool_loop_echoes_official_reasoning_on_the_followup() {
    let secret = "SECRET_COT_SHOULD_NOT_REACH_QQ loop";
    let script = Script::with_steps(vec![
        json!({
            "output": [
                {
                    "type": "reasoning",
                    "id": "rs_keep",
                    "summary": [],
                    "encrypted_content": "nope",
                    "content": [{"type": "reasoning_text", "text": secret}]
                },
                {
                    "type": "function_call",
                    "call_id": "call_rank",
                    "name": "rank_list",
                    "arguments": "{}"
                }
            ]
        }),
        json!({
            "output_text": "榜上没人",
            "output": [
                {
                    "type": "reasoning",
                    "content": [{"type": "reasoning_text", "text": "later-cot"}]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "榜上没人"}]
                }
            ]
        }),
    ]);
    let base = spawn_app(script.clone()).await;
    let client = test_client(&base);
    let tools = vec![ToolSpec::parse("rank.list", "当前榜", json!({})).unwrap()];
    let hub = Rec::default();
    let reply = reply_of(&client, &user("查榜"), &tools, BuiltinTools::OFF, 3, &hub)
        .await
        .unwrap();
    assert_eq!(reply, ToolReply::Text("榜上没人".into()));
    let bodies = script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    let input = bodies[1]["input"].as_array().unwrap();
    assert_eq!(input[1]["type"], "reasoning");
    assert_eq!(input[1]["content"][0]["text"], secret);
    assert!(input[1].get("encrypted_content").is_none());
    assert!(input[1].get("summary").is_none());
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[3]["type"], "function_call_output");
    assert!(!format!("{reply:?}").contains("SECRET_COT"));
}
