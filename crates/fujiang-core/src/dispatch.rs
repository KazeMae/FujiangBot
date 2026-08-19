use std::sync::Arc;

use tracing::{error, info};

use crate::event::Event;
use crate::plugin::{BotContext, Flow, Plugin};

pub struct Dispatcher {
    plugins: Vec<Arc<dyn Plugin>>,
}

impl Dispatcher {
    pub fn new(plugins: Vec<Arc<dyn Plugin>>) -> Self {
        Self { plugins }
    }

    pub fn help_text(&self, prefix: &str) -> String {
        let mut s = String::from("菜单：\n");
        for p in &self.plugins {
            s.push_str(&format!("【{}】\n{}\n\n", p.name(), p.help().trim()));
        }
        s.push_str(&format!("前缀 `{prefix}` ，指令区分大小写。"));
        s
    }

    pub async fn start_all(&self, ctx: &BotContext) -> anyhow::Result<()> {
        for p in &self.plugins {
            info!(plugin = p.name(), "starting plugin");
            p.on_start(ctx).await?;
        }
        Ok(())
    }

    pub async fn handle(&self, ctx: &BotContext, ev: &Event) {
        if let Some(msg) = ev.as_message() {
            if !ctx.config.allowed(msg.source) {
                return;
            }
            let line = msg.command_line();
            if line == format!("{}help", ctx.config.command_prefix) {
                if let Err(e) = ctx
                    .reply_text(msg, self.help_text(&ctx.config.command_prefix))
                    .await
                {
                    error!(error = %e, "send help");
                }
                return;
            }
        } else if let Event::Request(req) = ev {
            if req.kind == "friend" {
                // auto-approve is left to adapter if desired; ignore here
            }
        }

        for p in &self.plugins {
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
