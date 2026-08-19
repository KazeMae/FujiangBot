use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{error, info};

use crate::event::Event;
use crate::plugin::{BotContext, Flow, Plugin, PluginMeta};
use crate::scope::PluginScope;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginSnapshot {
    pub name: String,
    pub version: String,
    pub description: String,
    pub commands: Vec<String>,
}

impl PluginSnapshot {
    pub fn from_plugin(p: &dyn Plugin) -> Self {
        let PluginMeta {
            name,
            version,
            description,
            commands,
        } = p.meta();
        Self {
            name: name.into(),
            version: version.into(),
            description: description.into(),
            commands: commands.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

struct PluginSlot {
    plugin: Arc<dyn Plugin>,
    scope: Arc<PluginScope>,
}

pub struct Dispatcher {
    plugins: RwLock<Vec<PluginSlot>>,
}

impl Dispatcher {
    pub fn new(plugins: Vec<Arc<dyn Plugin>>) -> Self {
        Self {
            plugins: RwLock::new(
                plugins
                    .into_iter()
                    .map(|plugin| PluginSlot {
                        plugin,
                        scope: Arc::new(PluginScope::new()),
                    })
                    .collect(),
            ),
        }
    }

    pub async fn names(&self) -> Vec<String> {
        self.plugins
            .read()
            .await
            .iter()
            .map(|s| s.plugin.name().to_string())
            .collect()
    }

    pub async fn info_list(&self) -> Vec<PluginSnapshot> {
        self.plugins
            .read()
            .await
            .iter()
            .map(|s| PluginSnapshot::from_plugin(s.plugin.as_ref()))
            .collect()
    }

    pub async fn help_text(&self, prefix: &str) -> String {
        let mut s = format!("菜单（前缀 `{prefix}`，指令区分大小写）\n\n");
        for slot in self.plugins.read().await.iter() {
            s.push_str(&format!(
                "【{}】\n{}\n\n",
                slot.plugin.name(),
                slot.plugin.help().trim()
            ));
        }
        s.push_str("发各插件自己的入口命令（如 .contest、.ping）也能看对应说明。");
        s
    }

    pub async fn start_all(&self, ctx: &BotContext) -> anyhow::Result<()> {
        for slot in self.plugins.read().await.iter() {
            info!(plugin = slot.plugin.name(), "starting plugin");
            if let Err(e) = slot.plugin.on_start(ctx, &slot.scope).await {
                slot.scope.dispose().await;
                return Err(e);
            }
        }
        Ok(())
    }

    pub async fn stop_all(&self) {
        let slots: Vec<_> = self.plugins.write().await.drain(..).collect();
        for slot in slots {
            if let Err(e) = slot.plugin.on_stop().await {
                error!(plugin = slot.plugin.name(), error = %e, "plugin stop");
            }
            slot.scope.dispose().await;
        }
    }

    pub async fn insert(&self, plugin: Arc<dyn Plugin>, ctx: &BotContext) -> anyhow::Result<()> {
        let name = plugin.name();
        {
            let guard = self.plugins.read().await;
            if guard.iter().any(|s| s.plugin.name() == name) {
                return Ok(());
            }
        }
        info!(plugin = name, "hot-enable plugin");
        let scope = Arc::new(PluginScope::new());
        if let Err(e) = plugin.on_start(ctx, &scope).await {
            scope.dispose().await;
            return Err(e);
        }
        let mut guard = self.plugins.write().await;
        if guard.iter().any(|s| s.plugin.name() == name) {
            drop(guard);
            if let Err(e) = plugin.on_stop().await {
                error!(plugin = name, error = %e, "plugin stop");
            }
            scope.dispose().await;
            return Ok(());
        }
        guard.push(PluginSlot { plugin, scope });
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> anyhow::Result<bool> {
        let mut guard = self.plugins.write().await;
        if let Some(i) = guard.iter().position(|s| s.plugin.name() == name) {
            let slot = guard.remove(i);
            drop(guard);
            info!(plugin = name, "hot-disable plugin");
            if let Err(e) = slot.plugin.on_stop().await {
                error!(plugin = name, error = %e, "plugin stop");
            }
            slot.scope.dispose().await;
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

        let slots: Vec<_> = self
            .plugins
            .read()
            .await
            .iter()
            .map(|s| (s.plugin.clone(), s.scope.clone()))
            .collect();
        for (p, scope) in slots {
            match p.handle(ctx, ev, &scope).await {
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
