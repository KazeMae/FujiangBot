//! Chat plugin. `.agent` edits the session. A wake calls the LLM and returns [`Flow::Stop`].

mod client;
mod config;
mod session;
mod tools;
mod trigger;

pub use client::{ChatMessage, LlmClient, LlmError, Role};
pub use config::{AgentConfig, RateLimit};
pub use tools::ToolSpec;

use async_trait::async_trait;
use fujiang_core::{
    command, declare_plugin, BotContext, Event, Flow, Interest, MessageEvent, Plugin, PluginMeta,
    PluginScope,
};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::warn;

use crate::session::{save_exchange, session_key};
use crate::trigger::{build_messages, claim, fit_reply, llm_reply, Claim, BUSY_REPLY};

const HELP: &str = "对话。@我、消息里带唤醒词（默认「福酱」），或回复我的消息，我会接话。\n\
同一群共用一份上下文，私聊各自一份。\n\
.agent              本说明\n\
.agent reset        清空当前群或私聊的对话记录，人设保留\n\
.agent persona      查看当前人设\n\
.agent persona <名> 切换到配置里的人设\n\
.agent status       当前人设，以及这一小时还剩几次\n\
在配置里把 priority 设为 10，高于 fun。本插件接过的消息返回 Stop，fun 不会再处理同一条。\n\
require_at 为 true 时，只认 @我或回复我，单独的唤醒词不算。";

const USER_LIMIT_TIP: &str = "你这一小时里找福酱的次数到了，歇一会儿再来。";
const SESSION_LIMIT_TIP: &str = "这个会话这一小时的次数到了，歇一会儿再来。";

enum AgentCmd {
    Help,
    Reset,
    Status,
    ShowPersona,
    SetPersona(String),
    Unknown,
}

enum ChatPlan {
    Speak { messages: Vec<ChatMessage> },
    Tip(&'static str),
}

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

    fn commands(&self) -> &'static [&'static str] {
        &[".agent"]
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
        let prefix = ctx.bot_config().await.command_prefix;
        if let Some(rest) = command::strip_token(&msg.command_line(), ".agent", &prefix) {
            self.on_command(ctx, msg, &cfg, rest).await;
            return Ok(Flow::Stop);
        }
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
    async fn on_command(
        &self,
        ctx: &BotContext,
        msg: &MessageEvent,
        cfg: &AgentConfig,
        rest: &str,
    ) {
        let Some(pool) = self.pool.lock().await.clone() else {
            say(ctx, msg, BUSY_REPLY).await;
            return;
        };
        let prefix = ctx.bot_config().await.command_prefix;
        let cmd = parse_agent_cmd(rest);
        match apply_command(
            &pool,
            cfg,
            &session_key(msg.source),
            msg.user_id(),
            &cmd,
            &prefix,
            session::unix_now(),
        )
        .await
        {
            Ok(text) => say(ctx, msg, text).await,
            Err(err) => {
                warn!(error = %err, "agent command");
                say(ctx, msg, BUSY_REPLY).await;
            }
        }
    }

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
        // A missing key returns here and does not consume a slot.
        let client = match LlmClient::from_config(cfg, ctx.http.clone()) {
            Ok(client) => client,
            Err(err) => {
                say(ctx, msg, llm_reply(&err)).await;
                return;
            }
        };
        let messages = match plan_chat(
            &pool,
            cfg,
            &key,
            msg.user_id(),
            &user_text,
            session::unix_now(),
        )
        .await
        {
            Ok(ChatPlan::Tip(tip)) => {
                say(ctx, msg, tip).await;
                return;
            }
            Ok(ChatPlan::Speak { messages }) => messages,
            Err(err) => {
                warn!(error = %err, "agent chat");
                say(ctx, msg, BUSY_REPLY).await;
                return;
            }
        };
        let reply =
            match tools::run_tool_loop(&client, &messages, &cfg.tools, cfg.max_tool_rounds, ctx)
                .await
            {
                Ok(tools::ToolReply::Text(text)) => text,
                Ok(tools::ToolReply::Degraded(text)) => {
                    say(ctx, msg, text).await;
                    return;
                }
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

fn parse_agent_cmd(rest: &str) -> AgentCmd {
    let mut parts = rest.split_whitespace();
    let verb = parts.next().unwrap_or("");
    match verb {
        "" | "help" | "-h" | "--help" => AgentCmd::Help,
        "reset" => {
            if parts.next().is_some() {
                AgentCmd::Unknown
            } else {
                AgentCmd::Reset
            }
        }
        "status" => {
            if parts.next().is_some() {
                AgentCmd::Unknown
            } else {
                AgentCmd::Status
            }
        }
        "persona" => match parts.next() {
            None => AgentCmd::ShowPersona,
            Some(name) if parts.next().is_none() => AgentCmd::SetPersona(name.to_string()),
            Some(_) => AgentCmd::Unknown,
        },
        _ => AgentCmd::Unknown,
    }
}

async fn apply_command(
    pool: &SqlitePool,
    cfg: &AgentConfig,
    session_key: &str,
    user_id: i64,
    cmd: &AgentCmd,
    prefix: &str,
    now: i64,
) -> anyhow::Result<String> {
    match cmd {
        AgentCmd::Help => Ok(command::rewrite_help(HELP, &[".agent"], prefix)),
        AgentCmd::Unknown => {
            let shown = command::expand(".agent", prefix);
            Ok(format!("不认识这个子命令，发 {shown} 看说明"))
        }
        AgentCmd::Reset => {
            let n = session::clear_messages(pool, session_key).await?;
            if n == 0 {
                Ok("当前会话没有对话记录。".to_string())
            } else {
                Ok("已清空当前会话的对话记录。".to_string())
            }
        }
        AgentCmd::ShowPersona => {
            let stored = session::persona_of(pool, session_key).await?;
            let (title, prompt) = persona_line(cfg, stored.as_deref());
            Ok(format!("当前人设：{title}\n{}", fit_reply(prompt)))
        }
        AgentCmd::SetPersona(name) => {
            if cfg.prompt_for(name).is_none() {
                let known = cfg.persona_names().join("、");
                return Ok(format!("没有人设 {name}。可用：{known}。"));
            }
            session::set_persona(pool, session_key, name, now).await?;
            Ok(format!("已切换到人设 {name}。"))
        }
        AgentCmd::Status => {
            let stored = session::persona_of(pool, session_key).await?;
            let (title, _) = persona_line(cfg, stored.as_deref());
            let counts = session::usage_counts(pool, session_key, user_id, now).await?;
            let quota = quota_sentence(
                cfg.rate_limit.per_user_per_hour,
                counts.user,
                cfg.rate_limit.per_group_per_hour,
                counts.session,
            );
            Ok(format!("人设：{title}\n{quota}"))
        }
    }
}

/// History is loaded before the slot so a failed read does not burn quota.
async fn plan_chat(
    pool: &SqlitePool,
    cfg: &AgentConfig,
    session_key: &str,
    user_id: i64,
    user_text: &str,
    now: i64,
) -> anyhow::Result<ChatPlan> {
    let stored = session::persona_of(pool, session_key).await?;
    let (_, prompt) = cfg.resolve_persona(stored.as_deref());
    let history = session::recent_turns(pool, session_key, cfg.max_history_turns).await?;
    let slot = session::take_slot(
        pool,
        session_key,
        user_id,
        cfg.rate_limit.per_user_per_hour,
        cfg.rate_limit.per_group_per_hour,
        now,
    )
    .await?;
    Ok(match slot {
        session::Slot::Taken => ChatPlan::Speak {
            messages: build_messages(prompt, history, user_text.to_string()),
        },
        session::Slot::UserLimited => ChatPlan::Tip(USER_LIMIT_TIP),
        session::Slot::SessionLimited => ChatPlan::Tip(SESSION_LIMIT_TIP),
    })
}

fn persona_line<'a>(cfg: &'a AgentConfig, stored: Option<&str>) -> (String, &'a str) {
    let (effective, prompt) = cfg.resolve_persona(stored);
    let title = match stored {
        Some(saved) if saved != effective => {
            format!("{saved}，配置里没有，正在用 {effective}。")
        }
        _ => effective.to_string(),
    };
    (title, prompt)
}

fn seats_left(limit: u32, used: i64) -> Option<u32> {
    if limit == 0 {
        return None;
    }
    let used = u32::try_from(used.max(0)).unwrap_or(u32::MAX);
    Some(limit.saturating_sub(used))
}

fn quota_sentence(
    user_limit: u32,
    user_used: i64,
    session_limit: u32,
    session_used: i64,
) -> String {
    match (
        seats_left(user_limit, user_used),
        seats_left(session_limit, session_used),
    ) {
        (None, None) => "这一小时不限次数。".to_string(),
        (Some(user), None) => format!("你这一小时还剩 {user} 次，这个会话不限次数。"),
        (None, Some(session)) => format!("你这一小时不限次数，这个会话还剩 {session} 次。"),
        (Some(user), Some(session)) => {
            format!("你这一小时还剩 {user} 次，这个会话还剩 {session} 次。")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::session;
    use super::*;
    use fujiang_core::Source;
    use serde_json::json;

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        session::ensure_schema(&pool).await.unwrap();
        pool
    }

    fn cfg(user: u32, group: u32) -> AgentConfig {
        AgentConfig::from_value(&json!({
            "system_prompt": "默认人设",
            "personas": { "strict": { "system_prompt": "严格人设" } },
            "rate_limit": { "per_user_per_hour": user, "per_group_per_hour": group }
        }))
    }

    async fn usage_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn run(
        pool: &SqlitePool,
        cfg: &AgentConfig,
        key: &str,
        user: i64,
        cmd: &AgentCmd,
        now: i64,
    ) -> String {
        apply_command(pool, cfg, key, user, cmd, ".", now)
            .await
            .unwrap()
    }

    #[test]
    fn declares_agent_command_and_asks_for_priority() {
        let plugin = AgentPlugin::default();
        assert_eq!(plugin.name(), "agent");
        assert_eq!(plugin.commands(), [".agent"]);
        assert_eq!(plugin.interest(), Interest::Messages);
        assert!(plugin.help().contains("priority"));
        assert!(plugin.help().contains("10"));
        assert!(plugin.help().contains("Stop"));
        assert!(plugin.help().contains(".agent reset"));
        assert!(plugin.help().contains(".agent persona"));
        assert!(plugin.help().contains(".agent status"));
    }

    #[test]
    fn parses_subcommands_from_the_first_token() {
        assert!(matches!(parse_agent_cmd(""), AgentCmd::Help));
        assert!(matches!(parse_agent_cmd("-h"), AgentCmd::Help));
        assert!(matches!(parse_agent_cmd("reset"), AgentCmd::Reset));
        assert!(matches!(parse_agent_cmd("reset now"), AgentCmd::Unknown));
        assert!(matches!(parse_agent_cmd("status"), AgentCmd::Status));
        assert!(matches!(parse_agent_cmd("persona"), AgentCmd::ShowPersona));
        assert!(matches!(
            parse_agent_cmd("persona strict"),
            AgentCmd::SetPersona(ref name) if name == "strict"
        ));
        assert!(matches!(
            parse_agent_cmd("persona strict now"),
            AgentCmd::Unknown
        ));
        assert!(matches!(parse_agent_cmd("nope"), AgentCmd::Unknown));

        let rest = command::strip_token(".agent persona strict", ".agent", ".").unwrap();
        assert!(
            matches!(parse_agent_cmd(rest), AgentCmd::SetPersona(ref name) if name == "strict")
        );
        let rest = command::strip_token("!agent reset", ".agent", "!").unwrap();
        assert!(matches!(parse_agent_cmd(rest), AgentCmd::Reset));
        assert!(command::strip_token("福酱 reset", ".agent", ".").is_none());
        assert!(command::strip_token("请 .agent reset", ".agent", ".").is_none());
    }

    #[tokio::test]
    async fn reset_clears_only_this_session_and_keeps_the_persona() {
        let pool = pool().await;
        let cfg = cfg(2, 4);
        let group = session_key(Source::Group { id: 7 });
        let friend = session_key(Source::Friend { id: 7 });
        save_exchange(&pool, &group, 1, "g", "ga").await.unwrap();
        save_exchange(&pool, &friend, 7, "p", "pa").await.unwrap();
        session::set_persona(&pool, &group, "strict", 1)
            .await
            .unwrap();

        let text = run(&pool, &cfg, &group, 1, &AgentCmd::Reset, 2).await;
        assert_eq!(text, "已清空当前会话的对话记录。");
        assert!(session::recent_turns(&pool, &group, 20)
            .await
            .unwrap()
            .is_empty());
        let left = session::recent_turns(&pool, &friend, 20).await.unwrap();
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].content, "p");
        assert_eq!(
            session::persona_of(&pool, &group).await.unwrap().as_deref(),
            Some("strict")
        );
        assert_eq!(usage_rows(&pool).await, 0);

        let again = run(&pool, &cfg, &group, 1, &AgentCmd::Reset, 3).await;
        assert_eq!(again, "当前会话没有对话记录。");
    }

    #[tokio::test]
    async fn persona_view_switches_and_rejects_unknown_names() {
        let pool = pool().await;
        let cfg = cfg(2, 4);
        let key = session_key(Source::Group { id: 1 });

        let shown = run(&pool, &cfg, &key, 1, &AgentCmd::ShowPersona, 1).await;
        assert_eq!(shown, "当前人设：default\n默认人设");
        let switched = run(
            &pool,
            &cfg,
            &key,
            1,
            &AgentCmd::SetPersona("strict".into()),
            2,
        )
        .await;
        assert_eq!(switched, "已切换到人设 strict。");
        let shown = run(&pool, &cfg, &key, 1, &AgentCmd::ShowPersona, 3).await;
        assert_eq!(shown, "当前人设：strict\n严格人设");

        let missing = run(
            &pool,
            &cfg,
            &key,
            1,
            &AgentCmd::SetPersona("nope".into()),
            4,
        )
        .await;
        assert_eq!(missing, "没有人设 nope。可用：default、strict。");
        assert_eq!(
            session::persona_of(&pool, &key).await.unwrap().as_deref(),
            Some("strict")
        );
        assert_eq!(usage_rows(&pool).await, 0);

        session::set_persona(&pool, &key, "ghost", 5).await.unwrap();
        let stale = run(&pool, &cfg, &key, 1, &AgentCmd::ShowPersona, 6).await;
        assert_eq!(
            stale,
            "当前人设：ghost，配置里没有，正在用 default。\n默认人设"
        );

        let help = apply_command(&pool, &cfg, &key, 1, &AgentCmd::Help, "!", 7)
            .await
            .unwrap();
        assert!(help.contains("!agent reset"));
        assert!(!help.contains(".agent"));
        let unknown = apply_command(&pool, &cfg, &key, 1, &AgentCmd::Unknown, ".", 8)
            .await
            .unwrap();
        assert_eq!(unknown, "不认识这个子命令，发 .agent 看说明");
    }

    #[tokio::test]
    async fn chat_uses_the_persona_and_stops_at_the_cap_without_another_row() {
        let pool = pool().await;
        let cfg = cfg(1, 10);
        let key = "group:7";
        session::set_persona(&pool, key, "strict", 1).await.unwrap();
        save_exchange(&pool, key, 5, "u1", "a1").await.unwrap();
        save_exchange(&pool, key, 5, "u2", "a2").await.unwrap();

        let plan = plan_chat(&pool, &cfg, key, 5, "u3", 1_000).await.unwrap();
        let ChatPlan::Speak { messages } = plan else {
            panic!("expected a chat");
        };
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[0].content, "严格人设");
        assert_eq!(messages[1].content, "u1");
        assert_eq!(messages.last().unwrap().content, "u3");
        assert_eq!(usage_rows(&pool).await, 1);

        let status = run(&pool, &cfg, key, 5, &AgentCmd::Status, 1_000).await;
        assert_eq!(
            status,
            "人设：strict\n你这一小时还剩 0 次，这个会话还剩 9 次。"
        );

        let blocked = plan_chat(&pool, &cfg, key, 5, "more", 1_001).await.unwrap();
        assert!(matches!(blocked, ChatPlan::Tip(tip) if tip == USER_LIMIT_TIP));
        assert_eq!(usage_rows(&pool).await, 1);

        let other = plan_chat(&pool, &cfg, "group:8", 5, "elsewhere", 1_002)
            .await
            .unwrap();
        assert!(matches!(other, ChatPlan::Tip(tip) if tip == USER_LIMIT_TIP));
        assert_eq!(usage_rows(&pool).await, 1);
    }

    #[tokio::test]
    async fn session_cap_blocks_other_users_and_a_zero_cap_stays_open() {
        let pool = pool().await;
        let capped = cfg(5, 1);
        let first = plan_chat(&pool, &capped, "group:1", 1, "a", 2_000)
            .await
            .unwrap();
        assert!(matches!(first, ChatPlan::Speak { .. }));
        let second = plan_chat(&pool, &capped, "group:1", 2, "b", 2_001)
            .await
            .unwrap();
        assert!(matches!(second, ChatPlan::Tip(tip) if tip == SESSION_LIMIT_TIP));
        assert_eq!(usage_rows(&pool).await, 1);
        let elsewhere = plan_chat(&pool, &capped, "private:2", 2, "c", 2_002)
            .await
            .unwrap();
        assert!(matches!(elsewhere, ChatPlan::Speak { .. }));
        assert_eq!(usage_rows(&pool).await, 2);

        let open = cfg(0, 0);
        for n in 0..3 {
            let plan = plan_chat(&pool, &open, "group:9", 9, "z", 3_000 + n)
                .await
                .unwrap();
            assert!(matches!(plan, ChatPlan::Speak { .. }));
        }
        assert_eq!(usage_rows(&pool).await, 2);
        let status = run(&pool, &open, "group:9", 9, &AgentCmd::Status, 3_010).await;
        assert_eq!(status, "人设：default\n这一小时不限次数。");

        let user_open = cfg(0, 1);
        let spoke = plan_chat(&pool, &user_open, "group:3", 1, "a", 4_000)
            .await
            .unwrap();
        assert!(matches!(spoke, ChatPlan::Speak { .. }));
        let full = plan_chat(&pool, &user_open, "group:3", 2, "b", 4_001)
            .await
            .unwrap();
        assert!(matches!(full, ChatPlan::Tip(tip) if tip == SESSION_LIMIT_TIP));
        let status = run(&pool, &user_open, "group:3", 3, &AgentCmd::Status, 4_001).await;
        assert_eq!(
            status,
            "人设：default\n你这一小时不限次数，这个会话还剩 0 次。"
        );
    }

    #[tokio::test]
    async fn quota_window_drops_events_older_than_one_hour() {
        let pool = pool().await;
        let cfg = cfg(1, 10);
        let key = "group:1";
        let spoke = plan_chat(&pool, &cfg, key, 4, "a", 10_000).await.unwrap();
        assert!(matches!(spoke, ChatPlan::Speak { .. }));
        let inside = plan_chat(&pool, &cfg, key, 4, "b", 13_599).await.unwrap();
        assert!(matches!(inside, ChatPlan::Tip(tip) if tip == USER_LIMIT_TIP));
        let later = plan_chat(&pool, &cfg, key, 4, "c", 13_600).await.unwrap();
        assert!(matches!(later, ChatPlan::Speak { .. }));
        assert_eq!(usage_rows(&pool).await, 1);
        let again = plan_chat(&pool, &cfg, key, 4, "d", 13_601).await.unwrap();
        assert!(matches!(again, ChatPlan::Tip(tip) if tip == USER_LIMIT_TIP));
    }

    #[tokio::test]
    async fn history_limit_keeps_the_latest_turns_in_the_prompt() {
        let pool = pool().await;
        let cfg = AgentConfig::from_value(&json!({
            "system_prompt": "默认人设",
            "max_history_turns": 1,
            "rate_limit": { "per_user_per_hour": 0, "per_group_per_hour": 0 }
        }));
        let key = "private:3";
        save_exchange(&pool, key, 3, "u1", "a1").await.unwrap();
        save_exchange(&pool, key, 3, "u2", "a2").await.unwrap();
        let plan = plan_chat(&pool, &cfg, key, 3, "u3", 5).await.unwrap();
        let ChatPlan::Speak { messages } = plan else {
            panic!("expected a chat");
        };
        assert_eq!(
            messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["默认人设", "u2", "a2", "u3"]
        );
    }

    #[test]
    fn quota_wording_covers_each_cap() {
        assert_eq!(quota_sentence(0, 0, 0, 0), "这一小时不限次数。");
        assert_eq!(
            quota_sentence(2, 1, 0, 9),
            "你这一小时还剩 1 次，这个会话不限次数。"
        );
        assert_eq!(
            quota_sentence(0, 0, 4, 1),
            "你这一小时不限次数，这个会话还剩 3 次。"
        );
        assert_eq!(
            quota_sentence(30, 30, 120, 7),
            "你这一小时还剩 0 次，这个会话还剩 113 次。"
        );
    }
}
