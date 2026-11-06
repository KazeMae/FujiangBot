use sqlx::SqlitePool;

use crate::client::ChatMessage;
use crate::command::enabled_builtins;
use crate::config::AgentConfig;
use crate::session;
use crate::tools;
use crate::trigger::build_messages;

pub(crate) const USER_LIMIT_TIP: &str = "你这一小时里找福酱的次数到了，歇一会儿再来。";
pub(crate) const SESSION_LIMIT_TIP: &str = "这个会话这一小时的次数到了，歇一会儿再来。";
pub(crate) const BLACKLIST_TIP: &str = "这句话我不能接，换个说法再问我。";

pub(crate) enum ChatPlan {
    Speak { messages: Vec<ChatMessage> },
    Tip(&'static str),
    MissingKey,
}

/// History is loaded before the slot so a failed read does not burn quota.
pub(crate) async fn plan_chat(
    pool: &SqlitePool,
    cfg: &AgentConfig,
    session_key: &str,
    user_id: i64,
    user_text: &str,
    now: i64,
    key_ready: bool,
) -> anyhow::Result<ChatPlan> {
    if cfg.keyword_hit(user_text) {
        return Ok(ChatPlan::Tip(BLACKLIST_TIP));
    }
    if !key_ready {
        return Ok(ChatPlan::MissingKey);
    }
    if cfg.is_admin(user_id) {
        let messages = turn_messages(pool, cfg, session_key, user_text).await?;
        return Ok(ChatPlan::Speak { messages });
    }
    let messages = turn_messages(pool, cfg, session_key, user_text).await?;
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
        session::Slot::Taken => ChatPlan::Speak { messages },
        session::Slot::UserLimited => ChatPlan::Tip(USER_LIMIT_TIP),
        session::Slot::SessionLimited => ChatPlan::Tip(SESSION_LIMIT_TIP),
    })
}

async fn turn_messages(
    pool: &SqlitePool,
    cfg: &AgentConfig,
    session_key: &str,
    user_text: &str,
) -> anyhow::Result<Vec<ChatMessage>> {
    let stored = session::persona_of(pool, session_key).await?;
    let (_, persona) = cfg.resolve_persona(stored.as_deref());
    let enabled = tools::assemble_tools(&cfg.tools, enabled_builtins(cfg));
    let prompt = tools::instructions_with_catalog(persona, &enabled, cfg.inject_tool_list);
    let history = session::recent_turns(pool, session_key, cfg.max_history_turns).await?;
    Ok(build_messages(&prompt, history, user_text.to_string()))
}
