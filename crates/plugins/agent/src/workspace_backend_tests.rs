use std::os::unix::fs::PermissionsExt;

use super::*;
use fujiang_core::Source;

async fn resolved_owner(source: Source, actor: i64) -> crate::workspace::WorkspaceOwner {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    crate::workspace::ensure_schema(&pool).await.unwrap();
    crate::workspace::resolve(&pool, crate::workspace::HostTurn { source, actor })
        .await
        .unwrap()
}

#[tokio::test]
async fn production_authority_never_reaches_the_runner() {
    assert!(!production_available());
    assert!(ProductionSupply.acquire().is_err());
    let owner = resolved_owner(Source::Friend { id: 1 }, 1).await;
    let mut called = false;
    let err = execute_with(
        owner,
        crate::workspace_runner::Payload::Sh("true".to_string()),
        &ProductionSupply,
        |_, _, _| {
            called = true;
            Ok(crate::workspace_runner::RunResult {
                exit_code: 0,
                timed_out: false,
                stdout: Vec::new(),
                stderr: Vec::new(),
                truncated: false,
            })
        },
    )
    .unwrap_err();
    assert!(!called);
    let text = err.to_string();
    assert!(
        text.contains("accepted official rootfs unavailable"),
        "{text}"
    );
}

struct FixtureAuth {
    host: std::path::PathBuf,
    image: std::path::PathBuf,
}

impl RootfsAuthority for FixtureAuth {
    fn acquire(&self) -> anyhow::Result<(PathBuf, VerifiedBase)> {
        Ok((
            self.host.clone(),
            VerifiedBase::fixture(&self.image, [3u8; 32])?,
        ))
    }
}

#[tokio::test]
async fn existing_runner_failclosed_on_incomplete_root() {
    let root = std::env::temp_dir().join(format!(
        "ws-fail-{}-{}",
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
    let mode = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(&root, mode.clone()).unwrap();
    std::fs::set_permissions(&host, mode.clone()).unwrap();
    std::fs::set_permissions(&image, mode).unwrap();
    let sentinel = root.join("host-sentinel");
    std::fs::write(&sentinel, b"untouched").unwrap();
    let auth = FixtureAuth { host, image };
    let owner = resolved_owner(Source::Group { id: 4 }, 4).await;
    for payload in [
        crate::workspace_runner::Payload::Bash("true".to_string()),
        crate::workspace_runner::Payload::Python("print(1)".to_string()),
    ] {
        let mut entered = false;
        let outcome = execute_with(owner, payload, &auth, |root, work, payload| {
            entered = true;
            crate::workspace_runner::run(root, work, payload)
        });
        assert!(entered, "runner was not entered: {outcome:?}");
        match &outcome {
            Err(err) => {
                let text = err.to_string();
                eprintln!("native runner diagnostic: {text}");
                assert!(!text.to_lowercase().contains("ubuntu"), "{text}");
            }
            Ok(result) => {
                eprintln!(
                    "native runner diagnostic: exit={} stdout={} stderr={}",
                    result.exit_code,
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
                assert_ne!(result.exit_code, 0);
                let blob = format!(
                    "{}{}",
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
                assert!(!blob.to_lowercase().contains("ubuntu"), "{blob}");
            }
        }
        let mut reopened = false;
        let reopen = execute_with(
            owner,
            crate::workspace_runner::Payload::Sh("true".to_string()),
            &auth,
            |_, _, _| {
                reopened = true;
                Err(anyhow::anyhow!("reopen probe"))
            },
        );
        assert!(reopened, "owner lock still held: {reopen:?}");
        assert!(reopen.is_err());
    }
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"untouched");
    let _ = std::fs::remove_dir_all(root);
}
