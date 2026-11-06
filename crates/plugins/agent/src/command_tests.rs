use fujiang_core::{command, Source};
use serde_json::json;
use sqlx::SqlitePool;

use super::{apply_command, parse_agent_cmd, quota_sentence, AgentCmd};
use crate::config::AgentConfig;
use crate::session::{self, save_exchange, session_key};

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
fn parses_subcommands_from_the_first_token() {
    assert!(matches!(parse_agent_cmd(""), AgentCmd::Help));
    assert!(matches!(parse_agent_cmd("-h"), AgentCmd::Help));
    assert!(matches!(parse_agent_cmd("reset"), AgentCmd::Reset));
    assert!(matches!(parse_agent_cmd("reset now"), AgentCmd::Unknown));
    assert!(matches!(parse_agent_cmd("status"), AgentCmd::Status));
    assert!(matches!(parse_agent_cmd("tools"), AgentCmd::Tools));
    assert!(matches!(parse_agent_cmd("tools now"), AgentCmd::Unknown));
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
    assert!(matches!(parse_agent_cmd(rest), AgentCmd::SetPersona(ref name) if name == "strict"));
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
    assert!(help.contains("!agent tools"));
    assert!(!help.contains(".agent"));
    let unknown = apply_command(&pool, &cfg, &key, 1, &AgentCmd::Unknown, ".", 8)
        .await
        .unwrap();
    assert_eq!(unknown, "不认识这个子命令，发 .agent 看说明");
}

#[tokio::test]
async fn status_shows_today_for_this_user_and_this_session() {
    let pool = pool().await;
    let cfg = cfg(0, 0);
    let day = 86_400 * 10;
    session::record_daily(&pool, "group:7", 5, day + 10, 1, 4, 2)
        .await
        .unwrap();
    session::record_daily(&pool, "private:5", 5, day + 30, 1, 10, 1)
        .await
        .unwrap();
    session::record_daily(&pool, "group:7", 8, day + 40, 1, 7, 8)
        .await
        .unwrap();
    let status = run(&pool, &cfg, "group:7", 5, &AgentCmd::Status, day + 50).await;
    assert_eq!(
        status,
        "人设：default\n这一小时不限次数。\n今日你 2 次，输入 14，输出 3。今日会话 2 次，输入 11，输出 10。"
    );
    let next = run(&pool, &cfg, "group:7", 5, &AgentCmd::Status, day + 86_400).await;
    assert_eq!(
        next,
        "人设：default\n这一小时不限次数。\n今日你 0 次，输入 0，输出 0。今日会话 0 次，输入 0，输出 0。"
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
