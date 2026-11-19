use std::sync::Arc;

use async_trait::async_trait;
use fujiang_core::{
    BotContext, Event, Flow, MessageEvent, Plugin, PluginScope, Segment, Sender, Source,
};

use super::AgentPlugin;

struct NoopMessenger;

#[async_trait]
impl fujiang_core::Messenger for NoopMessenger {
    async fn send(&self, _target: Source, _segs: &[Segment]) -> anyhow::Result<i64> {
        Ok(1)
    }
    async fn get_message(&self, _id: i64) -> anyhow::Result<MessageEvent> {
        anyhow::bail!("no")
    }
    async fn get_image(&self, _file: &str) -> anyhow::Result<std::path::PathBuf> {
        anyhow::bail!("no")
    }
    async fn get_file(&self, _file_id: &str) -> anyhow::Result<std::path::PathBuf> {
        anyhow::bail!("no")
    }
    async fn onebot(
        &self,
        _action: &str,
        _params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        anyhow::bail!("no")
    }
}

async fn test_ctx() -> BotContext {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "fujiang-agent-handle-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = fujiang_store::Store::open(dir.join("t.db").to_str().unwrap(), dir.join("img"))
        .await
        .unwrap();
    BotContext {
        messenger: Arc::new(tokio::sync::RwLock::new(
            Arc::new(NoopMessenger) as Arc<dyn fujiang_core::Messenger>
        )),
        store,
        http: reqwest::Client::new(),
        config: Arc::new(tokio::sync::RwLock::new(fujiang_core::BotConfig {
            command_prefix: ".".into(),
            groups: vec![],
            allow_private: true,
            clist_username: String::new(),
            clist_api_key: String::new(),
            clist_limit: 10,
            contest_update_minutes: 60,
            rank_update_minutes: 60,
            daily_reset_hour: 4,
            fun_allow_mutate: true,
            fun_admins: vec![],
            fun_delete_files: false,
        })),
        plugin_configs: Arc::new(tokio::sync::RwLock::new(Default::default())),
        services: fujiang_core::ServiceHub::new(),
        events: fujiang_core::EventBus::new(),
    }
}

fn msg(source: Source, user_id: i64, self_id: i64, text: &str) -> MessageEvent {
    MessageEvent {
        id: 1,
        time: 0,
        self_id,
        sender: Sender {
            user_id,
            nickname: None,
            card: None,
        },
        source,
        segments: vec![Segment::text(text)],
        raw_text: text.to_string(),
    }
}

async fn handle(ev: Event) -> Flow {
    let ctx = test_ctx().await;
    let scope = PluginScope::for_plugin("agent");
    AgentPlugin::default()
        .handle(&ctx, &ev, &scope)
        .await
        .unwrap()
}

#[tokio::test]
async fn self_message_is_ignored_even_in_private_chat() {
    let ev = Event::Message(msg(Source::Friend { id: 9 }, 9, 9, "今天天气"));
    assert_eq!(handle(ev).await, Flow::Continue);
}

#[tokio::test]
async fn non_message_and_group_plain_text_are_not_claimed() {
    assert_eq!(
        handle(Event::Meta {
            kind: "heartbeat".into()
        })
        .await,
        Flow::Continue
    );
    let ev = Event::Message(msg(Source::Group { id: 7 }, 42, 9, "今天天气"));
    assert_eq!(handle(ev).await, Flow::Continue);
}

#[tokio::test]
async fn agent_command_stops_before_a_chat_turn() {
    let ev = Event::Message(msg(Source::Friend { id: 42 }, 42, 9, ".agent status"));
    assert_eq!(handle(ev).await, Flow::Stop);
}

#[tokio::test]
async fn private_lai_zhi_is_left_for_fun() {
    for text in ["来只hmc", "来只 hmc"] {
        let ev = Event::Message(msg(Source::Friend { id: 42 }, 42, 9, text));
        assert_eq!(handle(ev).await, Flow::Continue, "{text}");
    }
}

#[tokio::test]
async fn private_learn_trigger_is_left_for_fun() {
    let ctx = test_ctx().await;
    ctx.store
        .learn_add("活着？", "打赢牢大啦！", 1, None)
        .await
        .unwrap();
    let ev = Event::Message(msg(Source::Friend { id: 42 }, 42, 9, "活着？"));
    let scope = fujiang_core::PluginScope::for_plugin("agent");
    let flow = super::AgentPlugin::default()
        .handle(&ctx, &ev, &scope)
        .await
        .unwrap();
    assert_eq!(flow, Flow::Continue);
}

#[tokio::test]
async fn private_plain_chat_is_still_claimed() {
    // No session pool, so answer() only sends the busy reply. It must still Stop.
    let ev = Event::Message(msg(Source::Friend { id: 42 }, 42, 9, "今天天气"));
    assert_eq!(handle(ev).await, Flow::Stop);
}
