//! DeepSeek chat client plugin. Group messages are not handled yet.

mod client;
mod config;

pub use client::{ChatMessage, LlmClient, LlmError, Role};
pub use config::{AgentConfig, RateLimit};

use async_trait::async_trait;
use fujiang_core::{
    declare_plugin, BotContext, Event, Flow, Interest, Plugin, PluginMeta, PluginScope,
};

const HELP: &str = "M1 WIP：OpenAI 兼容的对话客户端已接好，群聊触发在后续版本。当前不处理消息。";

#[derive(Default)]
pub struct AgentPlugin;

declare_plugin!(AgentPlugin);

#[async_trait]
impl Plugin for AgentPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta::new("agent", "M1 WIP：对话客户端，群聊尚未接入", self.commands())
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

    async fn handle(
        &self,
        _ctx: &BotContext,
        _ev: &Event,
        _scope: &PluginScope,
    ) -> anyhow::Result<Flow> {
        Ok(Flow::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_claims_no_commands() {
        let plugin = AgentPlugin;
        assert_eq!(plugin.name(), "agent");
        assert!(plugin.commands().is_empty());
        assert_eq!(plugin.interest(), Interest::Messages);
        assert!(plugin.help().contains("M1"));
    }
}
