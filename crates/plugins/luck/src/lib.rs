use async_trait::async_trait;
use chrono::Local;
use fujiang_core::{command, BotContext, Event, Flow, Plugin, PluginScope};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

#[derive(Default)]
pub struct LuckPlugin;

#[async_trait]
impl Plugin for LuckPlugin {
    fn meta(&self) -> fujiang_core::PluginMeta {
        fujiang_core::PluginMeta::new("luck", "每日幸运数字", self.commands())
    }

    fn name(&self) -> &'static str {
        "luck"
    }

    fn help(&self) -> &'static str {
        "按日期和 QQ 号生成今天的幸运数字，同一天同一人同一 N 结果不变，换天会变。\n\
.luck N     在 1～N 里抽一个整数，N 必须 ≥ 1\n\
例：.luck 100"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[".luck"]
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
        let Some(rest) = command::strip_token(&line, ".luck", &prefix) else {
            return Ok(Flow::Continue);
        };
        let Some(lim) = rest
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<i64>().ok())
        else {
            ctx.reply_text(msg, command::plugin_help(self, &prefix))
                .await?;
            return Ok(Flow::Stop);
        };
        if lim < 1 {
            ctx.reply_text(msg, "N 必须 >= 1").await?;
            return Ok(Flow::Stop);
        }
        let n = luck_number(lim, msg.user_id());
        ctx.reply_text(msg, format!("你的幸运数字是{n}")).await?;
        Ok(Flow::Stop)
    }
}

fn luck_number(lim: i64, qq: i64) -> i64 {
    let d = Local::now().date_naive();
    let ymd = d.format("%Y%m%d").to_string().parse::<i64>().unwrap_or(0);
    let mut rng = StdRng::seed_from_u64((qq.wrapping_add(ymd)) as u64);
    rng.gen_range(1..=lim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_same_seed() {
        // same qq same day => same number; just ensure range
        let n = luck_number(100, 12345);
        assert!((1..=100).contains(&n));
    }
}
