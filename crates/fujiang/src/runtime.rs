use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use fujiang_adapter::{AnyAdapter, HttpAdapter, WsAdapter};
use fujiang_core::{BotContext, Dispatcher, Event, Gateway};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::{AdapterBackend, AppConfig};
use crate::dynload::PluginHub;
use crate::plugins;

pub struct AppState {
    pub config_path: PathBuf,
    pub cfg: RwLock<AppConfig>,
    pub ctx: BotContext,
    pub dispatcher: Arc<Dispatcher>,
    pub hub: Arc<PluginHub>,
    pub event_tx: mpsc::Sender<Event>,
    gateway: Mutex<Option<GatewaySlot>>,
    apply_lock: Mutex<()>,
}

struct GatewaySlot {
    stop: CancellationToken,
    task: JoinHandle<()>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ApplyReport {
    pub applied: Vec<String>,
    pub restart_required: Vec<String>,
    pub plugins: Vec<String>,
}

impl AppState {
    pub fn new(
        config_path: PathBuf,
        cfg: AppConfig,
        ctx: BotContext,
        dispatcher: Arc<Dispatcher>,
        hub: Arc<PluginHub>,
        event_tx: mpsc::Sender<Event>,
    ) -> Self {
        Self {
            config_path,
            cfg: RwLock::new(cfg),
            ctx,
            dispatcher,
            hub,
            event_tx,
            gateway: Mutex::new(None),
            apply_lock: Mutex::new(()),
        }
    }

    pub async fn start_gateway(&self, adapter: Arc<AnyAdapter>) {
        let slot = spawn_gateway(adapter, self.event_tx.clone());
        *self.gateway.lock().await = Some(slot);
    }

    pub async fn stop_gateway(&self) {
        if let Some(slot) = self.gateway.lock().await.take() {
            slot.stop.cancel();
            let _ = slot.task.await;
        }
    }

    pub async fn apply(&self, incoming: AppConfig) -> anyhow::Result<ApplyReport> {
        let _guard = self.apply_lock.lock().await;
        let old = self.cfg.read().await.clone();
        let mut next = AppConfig::merge_secrets(&old, incoming);
        if next.plugins.configs.is_empty() {
            next.plugins.configs = old.plugins.configs.clone();
        }
        validate_config(&next)?;

        let mut report = ApplyReport::default();
        if old.store.db != next.store.db {
            report.restart_required.push("store.db".into());
        }
        if old.store.image_root != next.store.image_root {
            report.restart_required.push("store.image_root".into());
        }
        if old.store.archive_dir != next.store.archive_dir {
            report.restart_required.push("store.archive_dir".into());
        }
        if old.admin.listen != next.admin.listen {
            report.restart_required.push("admin.listen".into());
        }
        if old.plugins.dir != next.plugins.dir {
            report.restart_required.push("plugins.dir".into());
        }
        if old.plugins.watch != next.plugins.watch {
            report.restart_required.push("plugins.watch".into());
        }

        *self.ctx.config.write().await = next.to_bot_config();
        *self.ctx.plugin_configs.write().await = plugins::all_configs(&next);
        report.applied.push("bot".into());

        self.ctx.store.set_archive_params(
            next.store.archive_after_days,
            next.store.archive_every_hours,
        );
        report.applied.push("store.archive".into());

        sync_plugins(&self.dispatcher, &self.ctx, &old, &next).await?;
        report.applied.push("plugins".into());

        if adapter_changed(&old, &next) {
            let adapter = Arc::new(build_adapter(&next)?);
            *self.ctx.messenger.write().await = adapter.messenger();
            self.restart_gateway(adapter).await;
            report.applied.push("adapter".into());
        }

        next.save(&self.config_path)
            .with_context(|| format!("write {}", self.config_path.display()))?;
        report.applied.push("file".into());

        report.plugins = self.dispatcher.names().await;
        *self.cfg.write().await = next;
        info!(applied = ?report.applied, restart = ?report.restart_required, "config applied");
        Ok(report)
    }

    pub async fn apply_plugin_config(
        &self,
        name: &str,
        value: serde_json::Value,
    ) -> anyhow::Result<ApplyReport> {
        let _guard = self.apply_lock.lock().await;
        let mut next = self.cfg.read().await.clone();
        plugins::apply_config(&mut next, name, value)?;
        validate_config(&next)?;

        *self.ctx.config.write().await = next.to_bot_config();
        *self.ctx.plugin_configs.write().await = plugins::all_configs(&next);

        restart_one(&self.dispatcher, &self.hub, &self.ctx, &next, name).await?;

        next.save(&self.config_path)
            .with_context(|| format!("write {}", self.config_path.display()))?;
        *self.cfg.write().await = next;
        Ok(ApplyReport {
            applied: vec![format!("plugins.{name}")],
            restart_required: vec![],
            plugins: self.dispatcher.names().await,
        })
    }

    async fn restart_gateway(&self, adapter: Arc<AnyAdapter>) {
        if let Some(old) = self.gateway.lock().await.take() {
            old.stop.cancel();
            let _ = old.task.await;
        }
        self.start_gateway(adapter).await;
    }
}

async fn sync_plugins(
    dispatcher: &Dispatcher,
    ctx: &BotContext,
    old: &AppConfig,
    new: &AppConfig,
) -> anyhow::Result<()> {
    let running = dispatcher.names().await;
    for name in plugins::NAMES {
        let want = plugins::enabled(new, name);
        let is_on = running.iter().any(|n| n == name);
        let restart = want && is_on && plugins::needs_restart(name, old, new);
        if is_on && (!want || restart) {
            dispatcher.remove(name).await?;
        }
        if want && (!is_on || restart) {
            if let Some(p) = plugins::make(name) {
                dispatcher.insert(p, ctx).await?;
            }
        }
    }
    Ok(())
}

async fn restart_one(
    dispatcher: &Dispatcher,
    hub: &crate::dynload::PluginHub,
    ctx: &BotContext,
    cfg: &AppConfig,
    name: &str,
) -> anyhow::Result<()> {
    if plugins::NAMES.contains(&name) {
        dispatcher.remove(name).await?;
        if plugins::enabled(cfg, name) {
            if let Some(p) = plugins::make(name) {
                dispatcher.insert(p, ctx).await?;
            }
        }
        return Ok(());
    }
    let running = dispatcher.names().await.iter().any(|n| n == name);
    if running {
        dispatcher.remove(name).await?;
        if let Some(p) = hub.plugin(name).await {
            dispatcher.insert(p, ctx).await?;
        }
    }
    Ok(())
}

pub fn build_adapter(cfg: &AppConfig) -> anyhow::Result<AnyAdapter> {
    let token = cfg.access_token().to_string();
    match cfg.adapter.backend {
        AdapterBackend::NapcatWs | AdapterBackend::LlonebotWs => {
            info!(backend = ?cfg.adapter.backend, ws = cfg.ws_url(), "using onebot websocket");
            Ok(AnyAdapter::Ws(Arc::new(WsAdapter::new(
                cfg.ws_url().to_string(),
                token,
            ))))
        }
        AdapterBackend::LlonebotHttp => {
            cfg.adapter
                .event_listen
                .parse::<std::net::SocketAddr>()
                .with_context(|| format!("adapter.event_listen `{}`", cfg.adapter.event_listen))?;
            info!(
                http_api = %cfg.adapter.http_api,
                event_listen = %cfg.adapter.event_listen,
                "using llonebot http"
            );
            Ok(AnyAdapter::Http(Arc::new(HttpAdapter::new(
                cfg.adapter.http_api.clone(),
                cfg.adapter.event_listen.clone(),
                token,
            )?)))
        }
    }
}

fn adapter_changed(old: &AppConfig, new: &AppConfig) -> bool {
    old.adapter.backend != new.adapter.backend
        || old.ws_url() != new.ws_url()
        || old.access_token() != new.access_token()
        || old.adapter.http_api != new.adapter.http_api
        || old.adapter.event_listen != new.adapter.event_listen
}

fn validate_config(cfg: &AppConfig) -> anyhow::Result<()> {
    if cfg.bot.command_prefix.is_empty() {
        anyhow::bail!("bot.command_prefix 不能为空");
    }
    if cfg.plugins.problem.daily_reset_hour > 23 {
        anyhow::bail!("plugins.problem.daily_reset_hour 必须是 0..=23");
    }
    if cfg.store.archive_every_hours == 0 {
        anyhow::bail!("store.archive_every_hours 至少为 1");
    }
    cfg.admin
        .listen
        .parse::<std::net::SocketAddr>()
        .with_context(|| format!("admin.listen `{}`", cfg.admin.listen))?;
    if cfg.adapter.backend == AdapterBackend::LlonebotHttp
        && cfg.adapter.event_listen == cfg.admin.listen
    {
        anyhow::bail!("admin.listen 不能和 adapter.event_listen 相同");
    }
    let local = cfg.admin.listen.starts_with("127.0.0.1")
        || cfg.admin.listen.starts_with("[::1]")
        || cfg.admin.listen.starts_with("localhost");
    if !local && cfg.admin.token.is_empty() {
        warn!(listen = %cfg.admin.listen, "admin listens non-locally without token");
    }
    Ok(())
}

fn spawn_gateway(adapter: Arc<AnyAdapter>, tx: mpsc::Sender<Event>) -> GatewaySlot {
    let stop = CancellationToken::new();
    let stop2 = stop.clone();
    let task = tokio::spawn(async move {
        if let Err(e) = adapter.run(tx, stop2).await {
            tracing::error!(error = %e, "gateway stopped");
        }
    });
    GatewaySlot { stop, task }
}
