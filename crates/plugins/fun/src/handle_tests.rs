use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fujiang_core::{
    BotContext, Event, Flow, MessageEvent, Plugin, PluginScope, Segment, Sender, Source,
};

use super::FunPlugin;

struct RecMessenger {
    sent: Mutex<Vec<Vec<Segment>>>,
}

#[async_trait]
impl fujiang_core::Messenger for RecMessenger {
    async fn send(&self, _target: Source, segs: &[Segment]) -> anyhow::Result<i64> {
        self.sent.lock().unwrap().push(segs.to_vec());
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

async fn test_ctx(messenger: Arc<RecMessenger>) -> BotContext {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "fujiang-fun-handle-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = fujiang_store::Store::open(dir.join("t.db").to_str().unwrap(), dir.join("img"))
        .await
        .unwrap();
    BotContext {
        messenger: Arc::new(tokio::sync::RwLock::new(
            messenger as Arc<dyn fujiang_core::Messenger>,
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

fn msg(source: Source, text: &str) -> MessageEvent {
    MessageEvent {
        id: 1,
        time: 0,
        self_id: 9,
        sender: Sender {
            user_id: 42,
            nickname: None,
            card: None,
        },
        source,
        segments: vec![Segment::text(text)],
        raw_text: text.to_string(),
    }
}

async fn seed_tag(ctx: &BotContext, tag: &str) -> String {
    let img = ctx
        .store
        .image_upsert(b"\x89PNG\r\n\x1a\nfake-hmc", "png", 1)
        .await
        .unwrap();
    ctx.store.image_attach(&img.md5, tag, 1).await.unwrap();
    img.md5
}

fn sent_images(rec: &RecMessenger) -> usize {
    rec.sent
        .lock()
        .unwrap()
        .iter()
        .filter(|segs| {
            segs.iter()
                .any(|s| matches!(s, Segment::Image { .. } | Segment::Video { .. }))
        })
        .count()
}

#[tokio::test]
async fn private_lai_zhi_sends_image_with_or_without_space() {
    let rec = Arc::new(RecMessenger {
        sent: Mutex::new(Vec::new()),
    });
    let ctx = test_ctx(rec.clone()).await;
    seed_tag(&ctx, "hmc").await;
    let plugin = FunPlugin;
    let scope = PluginScope::for_plugin("fun");
    for text in ["来只hmc", "来只 hmc"] {
        rec.sent.lock().unwrap().clear();
        let ev = Event::Message(msg(Source::Friend { id: 42 }, text));
        let flow = plugin.handle(&ctx, &ev, &scope).await.unwrap();
        assert_eq!(flow, Flow::Stop, "{text}");
        assert_eq!(sent_images(&rec), 1, "{text}");
    }
}

#[tokio::test]
async fn private_learn_still_replies() {
    let rec = Arc::new(RecMessenger {
        sent: Mutex::new(Vec::new()),
    });
    let ctx = test_ctx(rec.clone()).await;
    ctx.store
        .learn_add("活着？", "打赢牢大啦！", 1, None)
        .await
        .unwrap();
    let ev = Event::Message(msg(Source::Friend { id: 42 }, "活着？"));
    let flow = FunPlugin
        .handle(&ctx, &ev, &PluginScope::for_plugin("fun"))
        .await
        .unwrap();
    assert_eq!(flow, Flow::Continue);
    let sent = rec.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    let text = sent[0]
        .iter()
        .filter_map(|s| match s {
            Segment::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(text, "打赢牢大啦！");
}
