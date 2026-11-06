use fujiang_core::command;
use sqlx::SqlitePool;
use tracing::info;

use crate::config::AgentConfig;
use crate::session;
use crate::tools::{self, ToolSpec};
use crate::trigger::fit_reply;

pub(crate) const HELP: &str =
    "对话。@我、消息里带唤醒词（默认「福酱」），或回复我的消息，我会接话。\n\
同一群共用一份上下文，私聊各自一份。\n\
.agent              本说明\n\
.agent reset        清空当前群或私聊的对话记录和摘要，人设保留\n\
.agent memory       查看当前会话的摘要。这是会话摘要，不是分层记忆\n\
.agent forget       只清空摘要。这是会话摘要，不是分层记忆。对话记录和人设保留\n\
.agent persona      查看当前人设\n\
.agent persona <名> 切换到配置里的人设\n\
.agent status       当前人设、这一小时还剩几次，以及今日调用和 token\n\
.agent tools        列出当前启用的工具\n\
.agent schedule     列出、创建或取消自己的定时发文字。不调用模型\n\
在配置里把 priority 设为 10，高于 fun。本插件接过的消息返回 Stop，fun 不会再处理同一条。\n\
require_at 为 true 时，只认 @我或回复我，单独的唤醒词不算。";

pub(crate) enum AgentCmd {
    Help,
    Reset,
    Memory,
    Forget,
    Status,
    Tools,
    ShowPersona,
    SetPersona(String),
    Schedule(crate::schedule::HumanCmd),
    Unknown,
}

pub(crate) fn parse_agent_cmd(rest: &str) -> AgentCmd {
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
        "memory" => {
            if parts.next().is_some() {
                AgentCmd::Unknown
            } else {
                AgentCmd::Memory
            }
        }
        "forget" => {
            if parts.next().is_some() {
                AgentCmd::Unknown
            } else {
                AgentCmd::Forget
            }
        }
        "status" => {
            if parts.next().is_some() {
                AgentCmd::Unknown
            } else {
                AgentCmd::Status
            }
        }
        "tools" => {
            if parts.next().is_some() {
                AgentCmd::Unknown
            } else {
                AgentCmd::Tools
            }
        }
        "schedule" => AgentCmd::Schedule(crate::schedule::parse_human(parts)),
        "persona" => match parts.next() {
            None => AgentCmd::ShowPersona,
            Some(name) if parts.next().is_none() => AgentCmd::SetPersona(name.to_string()),
            Some(_) => AgentCmd::Unknown,
        },
        _ => AgentCmd::Unknown,
    }
}

pub(crate) async fn apply_command(
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
            let cleared = session::clear_summary(pool, session_key).await?;
            if n == 0 && !cleared {
                Ok("当前会话没有对话记录。".to_string())
            } else {
                Ok("已清空当前会话的对话记录。".to_string())
            }
        }
        AgentCmd::Memory => {
            if !cfg.memory_enabled {
                return Ok("会话记忆已关闭。".to_string());
            }
            let summary = session::summary_of(pool, session_key).await?;
            if summary.is_empty() {
                Ok("暂无摘要".to_string())
            } else {
                Ok(fit_reply(&summary))
            }
        }
        AgentCmd::Forget => {
            if !cfg.memory_enabled {
                return Ok("会话记忆已关闭。".to_string());
            }
            if session::clear_summary(pool, session_key).await? {
                Ok("已清空会话摘要。".to_string())
            } else {
                Ok("暂无摘要".to_string())
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
            let today = session::today_usage(pool, session_key, user_id, now).await?;
            Ok(format!(
                "人设：{title}\n{quota}\n{}",
                today_sentence(today.user, today.session)
            ))
        }
        AgentCmd::Tools => {
            let tools = log_enabled_tools(cfg);
            Ok(fit_reply(&tools::tools_command_text(&tools)))
        }
        AgentCmd::Schedule(_) => Ok(crate::schedule::USAGE.to_string()),
    }
}

pub(crate) fn enabled_builtins(cfg: &AgentConfig) -> tools::BuiltinTools {
    tools::BuiltinTools {
        rank: cfg.enable_rank_tools,
        contest: cfg.enable_contest_tools,
        problem: cfg.enable_problem_tools,
        memo: cfg.enable_memo_tools,
        memo_write: cfg.enable_memo_write_tools,
        fun: cfg.enable_fun_tools,
        memory: cfg.enable_memory_tools,
        memory_write: cfg.enable_memory_write_tools,
    }
}

pub(crate) fn log_enabled_tools(cfg: &AgentConfig) -> Vec<ToolSpec> {
    let tools = tools::resident_tools(&cfg.tools, enabled_builtins(cfg));
    let names: Vec<&str> = tools.iter().map(ToolSpec::name).collect();
    info!(tools = ?names, "agent tools");
    tools
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

fn today_sentence(user: session::DayUsage, session: session::DayUsage) -> String {
    format!(
        "今日你 {uc} 次，输入 {ui}，输出 {uo}。今日会话 {sc} 次，输入 {si}，输出 {so}。",
        uc = user.calls,
        ui = user.in_tokens,
        uo = user.out_tokens,
        sc = session.calls,
        si = session.in_tokens,
        so = session.out_tokens,
    )
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
#[path = "command_tests.rs"]
mod command_tests;
