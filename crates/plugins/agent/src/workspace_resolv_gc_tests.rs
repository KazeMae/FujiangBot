use std::ffi::{CString, OsString};
use std::fs;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::stage_reclaim::dir_names;
use super::{reclaim, write_guest, Stage, STAGE_MARK, SWAP, TMP_SWAP};

fn scratch(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "fujiang-resolv-gc-{}-{}",
        label,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir(&path).unwrap();
    path
}

fn root_fd(path: &Path) -> OwnedFd {
    let c = CString::new(path.as_os_str().as_bytes()).unwrap();
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::open(c.as_ptr(), flags) };
    assert!(fd >= 0);
    unsafe { OwnedFd::from_raw_fd(fd) }
}

fn etc_of(root: &Path) -> PathBuf {
    let etc = root.join("etc");
    fs::create_dir_all(&etc).unwrap();
    etc
}

#[test]
fn dir_scan_counts_non_utf8_names() {
    let root = scratch("scan");
    let bad = root.join("x");
    let name = OsString::from_vec(vec![0xff]);
    let path = root.join(&name);
    let _ = bad;
    fs::write(&path, b"z").unwrap();
    let fd = root_fd(&root);
    assert!(dir_names(fd.as_raw_fd(), 0).is_err());
    assert!(dir_names(fd.as_raw_fd(), 1).unwrap().is_empty());
    fs::write(root.join("ok"), b"z").unwrap();
    assert!(dir_names(fd.as_raw_fd(), 1).is_err());
    let _ = fs::remove_dir_all(&root);
}

fn plant(path: &Path, body: &[u8]) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn crash_orphan_removed_and_live_lock_kept() {
    let uid = unsafe { libc::geteuid() };
    let root = scratch("orphan");
    let etc = etc_of(&root);
    let fd = root_fd(&etc);
    plant(&etc.join(".resolv.4.4.hold"), STAGE_MARK);
    plant(&etc.join(".resolv.4.4.tmp"), b"orphan-body");
    plant(&etc.join(".resolv.01.2.hold"), STAGE_MARK);
    plant(&etc.join(".resolv.nope.hold"), STAGE_MARK);
    plant(&etc.join("resolv.conf.fujiang"), b"leave");
    fs::write(root.join("sentinel"), b"OUT").unwrap();
    assert!(reclaim(fd.as_raw_fd(), uid.wrapping_add(1)).is_ok());
    assert!(etc.join(".resolv.4.4.hold").exists());
    reclaim(fd.as_raw_fd(), uid).unwrap();
    assert!(!etc.join(".resolv.4.4.hold").exists());
    assert!(!etc.join(".resolv.4.4.tmp").exists());
    assert_eq!(fs::read(etc.join(".resolv.01.2.hold")).unwrap(), STAGE_MARK);
    assert_eq!(fs::read(etc.join(".resolv.nope.hold")).unwrap(), STAGE_MARK);
    assert_eq!(fs::read(etc.join("resolv.conf.fujiang")).unwrap(), b"leave");
    assert_eq!(fs::read(root.join("sentinel")).unwrap(), b"OUT");

    let hold_path = etc.join(".resolv.5.5.hold");
    plant(&hold_path, STAGE_MARK);
    let c = CString::new(hold_path.as_os_str().as_bytes()).unwrap();
    let held = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    assert!(held >= 0);
    let held = unsafe { OwnedFd::from_raw_fd(held) };
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    reclaim(fd.as_raw_fd(), uid).unwrap();
    assert_eq!(fs::read(&hold_path).unwrap(), STAGE_MARK);

    let mut child = Command::new("/usr/bin/python3");
    child
        .arg("-I")
        .arg("-c")
        .arg("import os; os.write(1,b'R'); os.read(0,1)")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let raw = held.as_raw_fd();
    unsafe {
        child.pre_exec(move || {
            let flags = libc::fcntl(raw, libc::F_GETFD);
            if flags < 0 || libc::fcntl(raw, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = child.spawn().unwrap();
    drop(held);
    let mut out = child.stdout.take().unwrap();
    let mut byte = [0u8; 1];
    out.read_exact(&mut byte).unwrap();
    reclaim(fd.as_raw_fd(), uid).unwrap();
    assert_eq!(fs::read(&hold_path).unwrap(), STAGE_MARK);
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    reclaim(fd.as_raw_fd(), uid).unwrap();
    assert!(!hold_path.exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn malicious_stage_and_swap_are_not_removed() {
    let uid = unsafe { libc::geteuid() };
    let root = scratch("bad");
    let etc = etc_of(&root);
    let fd = root_fd(&etc);
    let fifo = etc.join(".resolv.9.1.hold");
    let c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
    fs::create_dir(etc.join(".resolv.9.2.hold")).unwrap();
    let outside = scratch("out");
    fs::write(outside.join("keep"), b"KEEP").unwrap();
    std::os::unix::fs::symlink(outside.join("keep"), etc.join(".resolv.9.3.hold")).unwrap();
    plant(&etc.join(".resolv.9.4.hold"), STAGE_MARK);
    fs::hard_link(etc.join(".resolv.9.4.hold"), etc.join(".resolv.9.4.alias")).unwrap();
    plant(&etc.join(".resolv.9.4.tmp"), b"paired");
    reclaim(fd.as_raw_fd(), uid).unwrap();
    assert!(fifo.exists());
    assert!(etc.join(".resolv.9.2.hold").is_dir());
    assert!(fs::symlink_metadata(etc.join(".resolv.9.3.hold"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"KEEP");
    assert_eq!(fs::read(etc.join(".resolv.9.4.hold")).unwrap(), STAGE_MARK);
    assert_eq!(fs::read(etc.join(".resolv.9.4.alias")).unwrap(), STAGE_MARK);
    assert_eq!(fs::read(etc.join(".resolv.9.4.tmp")).unwrap(), b"paired");

    let swapped = etc.join(".resolv.3.3.hold");
    plant(&swapped, STAGE_MARK);
    SWAP.with(|slot| {
        let path = swapped.clone();
        *slot.borrow_mut() = Some(Box::new(move || {
            let backup = path.with_file_name("kept-inode");
            fs::rename(&path, &backup).unwrap();
            fs::write(&path, b"NEW").unwrap();
        }));
    });
    reclaim(fd.as_raw_fd(), uid).unwrap();
    assert_eq!(fs::read(etc.join("kept-inode")).unwrap(), STAGE_MARK);
    assert_eq!(fs::read(&swapped).unwrap(), b"NEW");

    fs::write(etc.join("resolv.conf"), b"nameserver 192.0.2.1\n").unwrap();
    let mut perms = fs::metadata(etc.join("resolv.conf")).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(etc.join("resolv.conf"), perms).unwrap();
    let guest = root_fd(&root);
    write_guest(guest.as_raw_fd(), b"nameserver 192.0.2.1\n").unwrap();
    assert_eq!(
        fs::read(etc.join("resolv.conf")).unwrap(),
        b"nameserver 192.0.2.1\n"
    );
    assert!(fifo.exists());
    assert!(etc.join(".resolv.9.2.hold").is_dir());
    assert!(fs::symlink_metadata(etc.join(".resolv.9.3.hold"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(etc.join(".resolv.9.4.hold")).unwrap(), STAGE_MARK);
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&outside);
}

#[test]
fn paired_cleanup_failure_keeps_claim_and_hold_only_restarts() {
    let uid = unsafe { libc::geteuid() };
    let root = scratch("pair");
    let etc = etc_of(&root);
    let fd = root_fd(&etc);
    plant(&etc.join(".resolv.8.8.hold"), STAGE_MARK);
    plant(&etc.join(".resolv.8.8.tmp"), b"body");
    TMP_SWAP.with(|slot| {
        let path = etc.join(".resolv.8.8.tmp");
        *slot.borrow_mut() = Some(Box::new(move || {
            let backup = path.with_file_name("tmp-inode");
            fs::rename(&path, &backup).unwrap();
            fs::write(&path, b"REPLACED").unwrap();
        }));
    });
    reclaim(fd.as_raw_fd(), uid).unwrap();
    assert_eq!(fs::read(etc.join(".resolv.8.8.hold")).unwrap(), STAGE_MARK);
    assert_eq!(fs::read(etc.join(".resolv.8.8.tmp")).unwrap(), b"REPLACED");
    assert_eq!(fs::read(etc.join("tmp-inode")).unwrap(), b"body");
    fs::remove_file(etc.join(".resolv.8.8.tmp")).unwrap();
    let c = CString::new(etc.join(".resolv.8.8.tmp").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
    let max = u64::MAX.to_string();
    plant(&etc.join(format!(".resolv.1.{max}.hold")), STAGE_MARK);
    plant(&etc.join(format!(".resolv.1.{max}.tmp")), b"wide");
    let too_long = format!(".resolv.1.{}.hold", "9".repeat(21));
    plant(&etc.join(&too_long), STAGE_MARK);
    reclaim(fd.as_raw_fd(), uid).unwrap();
    assert!(!etc.join(format!(".resolv.1.{max}.hold")).exists());
    assert!(!etc.join(format!(".resolv.1.{max}.tmp")).exists());
    assert_eq!(fs::read(etc.join(&too_long)).unwrap(), STAGE_MARK);
    assert_eq!(fs::read(etc.join(".resolv.8.8.hold")).unwrap(), STAGE_MARK);
    fs::remove_file(etc.join(".resolv.8.8.tmp")).unwrap();
    reclaim(fd.as_raw_fd(), uid).unwrap();
    assert!(!etc.join(".resolv.8.8.hold").exists());
    assert_eq!(fs::read(etc.join("tmp-inode")).unwrap(), b"body");
    plant(&etc.join(".resolv.legacy.tmp"), b"legacy");
    plant(&etc.join(".resolv.2.2.hold"), STAGE_MARK);
    reclaim(fd.as_raw_fd(), uid).unwrap();
    assert!(!etc.join(".resolv.2.2.hold").exists());
    assert_eq!(fs::read(etc.join(".resolv.legacy.tmp")).unwrap(), b"legacy");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn real_stage_crash_is_reclaimed_and_drop_keeps_replacement() {
    if std::env::var_os("FUJIANG_RESOLV_STAGE_WORKER").is_some() {
        let etc = PathBuf::from(std::env::var("FUJIANG_RESOLV_ETC").unwrap());
        let fd = root_fd(&etc);
        let (stage, body) = Stage::create(fd.as_raw_fd()).unwrap();
        super::write_full(body.as_raw_fd(), b"nameserver 192.0.2.1\n").unwrap();
        std::mem::forget(stage);
        std::mem::forget(body);
        std::process::exit(0);
    }
    let uid = unsafe { libc::geteuid() };
    let root = scratch("worker");
    let etc = etc_of(&root);
    let fd = root_fd(&etc);
    plant(&etc.join(".resolv.legacy.tmp"), b"legacy");
    let worker = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("workspace_runner::workspace_resolv::gc_tests::real_stage_crash_is_reclaimed_and_drop_keeps_replacement")
        .env("FUJIANG_RESOLV_STAGE_WORKER", "1")
        .env("FUJIANG_RESOLV_ETC", &etc)
        .output()
        .unwrap();
    assert!(
        worker.status.success(),
        "{}",
        String::from_utf8_lossy(&worker.stderr)
    );
    let holds: Vec<_> = fs::read_dir(&etc)
        .unwrap()
        .map(|ent| ent.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".hold"))
        .collect();
    assert_eq!(holds.len(), 1, "{holds:?}");
    assert!(etc.join(holds[0].replace(".hold", ".tmp")).is_file());
    reclaim(fd.as_raw_fd(), uid).unwrap();
    assert!(!etc.join(&holds[0]).exists());
    assert!(!etc.join(holds[0].replace(".hold", ".tmp")).exists());
    assert_eq!(fs::read(etc.join(".resolv.legacy.tmp")).unwrap(), b"legacy");

    let (stage, body) = Stage::create(fd.as_raw_fd()).unwrap();
    let tmp_name = etc
        .read_dir()
        .unwrap()
        .map(|ent| ent.unwrap().file_name().into_string().unwrap())
        .find(|name| name.ends_with(".tmp") && name != ".resolv.legacy.tmp")
        .unwrap();
    let tmp_path = etc.join(&tmp_name);
    let backup = etc.join("created-inode");
    fs::rename(&tmp_path, &backup).unwrap();
    fs::write(&tmp_path, b"UNRELATED").unwrap();
    drop(body);
    drop(stage);
    assert_eq!(fs::read(&tmp_path).unwrap(), b"UNRELATED");
    assert!(backup.is_file());
    assert!(fs::read_dir(&etc).unwrap().all(|ent| {
        !ent.unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".hold")
    }));
    let _ = fs::remove_dir_all(&root);
}
