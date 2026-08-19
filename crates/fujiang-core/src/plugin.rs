use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

use crate::event::{Event, MessageEvent, Segment, Source};
use crate::scope::PluginScope;
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

    /// Load image bytes. HTTP(S) URLs are downloaded; cache ids go through get_image.
    pub async fn fetch_media_bytes(&self, media: &crate::event::Media) -> anyhow::Result<Vec<u8>> {
        use crate::event::Media;
        match media {
            Media::Url(u) if is_http_url(u) => {
                let resp = self.http.get(u).send().await?.error_for_status()?;
                Ok(resp.bytes().await?.to_vec())
            }
            Media::Path(p) => Ok(tokio::fs::read(p).await?),
            Media::Base64(s) => {
                let raw = s.strip_prefix("base64://").unwrap_or(s);
                anyhow::bail!("base64 image not supported here ({} bytes)", raw.len())
            }
            Media::Url(u) | Media::FileId(u) => {
                let path = self.messenger().await.get_image(u).await?;
                Ok(tokio::fs::read(&path).await?)
            }
        }
    }
}

fn is_http_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Bump when `Plugin` / `BotContext` layout or the create symbol changes.
pub const PLUGIN_ABI: u32 = 2;

#[derive(Debug, Clone, Copy)]
pub struct PluginMeta {
    pub name: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub commands: &'static [&'static str],
}

impl PluginMeta {
    pub fn new(
        name: &'static str,
        description: &'static str,
        commands: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            version: env!("CARGO_PKG_VERSION"),
            description,
            commands,
        }
    }
}

/// Export a type as a loadable cdylib.
///
/// The `.so` / `.dylib` must be built from this workspace (same rustc + fujiang-core).
/// `create` returns a thin pointer to `Box<dyn Plugin>`.
#[macro_export]
macro_rules! declare_plugin {
    ($ty:ty) => {
        #[no_mangle]
        pub extern "C" fn fujiang_plugin_abi() -> u32 {
            $crate::PLUGIN_ABI
        }

        #[no_mangle]
        pub unsafe extern "C" fn fujiang_create_plugin() -> *mut Box<dyn $crate::Plugin> {
            let plugin: Box<dyn $crate::Plugin> = Box::new(<$ty>::default());
            Box::into_raw(Box::new(plugin))
        }
    };
}

#[async_trait]
pub trait Plugin: Send + Sync {
    fn meta(&self) -> PluginMeta {
        PluginMeta::new(self.name(), "", self.commands())
    }

    fn name(&self) -> &'static str;
    fn help(&self) -> &'static str;
    fn commands(&self) -> &'static [&'static str] {
        &[]
    }

    async fn on_start(&self, _ctx: &BotContext, _scope: &PluginScope) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_stop(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn handle(
        &self,
        ctx: &BotContext,
        ev: &Event,
        scope: &PluginScope,
    ) -> anyhow::Result<Flow>;
}
