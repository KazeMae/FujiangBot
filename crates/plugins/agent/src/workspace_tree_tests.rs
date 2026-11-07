use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::digest_limited;

fn scratch() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "wtree-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn open_dir(path: &Path) -> OwnedFd {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    assert!(fd >= 0, "{}", std::io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(fd) }
}

fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn golden(root: &Path) {
    let dir = root.join("e");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let payload = dir.join("payload");
    write_mode(&payload, b"data", 0o600);
    fs::hard_link(&payload, dir.join("alias")).unwrap();
    std::os::unix::fs::symlink("/e/payload", root.join("link")).unwrap();
}

fn hex_digest(root: &Path) -> [u8; 32] {
    let fd = open_dir(root);
    super::digest(fd.as_raw_fd()).unwrap()
}

#[test]
fn fixture_matches_python_bytes() {
    const EXPECTED: &str = "d4c7888733e446897db069da80dbd6dc00fc402689b3050f98ac36bc295d8dca";
    let root = scratch();
    golden(&root);
    let got = hex_digest(&root);
    let mut text = String::new();
    for byte in got {
        text.push_str(&format!("{byte:02x}"));
    }
    assert_eq!(text, EXPECTED, "replace after python helper");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn same_fd_is_stable_and_same_size_change_differs() {
    let root = scratch();
    golden(&root);
    let fd = open_dir(&root);
    let first = super::digest(fd.as_raw_fd()).unwrap();
    let second = super::digest(fd.as_raw_fd()).unwrap();
    assert_eq!(first, second);
    write_mode_trunc(&root.join("e/payload"), b"xxxx");
    let changed = super::digest(fd.as_raw_fd()).unwrap();
    assert_ne!(first, changed);
    let _ = fs::remove_dir_all(&root);
}

fn write_mode_trunc(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

#[test]
fn outside_hardlink_relative_escape_mount_and_fifo() {
    let root = scratch();
    let outside = scratch();
    write_mode(&outside.join("ext"), b"data", 0o600);
    fs::hard_link(outside.join("ext"), root.join("inside")).unwrap();
    let fd = open_dir(&root);
    assert!(super::digest(fd.as_raw_fd()).is_err());

    let escaped = scratch();
    std::os::unix::fs::symlink("../x", escaped.join("up")).unwrap();
    let fd = open_dir(&escaped);
    assert!(super::digest(fd.as_raw_fd()).is_err());

    let mounted = scratch();
    std::os::unix::fs::symlink("/e", mounted.join("proc")).unwrap();
    let fd = open_dir(&mounted);
    assert!(super::digest(fd.as_raw_fd()).is_err());
    let as_dir = scratch();
    fs::create_dir(as_dir.join("proc")).unwrap();
    fs::set_permissions(as_dir.join("proc"), fs::Permissions::from_mode(0o755)).unwrap();
    let fd = open_dir(&as_dir);
    assert!(super::digest(fd.as_raw_fd()).is_ok());

    let fifo = scratch();
    let name = std::ffi::CString::new(fifo.join("pipe").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let fd = open_dir(&fifo);
    assert!(super::digest(fd.as_raw_fd()).is_err());

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&outside);
    let _ = fs::remove_dir_all(&escaped);
    let _ = fs::remove_dir_all(&mounted);
    let _ = fs::remove_dir_all(&as_dir);
    let _ = fs::remove_dir_all(&fifo);
}

#[test]
fn node_budget_is_global_before_children() {
    let root = scratch();
    let dir = root.join("e");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    write_mode(&dir.join("payload"), b"data", 0o600);
    let fd = open_dir(&root);
    assert!(digest_limited(fd.as_raw_fd(), 1).is_err());
    assert!(digest_limited(fd.as_raw_fd(), 2).is_ok());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn hardlink_alias_uses_unique_byte_budget() {
    let root = scratch();
    write_mode(&root.join("payload"), b"data", 0o600);
    fs::hard_link(root.join("payload"), root.join("alias")).unwrap();
    let fd = open_dir(&root);
    let mut left = 8u64;
    let mut total = super::MAX_TOTAL - 4;
    let mut files = std::collections::HashMap::new();
    let mut rows = Vec::new();
    super::walk(
        fd.as_raw_fd(),
        b"",
        0,
        &mut left,
        &mut total,
        &mut files,
        &mut rows,
    )
    .unwrap();
    assert_eq!(total, super::MAX_TOTAL);
    let ent = files.values().next().unwrap();
    assert_eq!(ent.paths.len(), 2);
    let _ = fs::remove_dir_all(&root);
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn mode_only_file_644_to_600_and_dir_755_to_700_change_digest() {
    let root = scratch();
    let dir = root.join("e");
    fs::create_dir(&dir).unwrap();
    chmod(&dir, 0o755);
    write_mode(&dir.join("payload"), b"data", 0o644);
    chmod(&dir.join("payload"), 0o644);
    let fd = open_dir(&root);
    let file_644 = super::digest(fd.as_raw_fd()).unwrap();
    chmod(&dir.join("payload"), 0o600);
    let file_600 = super::digest(fd.as_raw_fd()).unwrap();
    assert_ne!(file_644, file_600);
    chmod(&dir, 0o700);
    let dir_700 = super::digest(fd.as_raw_fd()).unwrap();
    assert_ne!(file_600, dir_700);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ordinary_rwx_modes_hash_and_setid_sticky_unreadable_reject() {
    let ok = scratch();
    write_mode(&ok.join("rw"), b"data", 0o644);
    chmod(&ok.join("rw"), 0o664);
    fs::create_dir(ok.join("dx")).unwrap();
    chmod(&ok.join("dx"), 0o775);
    write_mode(&ok.join("dx/inner"), b"x", 0o644);
    chmod(&ok.join("dx/inner"), 0o755);
    fs::create_dir(ok.join("sticky")).unwrap();
    chmod(&ok.join("sticky"), 0o1777);
    fs::create_dir(ok.join("priv")).unwrap();
    chmod(&ok.join("priv"), 0o700);
    assert!(super::digest(open_dir(&ok).as_raw_fd()).is_ok());

    let setid = scratch();
    write_mode(&setid.join("f"), b"x", 0o644);
    chmod(&setid.join("f"), 0o4755);
    assert!(super::digest(open_dir(&setid).as_raw_fd()).is_err());

    let sticky_file = scratch();
    write_mode(&sticky_file.join("f"), b"x", 0o644);
    chmod(&sticky_file.join("f"), 0o1644);
    assert!(super::digest(open_dir(&sticky_file).as_raw_fd()).is_err());

    let unread_file = scratch();
    write_mode(&unread_file.join("f"), b"x", 0o600);
    chmod(&unread_file.join("f"), 0o200);
    assert!(super::digest(open_dir(&unread_file).as_raw_fd()).is_err());

    let unread_dir = scratch();
    fs::create_dir(unread_dir.join("d")).unwrap();
    chmod(&unread_dir.join("d"), 0o300);
    assert!(super::digest(open_dir(&unread_dir).as_raw_fd()).is_err());

    let _ = fs::remove_dir_all(&ok);
    let _ = fs::remove_dir_all(&setid);
    let _ = fs::remove_dir_all(&sticky_file);
    let _ = fs::remove_dir_all(&unread_file);
    let _ = fs::remove_dir_all(&unread_dir);
}
