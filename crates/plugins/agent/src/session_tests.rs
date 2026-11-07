use super::*;

async fn mem() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    ensure_schema(&pool).await.unwrap();
    ensure_schema(&pool).await.unwrap();
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
             WHERE name IN ('sessions', 'messages', 'messages_session', 'usage_events', 'usage_daily')
             ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        names,
        vec![
            "messages".to_string(),
            "messages_session".to_string(),
            "sessions".to_string(),
            "usage_daily".to_string(),
            "usage_events".to_string(),
        ]
    );
    let cols: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('sessions') ORDER BY name")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        cols,
        vec![
            "fold_rev".to_string(),
            "persona".to_string(),
            "session_key".to_string(),
            "summary".to_string(),
            "updated_at".to_string(),
        ]
    );
    pool
}

#[test]
fn group_is_one_key_and_private_is_per_friend() {
    assert_eq!(session_key(Source::Group { id: 7 }), "group:7");
    assert_ne!(
        session_key(Source::Group { id: 7 }),
        session_key(Source::Group { id: 8 })
    );
    assert_eq!(session_key(Source::Friend { id: 3 }), "private:3");
    assert_ne!(
        session_key(Source::Friend { id: 3 }),
        session_key(Source::Friend { id: 4 })
    );
    assert_ne!(
        session_key(Source::Group { id: 3 }),
        session_key(Source::Friend { id: 3 })
    );
}

#[tokio::test]
async fn history_keeps_the_latest_pairs_for_one_session() {
    let pool = mem().await;
    let group = session_key(Source::Group { id: 7 });
    let other = session_key(Source::Friend { id: 3 });
    save_exchange(&pool, &group, 1, "u1", "a1").await.unwrap();
    save_exchange(&pool, &group, 2, "u2", "a2").await.unwrap();
    save_exchange(&pool, &group, 1, "u3", "a3").await.unwrap();
    save_exchange(&pool, &other, 3, "solo", "ok").await.unwrap();

    let last = recent_turns(&pool, &group, 1).await.unwrap();
    assert_eq!(
        last,
        vec![
            ChatMessage {
                role: Role::User,
                content: "u3".into(),
                images: Vec::new(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "a3".into(),
                images: Vec::new(),
            },
        ]
    );
    let all = recent_turns(&pool, &group, 20).await.unwrap();
    assert_eq!(all.len(), 6);
    assert_eq!(all[0].content, "u1");
    assert_eq!(all[5].content, "a3");
    assert!(recent_turns(&pool, &group, 0).await.unwrap().is_empty());

    let user_ids: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT role, user_id FROM messages WHERE session_key = ?1 ORDER BY id")
            .bind(&group)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        user_ids,
        vec![
            ("user".into(), Some(1)),
            ("assistant".into(), None),
            ("user".into(), Some(2)),
            ("assistant".into(), None),
            ("user".into(), Some(1)),
            ("assistant".into(), None),
        ]
    );

    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE session_key = ?1")
        .bind(&group)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sessions, 1);

    let left = recent_turns(&pool, &other, 20).await.unwrap();
    assert_eq!(left.len(), 2);
    assert_eq!(left[0].content, "solo");
}

#[tokio::test]
async fn old_sessions_table_gains_persona_and_keeps_rows() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE sessions (
                session_key TEXT PRIMARY KEY,
                updated_at INTEGER NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO sessions (session_key, updated_at) VALUES ('group:1', 5)")
        .execute(&pool)
        .await
        .unwrap();
    ensure_schema(&pool).await.unwrap();
    ensure_schema(&pool).await.unwrap();
    let persona: Option<String> =
        sqlx::query_scalar("SELECT persona FROM sessions WHERE session_key = 'group:1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(persona, None);
    let summary: Option<String> =
        sqlx::query_scalar("SELECT summary FROM sessions WHERE session_key = 'group:1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(summary, None);
    set_persona(&pool, "group:1", "strict", 9).await.unwrap();
    save_exchange(&pool, "group:1", 3, "hi", "yo")
        .await
        .unwrap();
    assert_eq!(
        persona_of(&pool, "group:1").await.unwrap().as_deref(),
        Some("strict")
    );
    let updated: i64 =
        sqlx::query_scalar("SELECT updated_at FROM sessions WHERE session_key = 'group:1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(updated >= 9);
}

#[tokio::test]
async fn daily_rows_add_within_one_utc_day_and_split_on_the_next() {
    let pool = mem().await;
    let day = 86_400 * 10;
    record_daily(&pool, "group:7", 5, day + 10, 1, 4, 2)
        .await
        .unwrap();
    record_daily(&pool, "group:7", 5, day + 20, 1, 0, 0)
        .await
        .unwrap();
    record_daily(&pool, "private:5", 5, day + 30, 1, 10, 1)
        .await
        .unwrap();
    record_daily(&pool, "group:7", 8, day + 40, 1, 7, 8)
        .await
        .unwrap();
    record_daily(&pool, "group:7", 5, day, 0, 0, 0)
        .await
        .unwrap();

    let today = today_usage(&pool, "group:7", 5, day + 50).await.unwrap();
    assert_eq!(
        today.user,
        DayUsage {
            calls: 3,
            in_tokens: 14,
            out_tokens: 3,
        }
    );
    assert_eq!(
        today.session,
        DayUsage {
            calls: 3,
            in_tokens: 11,
            out_tokens: 10,
        }
    );
    let other = today_usage(&pool, "group:8", 9, day + 50).await.unwrap();
    assert_eq!(
        other,
        TodayUsage {
            user: DayUsage::default(),
            session: DayUsage::default(),
        }
    );

    record_daily(&pool, "group:7", 5, day + 86_400, 1, 3, 3)
        .await
        .unwrap();
    let next = today_usage(&pool, "group:7", 5, day + 86_400 + 1)
        .await
        .unwrap();
    assert_eq!(
        next.user,
        DayUsage {
            calls: 1,
            in_tokens: 3,
            out_tokens: 3,
        }
    );
    assert_eq!(next.session, next.user);
    clear_messages(&pool, "group:7").await.unwrap();
    let kept = today_usage(&pool, "group:7", 5, day + 50).await.unwrap();
    assert_eq!(kept.user.calls, 3);
    assert_eq!(kept.session.calls, 3);
}
