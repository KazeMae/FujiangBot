use std::fs;
use std::io::{Error, ErrorKind};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
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

fn alone() -> bool {
    if std::env::var_os("FUJIANG_STAGE_ISOLATED").is_some() {
        return false;
    }
    let current = std::thread::current();
    let name = current.name().expect("test name");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--test-threads=1"])
        .env("FUJIANG_STAGE_ISOLATED", "1")
        .status()
        .unwrap();
    assert!(status.success(), "{name}");
    true
}

fn stage(name: &str) -> (TestDir, PathBuf) {
    let dir = fixture_dir();
    let bin = dir.path().join(name);
    {
        let src_path = PathBuf::from("/usr/bin").join(name);
        let mut src = fs::File::open(&src_path).unwrap();
        let mode = src.metadata().unwrap().permissions().mode();
        let mut dst = fs::File::create(&bin).unwrap();
        std::io::copy(&mut src, &mut dst).unwrap();
        dst.set_permissions(fs::Permissions::from_mode(mode & 0o777))
            .unwrap();
        dst.sync_all().unwrap();
    }
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
    if alone() {
        return;
    }
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
    if alone() {
        return;
    }
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
    if alone() {
        return;
    }
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
fn seccomp_blocks_nested_namespaces_without_blocking_plain_clone() {
    let prog = super::build_filter().unwrap();
    let keys: Vec<u32> = prog.iter().map(|insn| insn.k).collect();
    for nr in [
        libc::SYS_unshare as u32,
        libc::SYS_setns as u32,
        libc::SYS_clone as u32,
        libc::SYS_clone3 as u32,
        libc::SYS_ptrace as u32,
        libc::SYS_keyctl as u32,
    ] {
        assert!(keys.contains(&nr), "{nr}");
    }
    assert!(keys.contains(&(libc::SECCOMP_RET_ERRNO | (libc::ENOSYS as u32))));
    assert!(keys.contains(&super::namespace_clone_mask()));
    let fork_flags = libc::CLONE_CHILD_CLEARTID | libc::CLONE_CHILD_SETTID | libc::SIGCHLD;
    assert_eq!(fork_flags & super::namespace_clone_mask() as i32, 0);
}

#[test]
fn push_fixed_args_block_fd_unshare_without_as_pid_1() {
    let mut cmd = Command::new("bwrap");
    push_fixed_args(&mut cmd, 11, 12, 13, 14);
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
        "--unshare-ipc",
        "--unshare-uts",
        "--clearenv",
        "--cap-drop",
    ] {
        assert!(args.iter().any(|arg| arg == flag));
    }
    assert!(args
        .windows(2)
        .any(|pair| pair[0] == "--cap-drop" && pair[1] == "ALL"));
    assert!(args
        .iter()
        .all(|arg| arg != "--userns" && arg != "--cap-add"));
    for lower in ["/usr", "/etc", "/var"] {
        assert!(args.windows(3).any(|triple| {
            triple[0] == "--ro-bind" && triple[1] == lower && triple[2] == lower
        }));
    }
    assert!(args
        .iter()
        .all(|arg| arg != "--overlay" && arg != "--overlay-src"));
    assert!(args.iter().all(|arg| !arg.contains("varlog")
        && !arg.contains("aptcache")
        && !arg.contains("aptlists")));
    for path in ["/var/log", "/var/tmp", "/var/cache", "/etc/apt/auth.conf.d"] {
        if Path::new(path).is_dir() {
            assert!(args
                .windows(2)
                .any(|pair| pair[0] == "--tmpfs" && pair[1] == path));
        }
    }
    if Path::new("/etc/apt/auth.conf").is_file() {
        assert!(args.windows(3).any(|triple| {
            triple[0] == "--ro-bind"
                && triple[1] == "/dev/null"
                && triple[2] == "/etc/apt/auth.conf"
        }));
    }
    for tmp in ["/tmp", "/run"] {
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--tmpfs" && pair[1] == tmp));
    }
    assert!(args.iter().all(|arg| arg != "--size" && arg != "--chmod"));
    assert!(args
        .windows(2)
        .all(|pair| !(pair[0] == "--tmpfs" && pair[1] == "/sys")));
    assert_eq!(
        args.iter()
            .filter(|arg| arg.as_str() == "--bind-fd")
            .count(),
        1
    );
    assert!(args
        .windows(3)
        .any(|triple| { triple[0] == "--bind-fd" && triple[1] == "11" && triple[2] == "/work" }));
    assert!(args.iter().all(|arg| arg != "--unshare-net"));
    assert!(args.iter().all(|arg| arg != "--as-pid-1"));
}

#[test]
fn child_only_seccomp_denies_namespace_ops_and_allows_fork() {
    let mut prog = super::build_filter().unwrap();
    let mut pipes = [0; 2];
    assert_eq!(
        unsafe { libc::pipe2(pipes.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        unsafe {
            libc::close(pipes[0]);
            let keep = pipes[1];
            if keep < 3 {
                libc::_exit(91);
            }
            if keep > 3
                && libc::syscall(
                    libc::SYS_close_range,
                    3 as libc::c_long,
                    (keep - 1) as libc::c_long,
                    0 as libc::c_long,
                ) != 0
            {
                libc::_exit(91);
            }
            if libc::syscall(
                libc::SYS_close_range,
                (keep + 1) as libc::c_long,
                u32::MAX as libc::c_long,
                0 as libc::c_long,
            ) != 0
            {
                libc::_exit(91);
            }
            let mut out = [-1i32; 5];
            let nnp = libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
            let fprog = libc::sock_fprog {
                len: prog.len() as libc::c_ushort,
                filter: prog.as_mut_ptr(),
            };
            let sec = libc::syscall(libc::SYS_seccomp, 1, 0, &fprog as *const libc::sock_fprog);
            if nnp == 0 && sec == 0 {
                out[0] = 0;
                if libc::unshare(libc::CLONE_NEWUSER) != 0 {
                    out[1] = Error::last_os_error().raw_os_error().unwrap_or(-1);
                }
                let ns = libc::open(
                    c"/proc/self/ns/user".as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC,
                );
                if ns >= 0 && libc::setns(ns, libc::CLONE_NEWUSER) != 0 {
                    out[2] = Error::last_os_error().raw_os_error().unwrap_or(-1);
                }
                let mut args = [0u8; 88];
                if libc::syscall(libc::SYS_clone3, args.as_mut_ptr(), args.len()) != 0 {
                    out[3] = Error::last_os_error().raw_os_error().unwrap_or(-1);
                }
                let child = libc::fork();
                if child == 0 {
                    libc::_exit(0);
                }
                let mut status = 0;
                if child > 0
                    && libc::waitpid(child, &mut status, 0) == child
                    && libc::WIFEXITED(status)
                {
                    out[4] = 1;
                }
            }
            let bytes = std::slice::from_raw_parts(out.as_ptr() as *const u8, 20);
            let _ = libc::write(pipes[1], bytes.as_ptr() as *const libc::c_void, bytes.len());
            libc::_exit(0);
        }
    }
    unsafe { libc::close(pipes[1]) };
    let mut buf = [0u8; 20];
    let mut got = 0;
    while got < buf.len() {
        let n = unsafe {
            libc::read(
                pipes[0],
                buf[got..].as_mut_ptr() as *mut libc::c_void,
                buf.len() - got,
            )
        };
        if n <= 0 {
            let mut status = 0;
            let _ = unsafe { libc::waitpid(pid, &mut status, 0) };
            assert!(n > 0, "child closed the pipe early");
        }
        got += n as usize;
    }
    unsafe { libc::close(pipes[0]) };
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    let words = buf
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| i32::from_ne_bytes(*chunk))
        .collect::<Vec<_>>();
    assert_eq!(words[0], 0, "child failed to install filter");
    assert_eq!(words[1], libc::EPERM);
    assert_eq!(words[2], libc::EPERM);
    assert_eq!(words[3], libc::ENOSYS);
    assert_eq!(words[4], 1);
    assert_eq!(
        unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) },
        0
    );
}
