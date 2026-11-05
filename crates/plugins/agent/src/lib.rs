//! Chat plugin. A hit calls the LLM, replies, and returns [`Flow::Stop`].

mod client;
mod config;
mod session;
mod trigger;

pub use client::{ChatMessage, LlmClient, LlmError, Role};
pub use config::{AgentConfig, RateLimit};

use async_trait::async_trait;
use fujiang_core::{
    declare_plugin, BotContext, Event, Flow, Interest, MessageEvent, Plugin, PluginMeta,
    PluginScope,
};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::warn;

use crate::session::{recent_turns, save_exchange, session_key};
use crate::trigger::{build_messages, claim, fit_reply, llm_reply, Claim, BUSY_REPLY};

const HELP: &str = "对话。@我、消息里带唤醒词（默认「福酱」），或回复我的消息，我会接话。\n\
同一群共用一份上下文，私聊各自一份。\n\
在配置里把 priority 设为 10，高于 fun。本插件接过的消息返回 Stop，fun 不会再处理同一条。\n\
require_at 为 true 时，只认 @我或回复我，单独的唤醒词不算。";

pub struct AgentPlugin {
    pool: Mutex<Option<SqlitePool>>,
}

impl Default for AgentPlugin {
    fn default() -> Self {
        Self {
            pool: Mutex::new(None),
        }
    }
}

declare_plugin!(AgentPlugin);

#[async_trait]
impl Plugin for AgentPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta::new("agent", "被叫到就接话，并拦住后面的插件", self.commands())
    }

    fn name(&self) -> &'static str {
        "agent"
    }

    fn help(&self) -> &'static str {
        HELP
    }

    fn interest(&self) -> Interest {
        Interest::Messages
    }

    async fn on_start(&self, ctx: &BotContext, scope: &PluginScope) -> anyhow::Result<()> {
        let pool = ctx.open_instance_db(scope).await?;
        session::ensure_schema(&pool).await?;
        *self.pool.lock().await = Some(pool);
        Ok(())
    }

    async fn on_stop(&self) -> anyhow::Result<()> {
        if let Some(pool) = self.pool.lock().await.take() {
            pool.close().await;
        }
        Ok(())
    }

    async fn handle(
        &self,
        ctx: &BotContext,
        ev: &Event,
        scope: &PluginScope,
    ) -> anyhow::Result<Flow> {
        let Some(msg) = ev.as_message() else {
            return Ok(Flow::Continue);
        };
        // Outgoing text from this account is not a new question.
        if msg.sender.user_id == msg.self_id {
            return Ok(Flow::Continue);
        }
        let cfg = AgentConfig::from_value(&ctx.instance_config(scope).await);
        let reply_from_self = quoted_bot(ctx, msg).await;
        let Claim::Hit { user_text } = claim(msg, &cfg.wake_words, cfg.require_at, reply_from_self)
        else {
            return Ok(Flow::Continue);
        };
        self.answer(ctx, msg, &cfg, user_text).await;
        Ok(Flow::Stop)
    }
}

impl AgentPlugin {
    async fn answer(
        &self,
        ctx: &BotContext,
        msg: &MessageEvent,
        cfg: &AgentConfig,
        user_text: String,
    ) {
        let Some(pool) = self.pool.lock().await.clone() else {
            say(ctx, msg, BUSY_REPLY).await;
            return;
        };
        let key = session_key(msg.source);
        let history = match recent_turns(&pool, &key, cfg.max_history_turns).await {
            Ok(history) => history,
            Err(err) => {
                warn!(error = %err, "agent history");
                say(ctx, msg, BUSY_REPLY).await;
                return;
            }
        };
        let client = match LlmClient::from_config(cfg, ctx.http.clone()) {
            Ok(client) => client,
            Err(err) => {
                say(ctx, msg, llm_reply(&err)).await;
                return;
            }
        };
        let messages = build_messages(&cfg.system_prompt, history, user_text.clone());
        let reply = match client.chat_completions(&messages).await {
            Ok(reply) => reply,
            Err(err) => {
                say(ctx, msg, llm_reply(&err)).await;
                return;
            }
        };
        let visible = fit_reply(&reply);
        if visible.is_empty() {
            say(ctx, msg, BUSY_REPLY).await;
            return;
        }
        if let Err(err) = ctx.reply_text(msg, &visible).await {
            warn!(error = %err, "agent reply");
            return;
        }
        if let Err(err) = save_exchange(&pool, &key, msg.user_id(), &user_text, &visible).await {
            warn!(error = %err, "agent save");
        }
    }
}

async fn quoted_bot(ctx: &BotContext, msg: &MessageEvent) -> bool {
    let Some(id) = msg.reply_id() else {
        return false;
    };
    match ctx.messenger().await.get_message(id).await {
        // sender.user_id is the author. self_id is this account on every event.
        Ok(prev) => prev.sender.user_id == msg.self_id,
        Err(_) => false,
    }
}

async fn say(ctx: &BotContext, msg: &MessageEvent, text: impl Into<String>) {
    if let Err(err) = ctx.reply_text(msg, text).await {
        warn!(error = %err, "agent reply");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_no_commands_and_asks_for_priority() {
        let plugin = AgentPlugin::default();
        assert_eq!(plugin.name(), "agent");
        assert!(plugin.commands().is_empty());
        assert_eq!(plugin.interest(), Interest::Messages);
        assert!(plugin.help().contains("priority"));
        assert!(plugin.help().contains("10"));
        assert!(plugin.help().contains("Stop"));
    }
}
