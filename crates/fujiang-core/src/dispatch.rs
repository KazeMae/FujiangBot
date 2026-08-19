use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{error, info};

use crate::event::Event;
use crate::plugin::{BotContext, Flow, Plugin};

pub struct Dispatcher {
    plugins: RwLock<Vec<Arc<dyn Plugin>>>,
}

impl Dispatcher {
    pub fn new(plugins: Vec<Arc<dyn Plugin>>) -> Self {
        Self {
            plugins: RwLock::new(plugins),
        }
    }

    pub async fn names(&self) -> Vec<String> {
        self.plugins
            .read()
            .await
            .iter()
            .map(|p| p.name().to_string())
            .collect()
    }

    pub async fn help_text(&self, prefix: &str) -> String {
        let mut s = format!("菜单（前缀 `{prefix}`，指令区分大小写）\n\n");
        for p in self.plugins.read().await.iter() {
            s.push_str(&format!("【{}】\n{}\n\n", p.name(), p.help().trim()));
        }
        s.push_str("直接发 .contest / .rank / .problem / .learn / .luck 也能看对应说明。");
        s
    }

    pub async fn start_all(&self, ctx: &BotContext) -> anyhow::Result<()> {
        for p in self.plugins.read().await.iter() {
            info!(plugin = p.name(), "starting plugin");
            p.on_start(ctx).await?;
        }
        Ok(())
    }

    pub async fn stop_all(&self) {
        let plugins = self.plugins.read().await.clone();
        for p in plugins {
            if let Err(e) = p.on_stop().await {
                error!(plugin = p.name(), error = %e, "plugin stop");
            }
        }
    }

    pub async fn insert(&self, plugin: Arc<dyn Plugin>, ctx: &BotContext) -> anyhow::Result<()> {
        let name = plugin.name();
        {
            let guard = self.plugins.read().await;
            if guard.iter().any(|p| p.name() == name) {
                return Ok(());
            }
        }
        info!(plugin = name, "hot-enable plugin");
        plugin.on_start(ctx).await?;
        let mut guard = self.plugins.write().await;
        if guard.iter().any(|p| p.name() == name) {
            drop(guard);
            plugin.on_stop().await?;
            return Ok(());
        }
        guard.push(plugin);
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> anyhow::Result<bool> {
        let mut guard = self.plugins.write().await;
        if let Some(i) = guard.iter().position(|p| p.name() == name) {
            let p = guard.remove(i);
            drop(guard);
            info!(plugin = name, "hot-disable plugin");
            p.on_stop().await?;
            return Ok(true);
        }
        Ok(false)
    }

    pub async fn handle(&self, ctx: &BotContext, ev: &Event) {
        if let Some(msg) = ev.as_message() {
            let cfg = ctx.bot_config().await;
            if !cfg.allowed(msg.source) {
                return;
            }
            if let Err(e) = persist_message(ctx, msg).await {
                error!(error = %e, message_id = msg.id, "persist message");
            }
            let line = msg.command_line();
            if line == format!("{}help", cfg.command_prefix) {
                if let Err(e) = ctx
                    .reply_text(msg, self.help_text(&cfg.command_prefix).await)
                    .await
                {
                    error!(error = %e, "send help");
                }
                return;
            }
        } else if let Event::Request(req) = ev {
            if req.kind == "friend" {
                // ignore
            }
        }

        let plugins = self.plugins.read().await.clone();
        for p in plugins {
            match p.handle(ctx, ev).await {
                Ok(Flow::Stop) => return,
                Ok(Flow::Continue) => {}
                Err(e) => {
                    error!(plugin = p.name(), error = %e, "plugin error");
                }
            }
        }
    }
}

async fn persist_message(ctx: &BotContext, msg: &crate::event::MessageEvent) -> anyhow::Result<()> {
    let segments_json = serde_json::to_string(&msg.segments).unwrap_or_else(|_| "[]".into());
    ctx.store
        .insert_message(&fujiang_store::MessageLog {
            message_id: msg.id,
            time: msg.time,
            self_id: msg.self_id,
            user_id: msg.user_id(),
            group_id: msg.source.group_id(),
            nickname: msg.sender.nickname.clone(),
            card: msg.sender.card.clone(),
            raw_text: msg.raw_text.clone(),
            segments_json,
        })
        .await
}
