use serde_json::json;
use sqlx::SqlitePool;

use super::{plan_chat, session, AgentConfig, ChatPlan, BLACKLIST_TIP, SESSION_LIMIT_TIP};

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

#[tokio::test]
async fn blacklist_tip_skips_the_slot_even_for_an_admin_and_even_without_a_key() {
    let pool = pool().await;
    let cfg = AgentConfig::from_value(&json!({
        "rate_limit": { "per_user_per_hour": 1, "per_group_per_hour": 1 },
        "admins": [7],
        "blacklist": ["代写"]
    }));
    for (user, ready) in [(7_i64, false), (7, true), (8, false), (8, true)] {
        let plan = plan_chat(&pool, &cfg, "group:1", user, "请代写", 1_000, ready)
            .await
            .unwrap();
        assert!(matches!(plan, ChatPlan::Tip(tip) if tip == BLACKLIST_TIP));
        assert_eq!(usage_rows(&pool).await, 0);
    }
}

#[tokio::test]
async fn admin_speaks_when_the_session_cap_is_full_and_inserts_nothing() {
    let pool = pool().await;
    let cfg = AgentConfig::from_value(&json!({
        "rate_limit": { "per_user_per_hour": 5, "per_group_per_hour": 1 },
        "admins": [7]
    }));
    let first = plan_chat(&pool, &cfg, "group:1", 8, "a", 2_000, true)
        .await
        .unwrap();
    assert!(matches!(first, ChatPlan::Speak { .. }));
    assert_eq!(usage_rows(&pool).await, 1);
    let admin = plan_chat(&pool, &cfg, "group:1", 7, "b", 2_001, true)
        .await
        .unwrap();
    assert!(matches!(admin, ChatPlan::Speak { .. }));
    assert_eq!(usage_rows(&pool).await, 1);
    let other = plan_chat(&pool, &cfg, "group:1", 9, "c", 2_002, true)
        .await
        .unwrap();
    assert!(matches!(other, ChatPlan::Tip(tip) if tip == SESSION_LIMIT_TIP));
    assert_eq!(usage_rows(&pool).await, 1);
}

#[tokio::test]
async fn missing_key_does_not_insert_a_usage_row() {
    let pool = pool().await;
    let cfg = cfg(1, 1);
    let plan = plan_chat(&pool, &cfg, "group:1", 8, "hello", 3_000, false)
        .await
        .unwrap();
    assert!(matches!(plan, ChatPlan::MissingKey));
    assert_eq!(usage_rows(&pool).await, 0);

    let admin = AgentConfig::from_value(&json!({
        "rate_limit": { "per_user_per_hour": 1, "per_group_per_hour": 1 },
        "admins": [7]
    }));
    let plan = plan_chat(&pool, &admin, "group:1", 7, "hello", 3_001, false)
        .await
        .unwrap();
    assert!(matches!(plan, ChatPlan::MissingKey));
    assert_eq!(usage_rows(&pool).await, 0);
}
