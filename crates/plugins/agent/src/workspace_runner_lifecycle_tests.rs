use std::fs;
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{pidfd_open, push_fixed_args, wait_wall, Guard};

struct TestDir(PathBuf);

impl TestDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture_dir() -> TestDir {
    let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target/runner-fixtures");
    fs::create_dir_all(&parent).unwrap();
    static SEQ: AtomicU64 = AtomicU64::new(0);
    loop {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!("{}-{}", std::process::id(), n));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return TestDir(path),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => panic!("{err}"),
        }
    }
}

fn stage(name: &str) -> (TestDir, PathBuf) {
    let dir = fixture_dir();
    let bin = dir.path().join(name);
    fs::copy(PathBuf::from("/usr/bin").join(name), &bin).unwrap();
    (dir, bin)
}

fn poll_exited(fd: &impl AsRawFd) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut pfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, 20) };
        assert!(rc >= 0);
        if rc > 0 {
            assert!(pfd.revents & libc::POLLIN != 0);
            return;
        }
        assert!(Instant::now() < deadline);
    }
}

fn assert_zombie(pid: u32) {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = text.rsplit_once(')').unwrap().1;
    assert_eq!(rest.split_whitespace().next(), Some("Z"));
}

fn assert_echild(pid: u32) {
    let rc = unsafe { libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG) };
    assert_eq!(rc, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn true_polls_pidfd_before_wait_and_finish_reaps() {
    let (dir, bin) = stage("true");
    let child = Command::new(&bin)
        .env_clear()
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let pidfd = pidfd_open(pid as i32).unwrap();
    poll_exited(&pidfd);
    assert_zombie(pid);
    let mut guard = Guard {
        child: Some(child),
        status: None,
        pidfd: Some(pidfd),
        armed: true,
    };
    let status = guard.finish().unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(!guard.armed);
    assert_echild(pid);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn sleep_wait_wall_deadline_reaps() {
    let (dir, bin) = stage("sleep");
    let child = Command::new(&bin)
        .arg("5")
        .env_clear()
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut guard = Guard {
        child: Some(child),
        status: None,
        pidfd: Some(pidfd_open(pid as i32).unwrap()),
        armed: true,
    };
    let (status, timed_out) =
        wait_wall(&mut guard, Instant::now() + Duration::from_millis(30)).unwrap();
    assert!(timed_out);
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(!guard.armed);
    assert_echild(pid);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn finish_without_pidfd_stays_armed_and_drop_reaps() {
    let (dir, bin) = stage("sleep");
    let child = Command::new(&bin)
        .arg("5")
        .env_clear()
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let watch = pidfd_open(pid as i32).unwrap();
    let mut guard = Guard {
        child: Some(child),
        status: None,
        pidfd: None,
        armed: true,
    };
    let err = guard.finish().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Other);
    assert!(guard.armed);
    drop(guard);
    poll_exited(&watch);
    assert_echild(pid);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn push_fixed_args_block_fd_unshare_without_as_pid_1() {
    let mut cmd = Command::new("bwrap");
    push_fixed_args(&mut cmd, 10, 11, 12, 13, 14);
    let args: Vec<String> = cmd
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--block-fd" && pair[1] == "13"));
    for flag in [
        "--unshare-user",
        "--unshare-pid",
        "--unshare-net",
        "--unshare-ipc",
        "--unshare-uts",
    ] {
        assert!(args.iter().any(|arg| arg == flag));
    }
    assert!(args.iter().all(|arg| arg != "--as-pid-1"));
}
