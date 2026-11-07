use super::*;
use std::io::Write;

fn ok_report() -> String {
    CASES.iter().map(|name| format!("{name}: ok\n")).collect()
}

#[test]
fn fixture_missing_apt_line_is_not_a_successful_bootstrap() {
    assert!(is_missing_apt("MISSING\n"));
    assert!(!is_missing_apt("MISSING_PYTHON\nAPT_OK\n"));
}

#[test]
fn fixture_missing_source_is_not_acceptance() {
    let missing = missing_parts(Path::new("/no/such-fujiang-rootfs"));
    assert!(missing.iter().any(|item| item == "source"), "{missing:?}");
    assert!(missing.iter().all(|item| !item.contains("accepted")));
    let report = unrun_report();
    assert!(report.lines().count() == CASES.len());
    assert!(report.lines().all(|line| line.ends_with(": unrun")));
    assert!(!report_bound(&report, ""));
}

#[test]
fn fixture_private_dir_lists_archive_without_assuming_uidmap() {
    let parent = private_parent("src");
    let missing = missing_parts(&parent);
    for name in META {
        assert!(missing.iter().any(|item| item == name), "{missing:?}");
    }
    assert!(missing.iter().any(|item| item == "archive"), "{missing:?}");
    assert!(!missing.iter().any(|item| item.contains("24.04")));
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn fixture_claim_rejects_existing_file() {
    let parent = private_parent("file");
    let file = parent.join("keep");
    fs::write(&file, b"keep").unwrap();
    assert!(claim_new(&file).is_err());
    assert_eq!(fs::read(&file).unwrap(), b"keep");
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn fixture_claim_rejects_existing_dir() {
    let parent = private_parent("dir");
    let existing = parent.join("have");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("keep"), b"keep").unwrap();
    assert!(claim_new(&existing).is_err());
    assert_eq!(fs::read(existing.join("keep")).unwrap(), b"keep");
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn fixture_claim_drop_and_panic_remove_only_new_dir() {
    let parent = private_parent("drop");
    fs::write(parent.join("marker"), b"marker").unwrap();
    let created = parent.join("new");
    let guard = claim_new(&created).unwrap();
    let nonce = created.join("nonce");
    create_new(&nonce, b"nonce").unwrap();
    assert!(nonce.starts_with(&created));
    assert!(!parent.join("nonce").exists());
    drop(guard);
    assert_eq!(fs::read(parent.join("marker")).unwrap(), b"marker");
    assert!(!nonce.exists());
    assert!(!created.exists());
    let boom = parent.join("boom");
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = claim_new(&boom).unwrap();
        panic!("cleanup");
    }));
    assert!(!boom.exists());
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn fixture_release_reports_unreadable_residual() {
    let parent = private_parent("ro");
    let created = parent.join("new");
    let guard = claim_new(&created).unwrap();
    let blocked = created.join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("x"), b"x").unwrap();
    let mut perms = fs::metadata(&blocked).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&blocked, perms).unwrap();
    let err = guard.release();
    assert!(err.is_err(), "silent cleanup");
    assert!(created.exists(), "residual path must stay visible");
    let mut perms = fs::metadata(&blocked).unwrap().permissions();
    perms.set_mode(0o700);
    fs::set_permissions(&blocked, perms).unwrap();
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn fixture_source_rejects_symlink_dotdot_and_alias_overlap() {
    let parent = private_parent("alias");
    let source = parent.join("source");
    fs::create_dir(&source).unwrap();
    let mut perms = fs::metadata(&source).unwrap().permissions();
    perms.set_mode(0o700);
    fs::set_permissions(&source, perms).unwrap();
    let link = parent.join("link");
    std::os::unix::fs::symlink(&source, &link).unwrap();
    assert!(open_private_source(&link).is_err());
    assert!(open_private_source(Path::new("/tmp/../tmp")).is_err());
    let work = link.join("child");
    assert!(separated(&source, &work).is_err());
    assert!(absence_script().contains("set -eu"));
    assert!(absence_script().contains("ABSENT"));
    assert!(!absence_script().contains("/etc/shadow"));
    assert!(bash_restore_script().contains("chown -hRP"));
    assert!(bash_restore_script().contains("find -P"));
    assert!(needs_restore(
        true,
        "apt: failed unaccepted\npython: unrun\n"
    ));
    assert!(!needs_restore(false, "apt: unrun\nsgid: unrun\n"));
    let _ = fs::remove_dir_all(&parent);
}

fn sample_identity() -> PinnedIdentity {
    PinnedIdentity {
        archive: "ubuntu-base-24.04.1-base-amd64.tar.gz".into(),
        sha256: "ab".into(),
        bytes: 3,
        tree: [9; 32],
        primary: "843938DF228D22F7B3742BC0D94AA3F0EFE21092".into(),
        policy: "workspace-rootfs-runtime-v2".into(),
        extracted: true,
        execution_accepted: false,
    }
}

fn bound_report(id: &str) -> String {
    CASES.iter().map(|name| case_line(name, "ok", id)).collect()
}

#[test]
fn fixture_identity_pin_keeps_every_field() {
    let full = sample_identity();
    let id = full.text();
    for key in [
        "archive=",
        "sha256=",
        "bytes=",
        "tree=",
        "primary=",
        "policy=",
        "extracted=true",
        "execution_accepted=false",
    ] {
        assert!(id.contains(key), "{key} missing from {id}");
    }
    let mut slot = None;
    pin_first(&mut slot, full.clone()).unwrap();
    pin_first(&mut slot, full.clone()).unwrap();
    assert_eq!(
        slot.as_ref().map(PinnedIdentity::text).as_deref(),
        Some(id.as_str())
    );
    let variants = [
        PinnedIdentity {
            archive: "other.tar.gz".into(),
            ..full.clone()
        },
        PinnedIdentity {
            sha256: "ff".into(),
            ..full.clone()
        },
        PinnedIdentity {
            bytes: 9,
            ..full.clone()
        },
        PinnedIdentity {
            tree: [1; 32],
            ..full.clone()
        },
        PinnedIdentity {
            primary: "OTHER".into(),
            ..full.clone()
        },
        PinnedIdentity {
            policy: "other-policy".into(),
            ..full.clone()
        },
        PinnedIdentity {
            extracted: false,
            ..full.clone()
        },
        PinnedIdentity {
            execution_accepted: true,
            ..full.clone()
        },
    ];
    for other in variants {
        assert!(pin_first(&mut slot, other).is_err());
        assert_eq!(
            slot.as_ref().map(PinnedIdentity::text).as_deref(),
            Some(id.as_str())
        );
    }
    assert!(report_bound(&bound_report(&id), &id));
}

#[test]
fn fixture_report_rejects_empty_truncated_duplicate_and_mixed() {
    let id = sample_identity().text();
    let bound = bound_report(&id);
    assert!(!report_bound("", &id));
    assert!(!report_bound("shell: ok\n", &id));
    assert!(!report_bound(&ok_report(), &id));
    let mut dup = bound.clone();
    dup.push_str(&case_line("shell", "ok", &id));
    assert!(!report_bound(&dup, &id));
    let missing_sha = id.replace(" sha256=ab", "");
    assert!(!missing_sha.contains("sha256="));
    assert!(!report_bound(&bound_report(&missing_sha), &missing_sha));
    let mut mixed = String::new();
    for (n, name) in CASES.iter().enumerate() {
        let body = if n == 0 {
            id.replace("bytes=3", "bytes=9")
        } else {
            id.clone()
        };
        mixed.push_str(&case_line(name, "ok", &body));
    }
    assert!(!report_bound(&mixed, &id));
    let report = drive(
        |name| {
            if name == "apt" {
                Err("MISSING apt unaccepted: stderr".into())
            } else {
                Ok(())
            }
        },
        String::new,
    );
    assert!(report.contains("apt: MISSING apt unaccepted"));
    assert!(report.contains("root-iso: unrun"));
    assert!(report.contains("python: unrun"));
    assert!(!report_bound(&report, &id));
    assert_eq!(CASES.len(), 9);
}

#[test]
fn fixture_payload_syntax_is_not_execution() {
    assert!(apt_records_python("PYTHON_PRESENT\nAPT_OK\n"));
    assert!(apt_records_python("MISSING_PYTHON\nAPT_OK\n"));
    assert!(!apt_records_python("APT_OK\n"));
    assert!(!is_missing_apt("PYTHON_PRESENT\nAPT_OK\n"));
    assert!(root_absent_script().contains("dpkg-query -W"));
    assert!(!root_absent_script().contains("|| true"));
    assert!(root_absent_script().contains("hello install ok installed"));
    for script in [
        apt_guest_script(),
        root_mark_script(),
        root_absent_script(),
        absence_script(),
        bash_restore_script(),
    ] {
        let mut child = std::process::Command::new("/bin/bash")
            .arg("-n")
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
