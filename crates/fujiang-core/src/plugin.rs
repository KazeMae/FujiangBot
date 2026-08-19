use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

use crate::event::{Event, MessageEvent, Segment, Source};
use crate::BotConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

#[async_trait]
pub trait Messenger: Send + Sync {
    async fn send(&self, target: Source, segs: &[Segment]) -> anyhow::Result<i64>;
    async fn get_message(&self, id: i64) -> anyhow::Result<MessageEvent>;
    async fn get_image(&self, file: &str) -> anyhow::Result<std::path::PathBuf>;
    async fn get_file(&self, file_id: &str) -> anyhow::Result<std::path::PathBuf>;
}

#[async_trait]
pub trait Gateway: Send + Sync {
    async fn run(
        self: Arc<Self>,
        tx: mpsc::Sender<Event>,
        stop: CancellationToken,
    ) -> anyhow::Result<()>;
}

#[derive(Clone)]
pub struct BotContext {
    pub messenger: Arc<RwLock<Arc<dyn Messenger>>>,
    pub store: fujiang_store::Store,
    pub http: reqwest::Client,
    pub config: Arc<RwLock<BotConfig>>,
}

impl BotContext {
    pub async fn bot_config(&self) -> BotConfig {
        self.config.read().await.clone()
    }

    pub async fn messenger(&self) -> Arc<dyn Messenger> {
        self.messenger.read().await.clone()
    }

    pub async fn send_segments(&self, target: Source, segs: Vec<Segment>) -> anyhow::Result<i64> {
        self.messenger().await.send(target, &segs).await
    }

    pub async fn send_text(&self, target: Source, text: impl Into<String>) -> anyhow::Result<i64> {
        self.send_segments(target, vec![Segment::text(text)]).await
    }

    pub async fn reply_text(
        &self,
        ev: &MessageEvent,
        text: impl Into<String>,
    ) -> anyhow::Result<i64> {
        self.send_text(ev.source, text).await
    }

    pub async fn send_image(&self, target: Source, path: impl Into<String>) -> anyhow::Result<i64> {
        self.send_segments(
            target,
            vec![Segment::Image {
                src: crate::event::Media::Path(path.into()),
                summary: None,
            }],
        )
        .await
    }
}

#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &'static str;
    fn help(&self) -> &'static str;
    fn commands(&self) -> &'static [&'static str] {
        &[]
    }

    async fn on_start(&self, _ctx: &BotContext) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_stop(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn handle(&self, ctx: &BotContext, ev: &Event) -> anyhow::Result<Flow>;
}
