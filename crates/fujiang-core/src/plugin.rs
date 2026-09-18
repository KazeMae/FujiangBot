use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

use crate::event::{Event, MessageEvent, Segment, Source};
use crate::events::{event_fn, EventBus, EventHandler, EventResult};
use crate::scope::PluginScope;
use crate::services::{Service, ServiceHub};
use crate::BotConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// What events a plugin wants after command routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interest {
    /// Only messages whose first token or prefix matches this plugin's commands.
    Commands,
    /// All allowed messages (and still receives its own commands first).
    Messages,
    /// Every event, including notice / request / meta.
    All,
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
    pub plugin_configs: Arc<RwLock<HashMap<String, Value>>>,
    pub services: ServiceHub,
    pub events: EventBus,
}

impl BotContext {
    pub async fn bot_config(&self) -> BotConfig {
        self.config.read().await.clone()
    }

    pub async fn plugin_config(&self, name: &str) -> Value {
        self.plugin_configs
            .read()
            .await
            .get(name)
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()))
    }

    /// Instance overlay on top of the type-level config (`rank-fresh` over `rank`).
    pub async fn instance_config(&self, scope: &PluginScope) -> Value {
        let kind = self.plugin_config(scope.plugin_kind()).await;
        if scope.is_primary() {
            return kind;
        }
        merge_json(kind, self.plugin_config(scope.instance_id()).await)
    }

    /// SQLite for this instance. Extra copies get `{kind}__{id}` unless `share_data = true`.
    pub async fn open_instance_db(&self, scope: &PluginScope) -> anyhow::Result<sqlx::SqlitePool> {
        self.open_plugin_db(&instance_data_key(
            scope,
            &self.instance_config(scope).await,
        ))
        .await
    }

    /// Register a named service. Revoked automatically when `scope` is disposed.
    pub fn provide(
        &self,
        scope: &PluginScope,
        name: &str,
        service: Arc<dyn Service>,
    ) -> anyhow::Result<()> {
        let owner = scope.plugin_name();
        anyhow::ensure!(!owner.is_empty(), "provide() needs a named PluginScope");
        let svc_name = service_name_for(scope, name);
        let gen = self.services.provide(&svc_name, owner, service)?;
        let hub = self.services.clone();
        let owner = owner.to_string();
        scope.defer(move || {
            hub.revoke(&svc_name, &owner, gen);
        });
        Ok(())
    }

    pub async fn call(&self, service: &str, method: &str, args: Value) -> anyhow::Result<Value> {
        self.services.call(service, method, args).await
    }

    /// Subscribe. Dropped when `scope` is disposed.
    pub fn listen(&self, scope: &PluginScope, event: &str, handler: Arc<dyn EventHandler>) -> u64 {
        self.listen_opts(scope, event, handler, false)
    }

    pub fn listen_opts(
        &self,
        scope: &PluginScope,
        event: &str,
        handler: Arc<dyn EventHandler>,
        prepend: bool,
    ) -> u64 {
        let plugin = scope.plugin_name();
        let id = self.events.on(event, plugin, handler, prepend);
        let bus = self.events.clone();
        scope.defer(move || {
            bus.off(id);
        });
        id
    }

    pub fn listen_fn<F, Fut>(&self, scope: &PluginScope, event: &str, f: F) -> u64
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<EventResult>> + Send + 'static,
    {
        self.listen(scope, event, event_fn(f))
    }

    pub fn emit(&self, event: impl Into<String>, payload: Value) {
        self.events.emit(event, payload);
    }

    /// `{data}/plugin-data/{name}/` for files this plugin owns.
    pub fn plugin_data_dir(&self, name: &str) -> anyhow::Result<PathBuf> {
        self.store.plugin_data_dir(name)
    }

    /// Private SQLite for this plugin. Do not put plugin tables in the host db.
    pub async fn open_plugin_db(&self, name: &str) -> anyhow::Result<sqlx::SqlitePool> {
        self.store.open_plugin_db(name).await
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

    pub async fn send_video(&self, target: Source, path: impl Into<String>) -> anyhow::Result<i64> {
        self.send_segments(
            target,
            vec![Segment::Video {
                src: crate::event::Media::Path(path.into()),
                name: None,
            }],
        )
        .await
    }

    /// Load media bytes. HTTP(S) URLs are downloaded; cache ids go through get_image, then get_file.
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
                anyhow::bail!("base64 media not supported here ({} bytes)", raw.len())
            }
            Media::Url(u) | Media::FileId(u) => {
                let m = self.messenger().await;
                let path = match m.get_image(u).await {
                    Ok(p) => p,
                    Err(_) => m.get_file(u).await?,
                };
                Ok(tokio::fs::read(&path).await?)
            }
        }
    }
}

fn is_http_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Bump when `Plugin` / `BotContext` layout or the create symbol changes.
pub const PLUGIN_ABI: u32 = 5;

fn merge_json(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Object(mut a), Value::Object(b)) => {
            for (k, v) in b {
                a.insert(k, v);
            }
            Value::Object(a)
        }
        (_, over) if over != Value::Null => over,
        (base, _) => base,
    }
}

pub fn service_name_for(scope: &PluginScope, name: &str) -> String {
    if scope.is_primary() {
        name.to_string()
    } else {
        format!("{name}#{}", scope.instance_id())
    }
}

pub fn instance_data_key(scope: &PluginScope, cfg: &Value) -> String {
    if scope.is_primary()
        || cfg
            .get("share_data")
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
    {
        scope.plugin_kind().to_string()
    } else {
        format!("{}__{}", scope.plugin_kind(), scope.instance_id())
    }
}

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

    /// Prefix matches for commands that eat the rest of the token (`.remind08:30`).
    fn command_prefixes(&self) -> &'static [&'static str] {
        &[]
    }

    fn interest(&self) -> Interest {
        Interest::Commands
    }

    /// Service names this plugin needs before `on_start`. Overlay: `inject` in plugin JSON.
    fn inject(&self) -> &'static [&'static str] {
        &[]
    }

    /// Service names this plugin will `ctx.provide` in `on_start`. Overlay: `provides`.
    fn provides(&self) -> &'static [&'static str] {
        &[]
    }

    async fn on_start(&self, _ctx: &BotContext, _scope: &PluginScope) -> anyhow::Result<()> {
        Ok(())
    }

    /// Called after the scope has been disposed (background tasks cancelled).
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
