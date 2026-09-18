use async_trait::async_trait;
use fujiang_core::{
    command, declare_plugin, BotContext, Event, EventResult, Flow, Plugin, PluginMeta, PluginScope,
};
use serde_json::json;
use tracing::info;

/// Example cdylib plugin. Build with `cargo build -p fujiang-plugin-echo`,
/// copy the `.dylib`/`.so` into `plugins/`, then load from the admin page or wait for watch.
#[derive(Default)]
pub struct EchoPlugin;

declare_plugin!(EchoPlugin);

#[async_trait]
impl Plugin for EchoPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta::new("echo", "示例热插插件：.ping → pong", self.commands())
    }

    fn name(&self) -> &'static str {
        "echo"
    }

    fn help(&self) -> &'static str {
        "示例动态插件。把编译出的 .so/.dylib 放到 plugins/ 即可热加载。\n\
.ping          回复 pong，并列出当前服务图\n\
.ping rank     调 rank.list（rank 插件提供的服务）"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[".ping"]
    }

    async fn on_start(&self, ctx: &BotContext, scope: &PluginScope) -> anyhow::Result<()> {
        ctx.listen_fn(scope, "rank.updated", |payload| async move {
            info!(%payload, "echo heard rank.updated");
            Ok(EventResult::Continue(None))
        });
        Ok(())
    }

    async fn handle(
        &self,
        ctx: &BotContext,
        ev: &Event,
        _scope: &PluginScope,
    ) -> anyhow::Result<Flow> {
        let Some(msg) = ev.as_message() else {
            return Ok(Flow::Continue);
        };
        let prefix = ctx.bot_config().await.command_prefix;
        let line = msg.command_line();
        let Some(rest) = command::strip_token(&line, ".ping", &prefix) else {
            return Ok(Flow::Continue);
        };
        if rest.trim() == "rank" {
            match ctx.call("rank", "list", json!({})).await {
                Ok(v) => {
                    let n = v.as_array().map(|a| a.len()).unwrap_or(0);
                    ctx.reply_text(msg, format!("rank.list → {n} 人")).await?;
                }
                Err(e) => {
                    ctx.reply_text(msg, format!("没有 rank 服务：{e:#}"))
                        .await?;
                }
            }
            return Ok(Flow::Stop);
        }
        let services = ctx.services.names();
        let line = if services.is_empty() {
            "pong".into()
        } else {
            format!("pong\nservices: {}", services.join(", "))
        };
        ctx.reply_text(msg, line).await?;
        Ok(Flow::Stop)
    }
}
