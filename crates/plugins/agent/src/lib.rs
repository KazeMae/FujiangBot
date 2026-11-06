//! Chat plugin. `.agent` edits the session. A wake calls the LLM and returns [`Flow::Stop`].

mod client;
mod command;
mod config;
mod layered_memory;
mod memory_consolidate;
mod plan;
mod schedule;
mod schedule_cron;
mod session;
mod tools;
mod trigger;
mod vision;

#[cfg(test)]
#[path = "guard_tests.rs"]
mod guard_tests;

pub use client::{ChatMessage, LlmClient, LlmError, Role};
pub use config::{AgentConfig, RateLimit};
pub use tools::ToolSpec;

use async_trait::async_trait;
use fujiang_core::{
    command::strip_token, declare_plugin, BotContext, Event, Flow, Interest, MessageEvent, Plugin,
    PluginMeta, PluginScope,
};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::warn;

use crate::command::{
    apply_command, enabled_builtins, log_enabled_tools, parse_agent_cmd, AgentCmd, HELP,
};
use crate::plan::{plan_chat, ChatPlan};
use crate::session::{save_exchange, session_key};
use crate::trigger::{claim, fit_reply, llm_reply, Claim, BUSY_REPLY};

#[cfg(test)]
pub(crate) use plan::{BLACKLIST_TIP, SESSION_LIMIT_TIP, USER_LIMIT_TIP};

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
        let cfg = AgentConfig::from_value(&ctx.instance_config(scope).await);
        memory_consolidate::consolidate_if(
            &pool,
            cfg.memory_consolidate_on_start,
            session::unix_now(),
            memory_consolidate::unused_secs(cfg.memory_max_unused_days),
            i64::from(cfg.memory_max_entries_per_scope),
        )
        .await?;
        let tick_pool = pool.clone();
        let tick_ctx = ctx.clone();
        scope.spawn(async move {
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_secs(schedule::TICK_SECS));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if let Err(err) =
                    tools::schedule_tick(&tick_pool, &tick_ctx, session::unix_now()).await
                {
                    warn!(error = %err, "agent schedule tick");
                }
            }
        });
        *self.pool.lock().await = Some(pool);
        log_enabled_tools(&cfg);
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
        if let Some(rest) = strip_token(&msg.command_line(), ".agent", &prefix) {
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
        if let AgentCmd::Schedule(human) = cmd {
            match tools::schedule_command(&pool, msg.user_id(), &human, session::unix_now(), ctx)
                .await
            {
                Ok(text) => {
                    say(
                        ctx,
                        msg,
                        fit_reply(&fujiang_core::command::rewrite_help(
                            &text,
                            &[".agent"],
                            &prefix,
                        )),
                    )
                    .await;
                }
                Err(err) => {
                    warn!(error = %err, "agent schedule");
                    say(ctx, msg, BUSY_REPLY).await;
                }
            }
            return;
        }
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
        let now = session::unix_now();
        let prepared = LlmClient::from_config(cfg, ctx.http.clone());
        let plan = plan_chat(
            &pool,
            cfg,
            &key,
            msg.user_id(),
            &user_text,
            now,
            prepared.is_ok(),
        )
        .await;
        let (client, messages, skipped) = match (prepared, plan) {
            (_, Ok(ChatPlan::Tip(tip))) => {
                say(ctx, msg, tip).await;
                return;
            }
            (Err(err), Ok(ChatPlan::MissingKey)) => {
                say(ctx, msg, llm_reply(&err)).await;
                return;
            }
            (Ok(client), Ok(ChatPlan::Speak { mut messages })) => {
                let medias = vision::message_images(msg);
                match vision::image_plan(cfg.vision_enabled, medias.len()) {
                    vision::ImagePlan::None => (client, messages, false),
                    vision::ImagePlan::Skip => (client, messages, true),
                    vision::ImagePlan::Load => {
                        let mut images = Vec::with_capacity(medias.len());
                        for media in &medias {
                            let loaded = match ctx.fetch_media_bytes(media).await {
                                Ok(bytes) => Ok(bytes),
                                Err(err) => {
                                    warn!(error = %err, "agent image");
                                    Err(())
                                }
                            };
                            match vision::loaded_part(cfg.max_image_bytes, loaded) {
                                Ok(part) => images.push(part),
                                Err(tip) => {
                                    say(ctx, msg, tip).await;
                                    return;
                                }
                            }
                        }
                        vision::give_images(&mut messages, images);
                        (client, messages, false)
                    }
                }
            }
            (_, Err(err)) => {
                warn!(error = %err, "agent chat");
                say(ctx, msg, BUSY_REPLY).await;
                return;
            }
            _ => {
                say(ctx, msg, BUSY_REPLY).await;
                return;
            }
        };
        let scope = tools::MemoCallScope::from_source(msg.source, msg.user_id());
        let pass = tools::ToolPass {
            max_rounds: cfg.max_tool_rounds,
            scope: Some(&scope),
            memory: Some(&pool),
            memory_unused_secs: memory_consolidate::unused_secs(cfg.memory_max_unused_days),
            memory_max_entries: i64::from(cfg.memory_max_entries_per_scope),
        };
        let turn = tools::run_tool_loop(
            &client,
            &messages,
            &cfg.tools,
            enabled_builtins(cfg),
            &pass,
            ctx,
        )
        .await;
        if let Err(err) = session::record_daily(
            &pool,
            &key,
            msg.user_id(),
            now,
            turn.calls,
            turn.in_tokens,
            turn.out_tokens,
        )
        .await
        {
            warn!(error = %err, "agent usage");
        }
        let reply = match turn.reply {
            Ok(tools::ToolReply::Text(text)) => text,
            Ok(tools::ToolReply::Degraded(text)) => {
                if skipped {
                    say(ctx, msg, fit_reply(&vision::show_reply(text, true))).await;
                } else {
                    say(ctx, msg, text).await;
                }
                return;
            }
            Err(err) => {
                let tip = llm_reply(&err);
                let shown = if skipped {
                    vision::show_reply(&tip, true)
                } else {
                    tip
                };
                say(ctx, msg, shown).await;
                return;
            }
        };
        let saved = fit_reply(&reply);
        let shown = fit_reply(&vision::show_reply(&reply, skipped));
        if shown.is_empty() {
            say(ctx, msg, BUSY_REPLY).await;
            return;
        }
        if let Err(err) = ctx.reply_text(msg, &shown).await {
            warn!(error = %err, "agent reply");
            return;
        }
        if saved.is_empty() {
            return;
        }
        if let Err(err) = save_exchange(&pool, &key, msg.user_id(), &user_text, &saved).await {
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
        assert!(plugin.help().contains(".agent memory"));
        assert!(plugin.help().contains(".agent forget"));
        assert!(plugin.help().contains(".agent persona"));
        assert!(plugin.help().contains(".agent status"));
        assert!(plugin.help().contains(".agent tools"));
        assert!(plugin.help().contains(".agent schedule"));
    }

    #[tokio::test]
    async fn tools_command_lists_the_enabled_set_and_injection_is_opt_in() {
        let pool = pool().await;
        let key = session_key(Source::Group { id: 3 });
        let off = AgentConfig::from_value(&json!({
            "system_prompt": "默认人设",
            "enable_rank_tools": false,
            "enable_contest_tools": false,
            "enable_problem_tools": false,
            "enable_memo_tools": false,
            "enable_fun_tools": false,
            "enable_memory_tools": false
        }));
        let empty = run(&pool, &off, &key, 1, &AgentCmd::Tools, 1).await;
        assert!(empty.starts_with("可用工具：\nskills.list："));
        assert!(empty.contains("\nskills.open："));
        assert!(!empty.contains("ping.pong"));
        assert!(!empty.contains("rank.list"));

        let listed = AgentConfig::from_value(&json!({
            "system_prompt": "默认人设",
            "enable_rank_tools": false,
            "enable_contest_tools": false,
            "enable_problem_tools": false,
            "enable_memo_tools": false,
            "enable_fun_tools": false,
            "enable_memory_tools": false,
            "tools": [{
                "name": "memo.list",
                "description": "列出备忘",
                "parameters": {"type": "object", "properties": {}}
            }]
        }));
        let text = run(&pool, &listed, &key, 1, &AgentCmd::Tools, 2).await;
        assert!(text.starts_with("可用工具：\nmemo.list：列出备忘\n"));
        assert!(text.contains("skills.list："));
        assert!(text.contains("skills.open："));
        assert!(!text.contains("memo_list"));
        assert!(!text.contains("ping.pong"));

        let quiet = plan_chat(&pool, &listed, &key, 1, "hi", 3, true)
            .await
            .unwrap();
        let ChatPlan::Speak { messages } = quiet else {
            panic!("expected a chat");
        };
        assert_eq!(messages[0].content, "默认人设");

        let loud = AgentConfig::from_value(&json!({
            "system_prompt": "默认人设",
            "inject_tool_list": true,
            "enable_rank_tools": false,
            "enable_contest_tools": false,
            "enable_problem_tools": false,
            "enable_memo_tools": false,
            "enable_fun_tools": false,
            "enable_memory_tools": false,
            "rate_limit": { "per_user_per_hour": 0, "per_group_per_hour": 0 },
            "tools": [{
                "name": "memo.list",
                "description": "列出备忘",
                "parameters": {"type": "object", "properties": {}}
            }]
        }));
        let plan = plan_chat(&pool, &loud, &key, 1, "hi", 4, true)
            .await
            .unwrap();
        let ChatPlan::Speak { messages } = plan else {
            panic!("expected a chat");
        };
        assert!(messages[0]
            .content
            .starts_with("默认人设\n\n可用工具：\nmemo.list：列出备忘\n"));
        assert!(messages[0].content.contains("skills.list："));
        assert!(messages[0].content.contains("skills.open："));
        assert!(!messages[0].content.contains("ping.pong"));
        assert_eq!(messages[0].role, Role::System);

        let shadowed = AgentConfig::from_value(&json!({
            "enable_contest_tools": false,
            "enable_problem_tools": false,
            "tools": [{
                "name": "rank.list",
                "description": "别的说明",
                "parameters": {"type": "object", "properties": {}}
            }]
        }));
        let rank = run(&pool, &shadowed, &key, 9, &AgentCmd::Tools, 5).await;
        assert!(rank.contains("rank.list：列出本群"));
        assert!(rank.contains("rank.get："));
        assert!(!rank.contains("别的说明"));
        assert!(!rank.contains("contest.upcoming"));
        assert!(rank.contains("memo.list："));
        assert!(rank.contains("fun.tags："));
        assert!(rank.contains("fun.search："));
        assert!(!rank.contains("memo.add"));
        assert!(!rank.contains("fun.delete"));
        assert!(!rank.contains('_'));

        let write = AgentConfig::from_value(&json!({
            "enable_rank_tools": false,
            "enable_contest_tools": false,
            "enable_problem_tools": false,
            "enable_memo_write_tools": true,
            "enable_fun_tools": false
        }));
        let both = run(&pool, &write, &key, 1, &AgentCmd::Tools, 6).await;
        assert!(both.contains("memo.list："));
        assert!(both.contains("memo.add："));
        assert!(!both.contains("memo_add"));
        assert!(!both.contains("rank.list"));
    }

    #[tokio::test]
    async fn chat_uses_the_persona_and_stops_at_the_cap_without_another_row() {
        let pool = pool().await;
        let cfg = cfg(1, 10);
        let key = "group:7";
        session::set_persona(&pool, key, "strict", 1).await.unwrap();
        save_exchange(&pool, key, 5, "u1", "a1").await.unwrap();
        save_exchange(&pool, key, 5, "u2", "a2").await.unwrap();

        let plan = plan_chat(&pool, &cfg, key, 5, "u3", 1_000, true)
            .await
            .unwrap();
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
            "人设：strict\n你这一小时还剩 0 次，这个会话还剩 9 次。\n今日你 0 次，输入 0，输出 0。今日会话 0 次，输入 0，输出 0。"
        );

        let blocked = plan_chat(&pool, &cfg, key, 5, "more", 1_001, true)
            .await
            .unwrap();
        assert!(matches!(blocked, ChatPlan::Tip(tip) if tip == USER_LIMIT_TIP));
        assert_eq!(usage_rows(&pool).await, 1);

        let other = plan_chat(&pool, &cfg, "group:8", 5, "elsewhere", 1_002, true)
            .await
            .unwrap();
        assert!(matches!(other, ChatPlan::Tip(tip) if tip == USER_LIMIT_TIP));
        assert_eq!(usage_rows(&pool).await, 1);
    }

    #[tokio::test]
    async fn session_cap_blocks_other_users_and_a_zero_cap_stays_open() {
        let pool = pool().await;
        let capped = cfg(5, 1);
        let first = plan_chat(&pool, &capped, "group:1", 1, "a", 2_000, true)
            .await
            .unwrap();
        assert!(matches!(first, ChatPlan::Speak { .. }));
        let second = plan_chat(&pool, &capped, "group:1", 2, "b", 2_001, true)
            .await
            .unwrap();
        assert!(matches!(second, ChatPlan::Tip(tip) if tip == SESSION_LIMIT_TIP));
        assert_eq!(usage_rows(&pool).await, 1);
        let elsewhere = plan_chat(&pool, &capped, "private:2", 2, "c", 2_002, true)
            .await
            .unwrap();
        assert!(matches!(elsewhere, ChatPlan::Speak { .. }));
        assert_eq!(usage_rows(&pool).await, 2);

        let open = cfg(0, 0);
        for n in 0..3 {
            let plan = plan_chat(&pool, &open, "group:9", 9, "z", 3_000 + n, true)
                .await
                .unwrap();
            assert!(matches!(plan, ChatPlan::Speak { .. }));
        }
        assert_eq!(usage_rows(&pool).await, 2);
        let status = run(&pool, &open, "group:9", 9, &AgentCmd::Status, 3_010).await;
        assert_eq!(
            status,
            "人设：default\n这一小时不限次数。\n今日你 0 次，输入 0，输出 0。今日会话 0 次，输入 0，输出 0。"
        );

        let user_open = cfg(0, 1);
        let spoke = plan_chat(&pool, &user_open, "group:3", 1, "a", 4_000, true)
            .await
            .unwrap();
        assert!(matches!(spoke, ChatPlan::Speak { .. }));
        let full = plan_chat(&pool, &user_open, "group:3", 2, "b", 4_001, true)
            .await
            .unwrap();
        assert!(matches!(full, ChatPlan::Tip(tip) if tip == SESSION_LIMIT_TIP));
        let status = run(&pool, &user_open, "group:3", 3, &AgentCmd::Status, 4_001).await;
        assert_eq!(
            status,
            "人设：default\n你这一小时不限次数，这个会话还剩 0 次。\n今日你 0 次，输入 0，输出 0。今日会话 0 次，输入 0，输出 0。"
        );
    }

    #[tokio::test]
    async fn quota_window_drops_events_older_than_one_hour() {
        let pool = pool().await;
        let cfg = cfg(1, 10);
        let key = "group:1";
        let spoke = plan_chat(&pool, &cfg, key, 4, "a", 10_000, true)
            .await
            .unwrap();
        assert!(matches!(spoke, ChatPlan::Speak { .. }));
        let inside = plan_chat(&pool, &cfg, key, 4, "b", 13_599, true)
            .await
            .unwrap();
        assert!(matches!(inside, ChatPlan::Tip(tip) if tip == USER_LIMIT_TIP));
        let later = plan_chat(&pool, &cfg, key, 4, "c", 13_600, true)
            .await
            .unwrap();
        assert!(matches!(later, ChatPlan::Speak { .. }));
        assert_eq!(usage_rows(&pool).await, 1);
        let again = plan_chat(&pool, &cfg, key, 4, "d", 13_601, true)
            .await
            .unwrap();
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
        let plan = plan_chat(&pool, &cfg, key, 3, "u3", 5, true).await.unwrap();
        let ChatPlan::Speak { messages } = plan else {
            panic!("expected a chat");
        };
        let system = &messages[0].content;
        let prior = system.find("先前上下文").expect("summary");
        assert!(system.find("默认人设").unwrap() < prior);
        assert!(system.contains("用户：u1"));
        assert!(system.contains("助手：a1"));
        assert_eq!(system.matches("用户：u1").count(), 1);
        assert_eq!(
            messages
                .iter()
                .skip(1)
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["u2", "a2", "u3"]
        );
        assert!(session::recent_turns(&pool, key, 20)
            .await
            .unwrap()
            .iter()
            .all(|m| m.content != "u1"));

        let again = plan_chat(&pool, &cfg, key, 3, "u3", 6, true).await.unwrap();
        let ChatPlan::Speak { messages: second } = again else {
            panic!("expected a chat");
        };
        assert_eq!(second[0].content, *system);
    }

    #[tokio::test]
    async fn memory_off_skips_fold_and_injection() {
        let pool = pool().await;
        let cfg = AgentConfig::from_value(&json!({
            "system_prompt": "默认人设",
            "max_history_turns": 1,
            "memory": { "enabled": false },
            "inject_tool_list": true,
            "rate_limit": { "per_user_per_hour": 0, "per_group_per_hour": 0 }
        }));
        let key = "private:4";
        save_exchange(&pool, key, 4, "u1", "a1").await.unwrap();
        save_exchange(&pool, key, 4, "u2", "a2").await.unwrap();
        let plan = plan_chat(&pool, &cfg, key, 4, "u3", 5, true).await.unwrap();
        let ChatPlan::Speak { messages } = plan else {
            panic!("expected a chat");
        };
        assert!(messages[0].content.starts_with("默认人设"));
        assert!(!messages[0].content.contains("先前上下文"));
        assert!(messages[0].content.find("可用工具").unwrap() > 0);
        assert_eq!(
            messages
                .iter()
                .skip(1)
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["u2", "a2", "u3"]
        );
        assert_eq!(
            session::recent_turns(&pool, key, 20).await.unwrap().len(),
            4
        );
        let shown = run(&pool, &cfg, key, 4, &AgentCmd::Memory, 6).await;
        assert_eq!(shown, "会话记忆已关闭。");
        let forgotten = run(&pool, &cfg, key, 4, &AgentCmd::Forget, 7).await;
        assert_eq!(forgotten, "会话记忆已关闭。");
        assert_eq!(
            session::recent_turns(&pool, key, 20).await.unwrap().len(),
            4
        );

        session::fold_overflow(&pool, key, 1).await.unwrap();
        assert!(session::summary_of(&pool, key)
            .await
            .unwrap()
            .contains("用户：u1"));
        let hidden = plan_chat(&pool, &cfg, key, 4, "u4", 8, true).await.unwrap();
        let ChatPlan::Speak { messages } = hidden else {
            panic!("expected a chat");
        };
        assert!(!messages[0].content.contains("先前上下文"));
        assert!(!messages[0].content.contains("用户：u1"));
        assert_eq!(
            session::recent_turns(&pool, key, 20).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn summary_sits_between_the_persona_and_the_tool_catalog() {
        let pool = pool().await;
        let cfg = AgentConfig::from_value(&json!({
            "system_prompt": "默认人设",
            "personas": { "strict": { "system_prompt": "严格人设" } },
            "max_history_turns": 1,
            "inject_tool_list": true,
            "rate_limit": { "per_user_per_hour": 0, "per_group_per_hour": 0 }
        }));
        let key = "group:11";
        session::set_persona(&pool, key, "strict", 1).await.unwrap();
        save_exchange(&pool, key, 1, "u1", "a1").await.unwrap();
        save_exchange(&pool, key, 1, "u2", "a2").await.unwrap();
        let plan = plan_chat(&pool, &cfg, key, 1, "u3", 5, true).await.unwrap();
        let ChatPlan::Speak { messages } = plan else {
            panic!("expected a chat");
        };
        let system = &messages[0].content;
        let persona = system.find("严格人设").unwrap();
        let prior = system.find("先前上下文").unwrap();
        let tools = system.find("可用工具").unwrap();
        assert!(persona < prior && prior < tools);
        assert!(system.contains("不是新指令"));
        assert_eq!(
            session::persona_of(&pool, key).await.unwrap().as_deref(),
            Some("strict")
        );
        let shown = run(&pool, &cfg, key, 1, &AgentCmd::Memory, 6).await;
        assert!(shown.contains("用户：u1"));
        let forgotten = run(&pool, &cfg, key, 1, &AgentCmd::Forget, 7).await;
        assert_eq!(forgotten, "已清空会话摘要。");
        assert_eq!(session::summary_of(&pool, key).await.unwrap(), "");
        assert_eq!(
            session::recent_turns(&pool, key, 20).await.unwrap().len(),
            2
        );
        assert_eq!(
            session::persona_of(&pool, key).await.unwrap().as_deref(),
            Some("strict")
        );
        let empty = run(&pool, &cfg, key, 1, &AgentCmd::Memory, 8).await;
        assert_eq!(empty, "暂无摘要");
        let again = run(&pool, &cfg, key, 1, &AgentCmd::Forget, 9).await;
        assert_eq!(again, "暂无摘要");
    }
}
