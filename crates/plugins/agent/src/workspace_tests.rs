use fujiang_core::Source;
use serde_json::json;
use sqlx::SqlitePool;

use super::*;
use crate::session;
use crate::tools::{self, BuiltinTools, ToolSpec};

async fn pool() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    session::ensure_schema(&pool).await.unwrap();
    pool
}

fn group(id: i64, actor: i64) -> HostTurn {
    HostTurn {
        source: Source::Group { id },
        actor,
    }
}

fn private(id: i64, actor: i64) -> HostTurn {
    HostTurn {
        source: Source::Friend { id },
        actor,
    }
}

#[test]
fn command_words_do_not_take_a_target() {
    assert_eq!(parse_cmd(std::iter::empty()), WorkspaceCmd::Show);
    assert_eq!(parse_cmd(["personal"].into_iter()), WorkspaceCmd::Personal);
    assert_eq!(parse_cmd(["本人"].into_iter()), WorkspaceCmd::Personal);
    assert_eq!(parse_cmd(["group"].into_iter()), WorkspaceCmd::Group);
    assert_eq!(parse_cmd(["群"].into_iter()), WorkspaceCmd::Group);
    assert_eq!(
        parse_cmd(["personal", "10002"].into_iter()),
        WorkspaceCmd::Bad
    );
    assert_eq!(parse_cmd(["本人", "昵称"].into_iter()), WorkspaceCmd::Bad);
    assert_eq!(parse_cmd(["group", "/tmp"].into_iter()), WorkspaceCmd::Bad);
    assert!(matches!(
        crate::command::parse_agent_cmd("workspace personal"),
        crate::command::AgentCmd::Workspace(WorkspaceCmd::Personal)
    ));
}

#[tokio::test]
async fn two_people_and_two_groups_rebind_only_for_the_real_actor() {
    let pool = pool().await;
    let a = 10;
    let b = 11;
    let g1 = 20;
    let g2 = 21;
    assert_eq!(
        resolve(&pool, group(g1, a)).await.unwrap(),
        WorkspaceOwner::Group(GroupChat(g1))
    );
    command(&pool, group(g1, a), &WorkspaceCmd::Personal)
        .await
        .unwrap();
    assert_eq!(
        resolve(&pool, group(g1, a)).await.unwrap(),
        WorkspaceOwner::Personal(PersonalQq(a))
    );
    assert_eq!(
        resolve(&pool, group(g1, b)).await.unwrap(),
        WorkspaceOwner::Group(GroupChat(g1))
    );
    assert_eq!(
        resolve(&pool, group(g2, a)).await.unwrap(),
        WorkspaceOwner::Group(GroupChat(g2))
    );
    command(&pool, group(g2, b), &WorkspaceCmd::Personal)
        .await
        .unwrap();
    assert_eq!(
        resolve(&pool, group(g2, b)).await.unwrap(),
        WorkspaceOwner::Personal(PersonalQq(b))
    );
    assert_eq!(
        resolve(&pool, group(g1, a)).await.unwrap(),
        WorkspaceOwner::Personal(PersonalQq(a))
    );
    command(&pool, group(g1, a), &WorkspaceCmd::Group)
        .await
        .unwrap();
    assert_eq!(
        resolve(&pool, group(g1, a)).await.unwrap(),
        WorkspaceOwner::Group(GroupChat(g1))
    );
    assert_eq!(
        resolve(&pool, group(g2, b)).await.unwrap(),
        WorkspaceOwner::Personal(PersonalQq(b))
    );
}

#[tokio::test]
async fn private_defaults_to_self_and_rejects_a_mismatched_source() {
    let pool = pool().await;
    assert_eq!(
        resolve(&pool, private(10, 10)).await.unwrap(),
        WorkspaceOwner::Personal(PersonalQq(10))
    );
    let text = command(&pool, private(10, 10), &WorkspaceCmd::Personal)
        .await
        .unwrap();
    assert!(text.contains("当前本人"));
    assert!(resolve(&pool, private(10, 11)).await.is_err());
    let refused = command(&pool, private(10, 11), &WorkspaceCmd::Group)
        .await
        .unwrap();
    assert!(refused.contains("不一致"));
}

#[test]
fn the_same_number_is_not_the_same_owner() {
    let qq = PersonalQq(20);
    let group = GroupChat(20);
    assert_ne!(WorkspaceOwner::Personal(qq), WorkspaceOwner::Group(group));
    assert_eq!(qq.get(), group.get());
}

#[tokio::test]
async fn each_tool_call_rereads_the_choice_and_rejects_bad_turns() {
    let pool = pool().await;
    assert!(resolve(&pool, group(0, 10)).await.is_err());
    assert!(resolve(&pool, group(20, 0)).await.is_err());
    assert!(resolve(&pool, private(0, 0)).await.is_err());
    command(&pool, group(20, 20), &WorkspaceCmd::Personal)
        .await
        .unwrap();
    let personal = handle_tool(
        Some(&pool),
        Some(group(20, 20)),
        BASH_WIRE,
        &json!({"script": "true", "lang": "sh"}),
    )
    .await
    .unwrap();
    assert_eq!(personal["bound"], "本人 20");
    assert_ne!(personal["bound"], "当前群 20");
    command(&pool, group(20, 20), &WorkspaceCmd::Group)
        .await
        .unwrap();
    let restored = handle_tool(
        Some(&pool),
        Some(group(20, 20)),
        BASH_WIRE,
        &json!({"script": "true"}),
    )
    .await
    .unwrap();
    assert_eq!(restored["bound"], "当前群 20");
    assert_eq!(restored["available"], false);
}

#[tokio::test]
async fn unavailable_route_does_not_run_the_script() {
    let pool = pool().await;
    let sentinel = std::env::temp_dir().join(format!("fujiang-ws-sentinel-{}", std::process::id()));
    let _ = std::fs::remove_file(&sentinel);
    assert!(!sentinel.exists());
    let script = format!("touch {}", sentinel.display());
    let closed = handle_tool(
        Some(&pool),
        Some(group(20, 10)),
        BASH_WIRE,
        &json!({"script": script, "lang": "sh"}),
    )
    .await
    .unwrap();
    assert_eq!(closed["available"], false);
    assert_eq!(closed["error"], RUNNER_UNAVAILABLE);
    assert_eq!(closed["bound"], "当前群 20");
    assert!(!sentinel.exists());
    for key in ["path", "cwd", "actor", "identity", "user_id", "group_id"] {
        let mut args = serde_json::Map::new();
        args.insert("script".into(), json!("true"));
        args.insert(key.into(), json!("x"));
        assert!(
            handle_tool(
                Some(&pool),
                Some(group(20, 10)),
                BASH_WIRE,
                &serde_json::Value::Object(args)
            )
            .await
            .is_err(),
            "{key}"
        );
    }
    assert!(handle_tool(
        Some(&pool),
        Some(group(20, 10)),
        BASH_WIRE,
        &json!({"script": "true", "lang": null})
    )
    .await
    .is_err());
    assert!(handle_tool(
        Some(&pool),
        Some(group(20, 10)),
        CODE_WIRE,
        &json!({"script": "print(1)", "lang": 1})
    )
    .await
    .is_err());
    assert!(handle_tool(
        None,
        Some(group(20, 10)),
        CODE_WIRE,
        &json!({"script": "print(1)", "lang": "python"})
    )
    .await
    .is_err());
    assert!(handle_tool(
        Some(&pool),
        None,
        CODE_WIRE,
        &json!({"script": "print(1)", "lang": "python"})
    )
    .await
    .is_err());
    assert!(handle_tool(
        Some(&pool),
        Some(private(10, 11)),
        BASH_WIRE,
        &json!({"script": "true"})
    )
    .await
    .is_err());
    let tools = vec![ToolSpec::parse(
        "workspace.bash",
        "run",
        json!({"type": "object", "properties": {"cwd": {"type": "string"}}}),
    )
    .unwrap()];
    let listed = tools::resident_tools(&tools, BuiltinTools::OFF);
    assert!(listed.iter().all(|spec| !is_exec_wire(spec.wire_name())));
    assert!(listed.iter().all(|spec| spec.service() != "workspace"));
}
