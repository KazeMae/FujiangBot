use async_trait::async_trait;
use fujiang_core::{declare_plugin, BotContext, Event, Flow, Plugin, PluginMeta};

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
.ping     回复 pong"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[".ping"]
    }

    async fn handle(&self, ctx: &BotContext, ev: &Event) -> anyhow::Result<Flow> {
        let Some(msg) = ev.as_message() else {
            return Ok(Flow::Continue);
        };
        if msg.command_line() == ".ping" {
            ctx.reply_text(msg, "pong").await?;
            return Ok(Flow::Stop);
        }
        Ok(Flow::Continue)
    }
}
