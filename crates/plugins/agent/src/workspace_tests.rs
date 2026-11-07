use std::os::fd::AsRawFd;

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
    for key in [
        "path", "cwd", "actor", "identity", "user_id", "group_id", "rootfs", "owner",
    ] {
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

struct FixtureAuth {
    host: std::path::PathBuf,
    image: std::path::PathBuf,
    identity: [u8; 32],
}

impl crate::workspace_backend::RootfsAuthority for FixtureAuth {
    fn acquire(
        &self,
    ) -> anyhow::Result<(std::path::PathBuf, crate::workspace_store::VerifiedBase)> {
        let base = crate::workspace_store::VerifiedBase::fixture(&self.image, self.identity)?;
        Ok((self.host.clone(), base))
    }
}

fn scratch_pair() -> (std::path::PathBuf, FixtureAuth) {
    let root = std::env::temp_dir().join(format!(
        "ws-chain-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let host = root.join("host");
    let image = root.join("image");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::create_dir_all(&image).unwrap();
    (
        root,
        FixtureAuth {
            host,
            image,
            identity: [9u8; 32],
        },
    )
}

fn ino(fd: std::os::fd::BorrowedFd<'_>) -> u64 {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstat(fd.as_raw_fd(), &mut st) }, 0);
    st.st_ino
}

fn write_name(dir: std::os::fd::BorrowedFd<'_>, name: &str, data: &[u8]) {
    let name = std::ffi::CString::new(name).unwrap();
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_CREAT | libc::O_WRONLY | libc::O_TRUNC | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    assert!(fd >= 0);
    assert_eq!(
        unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) },
        data.len() as isize
    );
    unsafe { libc::close(fd) };
}

fn read_name(dir: std::os::fd::BorrowedFd<'_>, name: &str) -> Option<Vec<u8>> {
    let name = std::ffi::CString::new(name).unwrap();
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return None;
    }
    let mut buf = [0u8; 64];
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    unsafe { libc::close(fd) };
    assert!(n >= 0);
    Some(buf[..n as usize].to_vec())
}

fn mock_ok(stdout: &[u8]) -> crate::workspace_runner::RunResult {
    crate::workspace_runner::RunResult {
        exit_code: 0,
        timed_out: false,
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
        truncated: false,
    }
}

async fn mock_call(
    pool: &SqlitePool,
    turn: HostTurn,
    auth: &FixtureAuth,
    args: &serde_json::Value,
    touch: Option<(&'static str, &'static [u8])>,
    seen: &mut Option<(u64, u64)>,
) -> Result<serde_json::Value, ()> {
    handle_tool_with(Some(pool), Some(turn), BASH_WIRE, args, |owner, payload| {
        crate::workspace_backend::execute_with(owner, payload, auth, |root, work, payload| {
            assert!(matches!(payload, crate::workspace_runner::Payload::Sh(_)));
            *seen = Some((ino(root), ino(work)));
            assert_ne!(ino(root), ino(work));
            if let Some((name, bytes)) = touch {
                write_name(work, name, bytes);
            }
            Ok(mock_ok(b"MOCK execution"))
        })
    })
    .await
}

#[tokio::test]
async fn mock_store_chain_separates_people_and_persists() {
    let pool = pool().await;
    let (root, auth) = scratch_pair();
    let a = 20;
    let b = 21;
    let g1 = 20;
    let g2 = 22;
    let script = json!({"script": "true", "lang": "sh"});
    let mut group_seen = None;
    let first = mock_call(
        &pool,
        group(g1, a),
        &auth,
        &script,
        Some(("group-mark", b"group")),
        &mut group_seen,
    )
    .await
    .unwrap();
    assert_eq!(first["stdout"], "MOCK execution");
    assert_eq!(first["exit_code"], 0);
    let (group_root, group_work) = group_seen.unwrap();

    command(&pool, group(g1, a), &WorkspaceCmd::Personal)
        .await
        .unwrap();
    let mut personal_seen = None;
    mock_call(
        &pool,
        group(g1, a),
        &auth,
        &script,
        Some(("personal-mark", b"personal")),
        &mut personal_seen,
    )
    .await
    .unwrap();
    let (personal_root, personal_work) = personal_seen.unwrap();
    assert_ne!(personal_work, group_work);
    assert_ne!(personal_root, group_root);

    let mut again = None;
    mock_call(&pool, group(g1, a), &auth, &script, None, &mut again)
        .await
        .unwrap();
    assert_eq!(again, Some((personal_root, personal_work)));

    let mut b_group = None;
    mock_call(&pool, group(g1, b), &auth, &script, None, &mut b_group)
        .await
        .unwrap();
    assert_eq!(b_group, Some((group_root, group_work)));

    let mut other = None;
    mock_call(&pool, group(g2, a), &auth, &script, None, &mut other)
        .await
        .unwrap();
    let (other_root, other_work) = other.unwrap();
    assert_ne!(other_work, personal_work);
    assert_ne!(other_work, group_work);
    assert_ne!(other_root, personal_root);

    command(&pool, group(g1, b), &WorkspaceCmd::Personal)
        .await
        .unwrap();
    let mut b_personal = None;
    mock_call(&pool, group(g1, b), &auth, &script, None, &mut b_personal)
        .await
        .unwrap();
    let (_b_root, b_work) = b_personal.unwrap();
    assert_ne!(b_work, personal_work);
    assert_ne!(b_work, group_work);

    command(&pool, group(g1, a), &WorkspaceCmd::Group)
        .await
        .unwrap();
    let mut rebound = None;
    mock_call(&pool, group(g1, a), &auth, &script, None, &mut rebound)
        .await
        .unwrap();
    assert_eq!(rebound, Some((group_root, group_work)));

    let probe = std::cell::Cell::new(false);
    handle_tool_with(
        Some(&pool),
        Some(group(g1, a)),
        BASH_WIRE,
        &script,
        |owner, payload| {
            crate::workspace_backend::execute_with(owner, payload, &auth, |root, work, _| {
                assert_eq!(ino(work), group_work);
                assert_eq!(
                    read_name(work, "group-mark").as_deref(),
                    Some(&b"group"[..])
                );
                assert!(read_name(work, "personal-mark").is_none());
                assert_ne!(ino(root), personal_root);
                probe.set(true);
                Ok(mock_ok(b"MOCK execution"))
            })
        },
    )
    .await
    .unwrap();
    handle_tool_with(
        Some(&pool),
        Some(group(g1, a)),
        CODE_WIRE,
        &json!({"script": "print(1)", "lang": "python"}),
        |owner, payload| {
            assert!(matches!(
                payload,
                crate::workspace_runner::Payload::Python(_)
            ));
            crate::workspace_backend::execute_with(owner, payload, &auth, |_, work, _| {
                assert_eq!(
                    read_name(work, "group-mark").as_deref(),
                    Some(&b"group"[..])
                );
                Ok(mock_ok(b"MOCK execution"))
            })
        },
    )
    .await
    .unwrap();
    command(&pool, group(g1, a), &WorkspaceCmd::Personal)
        .await
        .unwrap();
    handle_tool_with(
        Some(&pool),
        Some(group(g1, a)),
        BASH_WIRE,
        &json!({"script": "true", "lang": "bash"}),
        |owner, payload| {
            assert!(matches!(payload, crate::workspace_runner::Payload::Bash(_)));
            crate::workspace_backend::execute_with(owner, payload, &auth, |_, work, _| {
                assert_eq!(
                    read_name(work, "personal-mark").as_deref(),
                    Some(&b"personal"[..])
                );
                assert!(read_name(work, "group-mark").is_none());
                Ok(mock_ok(b"MOCK execution"))
            })
        },
    )
    .await
    .unwrap();
    assert!(probe.get());
    assert!(!production_available());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn bad_actor_and_owner_args_never_open_the_store() {
    let pool = pool().await;
    let (root, auth) = scratch_pair();
    let mut calls = 0u32;
    let exec = |owner, payload| {
        calls += 1;
        crate::workspace_backend::execute_with(owner, payload, &auth, |_, _, _| {
            Ok(mock_ok(b"MOCK execution"))
        })
    };
    assert!(handle_tool_with(
        Some(&pool),
        Some(private(10, 11)),
        BASH_WIRE,
        &json!({"script": "true"}),
        exec
    )
    .await
    .is_err());
    assert_eq!(calls, 0);
    assert!(std::fs::read_dir(auth.host.join("personal")).is_err());
    for key in ["owner", "qq", "group", "path"] {
        let mut args = serde_json::Map::new();
        args.insert("script".into(), json!("true"));
        args.insert(key.into(), json!("20"));
        assert!(
            handle_tool_with(
                Some(&pool),
                Some(group(20, 20)),
                BASH_WIRE,
                &serde_json::Value::Object(args),
                |_, _| {
                    calls += 1;
                    Ok(mock_ok(b"MOCK execution"))
                }
            )
            .await
            .is_err(),
            "{key}"
        );
    }
    assert!(handle_tool_with(
        Some(&pool),
        Some(group(20, 20)),
        BASH_WIRE,
        &json!({"script": "x".repeat(64 * 1024 + 1)}),
        |_, _| {
            calls += 1;
            Ok(mock_ok(b"MOCK execution"))
        }
    )
    .await
    .is_err());
    assert_eq!(calls, 0);
    assert!(std::fs::read_dir(&auth.host).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(root);
}
