#[test]
fn failed_stage_directory_is_removed() {
    let name = format!(
        "fujiang-runtime-abandon-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let path = std::env::temp_dir().join(&name);
    let err = super::abandon_fresh_stage(&name).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Other);
    assert!(!path.exists(), "{}", path.display());
}

#[test]
fn discovery_timeout_drains_and_reaps() {
    use std::process::Command;
    use std::time::{Duration, Instant};
    let mut cmd = Command::new("/usr/bin/python3");
    cmd.arg("-I")
        .arg("-c")
        .arg("import sys,time; sys.stdout.write('Z'*200000); sys.stdout.flush(); time.sleep(30)");
    let start = Instant::now();
    let err = super::output_timeout(&mut cmd).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(start.elapsed() < Duration::from_secs(8));
}

fn scratch() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "ws-link-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(p.join("pub/sub")).unwrap();
    std::fs::create_dir_all(p.join("stage")).unwrap();
    std::fs::write(p.join("pub/target"), b"PINNED").unwrap();
    p
}

fn mat(src: &std::path::Path, dst: &std::path::Path, root: &str) -> std::io::Result<()> {
    super::materialize_public_link(src, dst, &[root], false)
}

#[test]
fn public_link_materialize_stays_inside_root() {
    let p = scratch();
    let root = p.join("pub");
    let allow = root.to_str().unwrap();
    std::os::unix::fs::symlink("../target", root.join("sub/rel")).unwrap();
    let dst = p.join("stage/rel");
    mat(&root.join("sub/rel"), &dst, allow).unwrap();
    assert_eq!(std::fs::read(&dst).unwrap(), b"PINNED");
    assert!(std::fs::symlink_metadata(&dst).unwrap().is_file());
    std::os::unix::fs::symlink(root.join("target"), root.join("abs")).unwrap();
    mat(&root.join("abs"), &p.join("stage/abs"), allow).unwrap();
    assert_eq!(std::fs::read(p.join("stage/abs")).unwrap(), b"PINNED");
    std::os::unix::fs::symlink("/etc/passwd", root.join("out")).unwrap();
    assert!(mat(&root.join("out"), &p.join("stage/out"), allow)
        .unwrap_err()
        .to_string()
        .contains("escape"));
    assert!(!p.join("stage/out").exists());
    std::os::unix::fs::symlink("../../outside", root.join("sub/leave")).unwrap();
    std::fs::create_dir(root.join("sub/realdir")).unwrap();
    std::os::unix::fs::symlink("realdir", root.join("sub/dirlink")).unwrap();
    assert!(mat(&root.join("sub/dirlink"), &p.join("stage/dir"), allow)
        .unwrap_err()
        .to_string()
        .contains("directory"));
    assert!(mat(&root.join("sub/leave"), &p.join("stage/leave"), allow)
        .unwrap_err()
        .to_string()
        .contains("escape"));
    std::os::unix::fs::symlink("mid", root.join("a")).unwrap();
    std::os::unix::fs::symlink("target", root.join("mid")).unwrap();
    mat(&root.join("a"), &p.join("stage/chain"), allow).unwrap();
    assert_eq!(std::fs::read(p.join("stage/chain")).unwrap(), b"PINNED");
    std::os::unix::fs::symlink("b", root.join("cyc-a")).unwrap();
    std::os::unix::fs::symlink("cyc-a", root.join("b")).unwrap();
    assert!(mat(&root.join("cyc-a"), &p.join("stage/cyc"), allow)
        .unwrap_err()
        .to_string()
        .contains("limit"));
    let mut prev = root.join("hop0");
    std::fs::write(&prev, b"PINNED").unwrap();
    for i in 1..=9 {
        let next = root.join(format!("hop{i}"));
        std::os::unix::fs::symlink(prev.file_name().unwrap(), &next).unwrap();
        prev = next;
    }
    assert!(mat(&prev, &p.join("stage/hops"), allow)
        .unwrap_err()
        .to_string()
        .contains("limit"));
    let _ = std::fs::remove_dir_all(&p);
}

#[test]
fn public_link_fifo_and_replaced_source_fail() {
    let p = scratch();
    let root = p.join("pub");
    let allow = root.to_str().unwrap();
    let fifo = root.join("fifo");
    assert_eq!(
        unsafe {
            libc::mkfifo(
                std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())
                    .unwrap()
                    .as_ptr(),
                0o644,
            )
        },
        0
    );
    std::os::unix::fs::symlink("fifo", root.join("via")).unwrap();
    let start = std::time::Instant::now();
    assert!(mat(&root.join("via"), &p.join("stage/fifo"), allow).is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    assert!(!p.join("stage/fifo").exists());
    super::set_race_swap(Some(root.join("target")));
    std::os::unix::fs::symlink("target", root.join("swap")).unwrap();
    let raced = mat(&root.join("swap"), &p.join("stage/swap"), allow);
    super::set_race_swap(None);
    let raced = raced.unwrap_err();
    assert!(raced.to_string().contains("race"), "{raced}");
    assert!(!p.join("stage/swap").exists());
    let _ = std::fs::remove_dir_all(&p);
}

#[test]
fn only_exact_sitecustomize_hook_is_omitted() {
    let src = std::path::Path::new("/usr/lib/python3.14/sitecustomize.py");
    let rel = std::path::Path::new("../../../etc/python3.14/sitecustomize.py");
    let abs = std::path::Path::new("/etc/python3.14/sitecustomize.py");
    assert!(super::sitecustomize_hook(src, rel));
    assert!(super::sitecustomize_hook(src, abs));
    assert!(super::sitecustomize_hook(
        std::path::Path::new("/usr/lib64/python3.14/sitecustomize.py"),
        rel
    ));
    assert!(!super::sitecustomize_hook(
        std::path::Path::new("/usr/lib/python3.14/other.py"),
        rel
    ));
    assert!(!super::sitecustomize_hook(
        src,
        std::path::Path::new("../../../etc/python3.13/sitecustomize.py")
    ));
    assert!(!super::sitecustomize_hook(
        src,
        std::path::Path::new("../../../etc/python3.14/other.py")
    ));
    assert!(!super::sitecustomize_hook(
        std::path::Path::new("/tmp/python3.14/sitecustomize.py"),
        abs
    ));
    let p = scratch();
    let tree = p.join("tree");
    let dst = p.join("out");
    std::fs::create_dir(&tree).unwrap();
    std::fs::create_dir(&dst).unwrap();
    std::os::unix::fs::symlink(rel, tree.join("sitecustomize.py")).unwrap();
    assert!(super::copy_dir(&tree, &dst, &p.join("stage"), 0).is_err());
    assert!(!dst.join("sitecustomize.py").exists());
    let _ = std::fs::remove_dir_all(&p);
}
