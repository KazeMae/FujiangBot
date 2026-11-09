use std::ffi::CStr;
use std::fmt;
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[path = "workspace_runner_io.rs"]
mod workspace_runner_io;

use workspace_runner_io::{recv_bound, spawn_drain};

#[path = "workspace_runner_mount.rs"]
mod mount;

use mount::{push_fixed_args, scope_landlock};

use anyhow::{bail, Context, Result};
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::Deserialize;

const WALL: Duration = Duration::from_secs(300);
const CPU_SECONDS: libc::rlim_t = 120;
const REAP_BOUND: Duration = Duration::from_secs(1);
const OUT_CAP: usize = 64 * 1024;
const INFO_CAP: usize = 8 * 1024;
const CLOSE_RANGE_CLOEXEC: libc::c_long = 4;
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH_X86_64: u32 = 0xc000003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH_AARCH64: u32 = 0xc00000b7;
const X32_SYSCALL_BIT: u32 = 0x4000_0000;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;
const ROOT_DIRS: [&CStr; 6] = [
    c"upper",
    c"ovlwork",
    c"work",
    c"varlog",
    c"aptcache",
    c"aptlists-partial",
];

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[derive(Debug)]
pub enum Payload {
    Bash(String),
    Sh(String),
    Python(String),
}

#[derive(Debug)]
pub struct RunResult {
    pub exit_code: i32,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

struct Guard {
    child: Option<Child>,
    status: Option<ExitStatus>,
    pidfd: Option<OwnedFd>,
    armed: bool,
}

impl Guard {
    fn signal_init(&self) -> io::Result<()> {
        let Some(pidfd) = self.pidfd.as_ref() else {
            return Ok(());
        };
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd() as libc::c_long,
                libc::SIGKILL as libc::c_long,
                0 as libc::c_long,
                0 as libc::c_long,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(err)
        }
    }

    fn signal_bwrap(&self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        let Some(child) = self.child.as_ref() else {
            return Ok(());
        };
        let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGKILL) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(err)
        }
    }

    fn poll_init(&self) -> io::Result<()> {
        let pidfd = self
            .pidfd
            .as_ref()
            .ok_or_else(|| io::Error::other("namespace init pidfd missing"))?;
        let deadline = Instant::now() + REAP_BOUND;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    ErrorKind::TimedOut,
                    "init reap exceeded bound",
                ));
            }
            let wait_ms = i32::try_from(deadline.saturating_duration_since(now).as_millis())
                .unwrap_or(i32::MAX);
            let mut pfd = libc::pollfd {
                fd: pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let rc = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if rc == 0 {
                return Err(io::Error::new(
                    ErrorKind::TimedOut,
                    "init reap exceeded bound",
                ));
            }
            if pfd.revents & libc::POLLIN != 0 {
                return Ok(());
            }
            if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err(io::Error::other("pidfd poll failed"));
            }
        }
    }

    fn reap_bounded(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let Some(child) = self.child.as_mut() else {
            return Err(io::Error::other("bwrap already reaped"));
        };
        let deadline = Instant::now() + REAP_BOUND;
        loop {
            if let Some(status) = child.try_wait()? {
                self.status = Some(status);
                self.child = None;
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    ErrorKind::TimedOut,
                    "bwrap reap exceeded bound",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(&mut self) -> io::Result<ExitStatus> {
        self.signal_init()?;
        self.poll_init()?;
        self.signal_bwrap()?;
        let status = self.reap_bounded()?;
        self.armed = false;
        Ok(status)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = self.signal_init();
        if self.pidfd.is_some() {
            let _ = self.poll_init();
        }
        let _ = self.signal_bwrap();
        let _ = self.reap_bounded();
        self.armed = false;
    }
}

pub fn run(root: BorrowedFd<'_>, work: BorrowedFd<'_>, payload: Payload) -> Result<RunResult> {
    ensure_host()?;
    ensure_bwrap()?;
    ensure_root_layout(root)?;
    let root_fd = dup_high(root.as_raw_fd()).context("dup root fd")?;
    let work_fd = dup_high(work.as_raw_fd()).context("dup work fd")?;
    let seccomp = seccomp_memfd().context("seccomp memfd")?;
    let (info_r, info_w) = cloexec_pipe().context("info pipe")?;
    let (block_r, block_w) = cloexec_pipe().context("block pipe")?;
    require_high(root_fd.as_raw_fd())?;
    require_high(work_fd.as_raw_fd())?;
    require_high(info_w.as_raw_fd())?;
    require_high(block_r.as_raw_fd())?;
    require_high(seccomp.as_raw_fd())?;

    let mut cmd = Command::new("/usr/bin/bwrap");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    push_fixed_args(
        &mut cmd,
        root_fd.as_raw_fd(),
        work_fd.as_raw_fd(),
        info_w.as_raw_fd(),
        block_r.as_raw_fd(),
        seccomp.as_raw_fd(),
    );
    match &payload {
        Payload::Bash(script) => {
            cmd.arg("/bin/bash")
                .arg("--noprofile")
                .arg("--norc")
                .arg("-c")
                .arg(script);
        }
        Payload::Sh(script) => {
            cmd.arg("/bin/sh").arg("-c").arg(script);
        }
        Payload::Python(script) => {
            cmd.arg("/usr/bin/python3").arg("-I").arg("-c").arg(script);
        }
    }
    install_pre_exec(
        &mut cmd,
        root_fd.as_raw_fd(),
        work_fd.as_raw_fd(),
        info_w.as_raw_fd(),
        block_r.as_raw_fd(),
        seccomp.as_raw_fd(),
    );

    let deadline = Instant::now() + WALL;
    let child = cmd.spawn().context("spawn bwrap")?;
    let mut guard = Guard {
        child: Some(child),
        status: None,
        pidfd: None,
        armed: true,
    };
    let stdout = guard
        .child
        .as_mut()
        .context("bwrap missing after spawn")?
        .stdout
        .take()
        .context("bwrap stdout")?;
    let stderr = guard
        .child
        .as_mut()
        .context("bwrap missing after spawn")?
        .stderr
        .take()
        .context("bwrap stderr")?;
    drop(info_w);
    drop(block_r);
    drop(seccomp);
    drop(root_fd);
    drop(work_fd);

    let mut stdout_rx = spawn_drain(stdout).context("stdout reader")?;
    let mut stderr_rx = spawn_drain(stderr).context("stderr reader")?;

    let pid = read_child_pid(info_r.as_raw_fd(), deadline)?;
    drop(info_r);
    guard.pidfd = Some(pidfd_open(pid).context("pidfd_open")?);
    write_full(block_w.as_raw_fd(), &[0]).context("release block-fd")?;
    drop(block_w);

    let (status, timed_out) = wait_wall(&mut guard, deadline)?;
    let (stdout, out_trunc) = recv_bound(&mut stdout_rx).context("stdout reader")?;
    let (stderr, err_trunc) = recv_bound(&mut stderr_rx).context("stderr reader")?;
    Ok(RunResult {
        exit_code: exit_code_of(status),
        timed_out,
        stdout,
        stderr,
        truncated: out_trunc || err_trunc,
    })
}

fn wait_wall(guard: &mut Guard, deadline: Instant) -> Result<(ExitStatus, bool)> {
    loop {
        if Instant::now() >= deadline {
            let status = guard.finish().context("timeout kill")?;
            return Ok((status, true));
        }
        let child = guard
            .child
            .as_mut()
            .context("bwrap missing while waiting")?;
        if let Some(status) = child.try_wait().context("try_wait bwrap")? {
            guard.status = Some(status);
            guard.child = None;
            let status = guard.finish().context("reap namespace")?;
            return Ok((status, false));
        }
        let slice =
            Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now()));
        if slice.is_zero() {
            continue;
        }
        thread::sleep(slice);
    }
}

fn install_pre_exec(
    cmd: &mut Command,
    root: RawFd,
    work: RawFd,
    info: RawFd,
    block: RawFd,
    seccomp: RawFd,
) {
    let keep = [root, work, info, block, seccomp];
    unsafe {
        cmd.pre_exec(move || {
            deny_new_privs()?;
            scope_landlock()?;
            set_limits()?;
            // close_range marks every fd >= 3 cloexec before exec. Unknown
            // descriptors die at exec. The whitelist is cleared so bwrap
            // still receives --bind-fd, info, block, and seccomp. Do not
            // close them here: Rust's spawn error pipe is also fd >= 3.
            close_from_stdio()?;
            for fd in keep {
                clear_cloexec(fd)?;
            }
            Ok(())
        });
    }
}

fn deny_new_privs() -> io::Result<()> {
    let pr = unsafe {
        libc::prctl(
            libc::PR_SET_NO_NEW_PRIVS,
            1 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if pr != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn close_from_stdio() -> io::Result<()> {
    let rc = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3 as libc::c_long,
            u32::MAX as libc::c_long,
            CLOSE_RANGE_CLOEXEC,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_limits() -> io::Result<()> {
    lim(libc::RLIMIT_CPU, CPU_SECONDS)?;
    lim(libc::RLIMIT_AS, 512 * 1024 * 1024)?;
    lim(libc::RLIMIT_NOFILE, 64)?;
    lim(libc::RLIMIT_CORE, 0)?;
    Ok(())
}

fn lim(resource: libc::__rlimit_resource_t, value: libc::rlim_t) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn clear_cloexec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn ensure_host() -> Result<()> {
    let uid = unsafe { libc::getuid() };
    let euid = unsafe { libc::geteuid() };
    if uid == 0 || euid == 0 {
        bail!("host uid and euid must be non-zero");
    }
    Ok(())
}

fn ensure_bwrap() -> Result<()> {
    let fd = syscall_fd(unsafe {
        libc::open(
            c"/usr/bin/bwrap".as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        ) as libc::c_long
    })
    .context("open /usr/bin/bwrap")?;
    let st = fstat(fd.as_raw_fd()).context("stat /usr/bin/bwrap")?;
    if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
        bail!("bwrap is not a regular file");
    }
    if st.st_mode & libc::S_ISUID != 0 {
        bail!("bwrap must not be setuid");
    }
    let help = std::process::Command::new("/usr/bin/bwrap")
        .arg("--help")
        .env_clear()
        .output()
        .context("bwrap --help")?;
    let text = String::from_utf8_lossy(&help.stdout);
    if !text.contains("--bind-fd") || !text.contains("--overlay") {
        bail!("bwrap has no --bind-fd or --overlay");
    }
    Ok(())
}

fn ensure_root_layout(root: BorrowedFd<'_>) -> Result<()> {
    for name in ROOT_DIRS {
        let fd = openat2_dir(root.as_raw_fd(), name)
            .with_context(|| format!("openat2 {}", name.to_string_lossy()))?;
        let st =
            fstat(fd.as_raw_fd()).with_context(|| format!("fstat {}", name.to_string_lossy()))?;
        if (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
            bail!("{} is not a directory", name.to_string_lossy());
        }
    }
    Ok(())
}

fn openat2_dir(dirfd: RawFd, path: &CStr) -> io::Result<OwnedFd> {
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        mode: 0,
        resolve: RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS | RESOLVE_BENEATH,
    };
    let rc = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd as libc::c_long,
            path.as_ptr() as libc::c_long,
            &how as *const OpenHow as libc::c_long,
            std::mem::size_of::<OpenHow>() as libc::c_long,
        )
    };
    syscall_fd(rc)
}

fn seccomp_memfd() -> Result<OwnedFd> {
    let filters = build_filter()?;
    let fd = syscall_fd(unsafe {
        libc::memfd_create(c"seccomp".as_ptr(), libc::MFD_CLOEXEC) as libc::c_long
    })
    .context("memfd_create")?;
    let bytes = unsafe {
        std::slice::from_raw_parts(
            filters.as_ptr() as *const u8,
            filters.len() * std::mem::size_of::<libc::sock_filter>(),
        )
    };
    write_full(fd.as_raw_fd(), bytes)?;
    if unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_SET) } < 0 {
        return Err(io::Error::last_os_error()).context("seccomp rewind");
    }
    Ok(fd)
}

fn build_filter() -> Result<Vec<libc::sock_filter>> {
    let arch = audit_arch()?;
    let mut prog = vec![
        stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, off_arch()),
        jump(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, arch, 1, 0),
        stmt(libc::BPF_RET | libc::BPF_K, errno_eperm()),
        stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, off_nr()),
    ];
    let mut to_eperm = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        to_eperm.push(prog.len());
        prog.push(jump(
            libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K,
            X32_SYSCALL_BIT,
            0,
            0,
        ));
    }
    for nr in blocked_syscalls() {
        to_eperm.push(prog.len());
        prog.push(jump(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, nr, 0, 0));
    }
    for nr in [libc::SYS_unshare, libc::SYS_setns] {
        to_eperm.push(prog.len());
        prog.push(jump(
            libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
            nr as u32,
            0,
            0,
        ));
    }
    let clone3_at = prog.len();
    prog.push(jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        libc::SYS_clone3 as u32,
        0,
        0,
    ));
    let clone_at = prog.len();
    prog.push(jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        libc::SYS_clone as u32,
        0,
        0,
    ));
    prog.push(stmt(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_ALLOW));
    let flags_at = prog.len();
    prog.push(stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, off_arg0()));
    let mask_at = prog.len();
    prog.push(jump(
        libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K,
        namespace_clone_mask(),
        0,
        0,
    ));
    prog.push(stmt(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_ALLOW));
    let eperm_at = prog.len();
    prog.push(stmt(libc::BPF_RET | libc::BPF_K, errno_eperm()));
    let enosys_at = prog.len();
    prog.push(stmt(
        libc::BPF_RET | libc::BPF_K,
        libc::SECCOMP_RET_ERRNO | (libc::ENOSYS as u32),
    ));
    for i in to_eperm {
        prog[i].jt = u8::try_from(eperm_at - (i + 1)).context("seccomp jump")?;
    }
    prog[clone3_at].jt = u8::try_from(enosys_at - (clone3_at + 1)).context("seccomp jump")?;
    prog[clone_at].jt = u8::try_from(flags_at - (clone_at + 1)).context("seccomp jump")?;
    prog[mask_at].jt = u8::try_from(eperm_at - (mask_at + 1)).context("seccomp jump")?;
    Ok(prog)
}

fn namespace_clone_mask() -> u32 {
    (libc::CLONE_NEWNS
        | libc::CLONE_NEWUTS
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWUSER
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWNET
        | libc::CLONE_NEWCGROUP
        | libc::CLONE_NEWTIME) as u32
}

fn off_arg0() -> u32 {
    std::mem::offset_of!(libc::seccomp_data, args) as u32
}

#[cfg(target_arch = "x86_64")]
fn audit_arch() -> Result<u32> {
    Ok(AUDIT_ARCH_X86_64)
}

#[cfg(target_arch = "aarch64")]
fn audit_arch() -> Result<u32> {
    Ok(AUDIT_ARCH_AARCH64)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn audit_arch() -> Result<u32> {
    bail!("unsupported audit arch");
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn blocked_syscalls() -> [u32; 6] {
    [
        libc::SYS_keyctl as u32,
        libc::SYS_add_key as u32,
        libc::SYS_request_key as u32,
        libc::SYS_ptrace as u32,
        libc::SYS_process_vm_readv as u32,
        libc::SYS_process_vm_writev as u32,
    ]
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn blocked_syscalls() -> [u32; 0] {
    []
}

fn off_nr() -> u32 {
    std::mem::offset_of!(libc::seccomp_data, nr) as u32
}

fn off_arch() -> u32 {
    std::mem::offset_of!(libc::seccomp_data, arch) as u32
}

fn errno_eperm() -> u32 {
    libc::SECCOMP_RET_ERRNO | (libc::EPERM as u32)
}

fn stmt(code: u32, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u32, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    }
}

fn read_child_pid(fd: RawFd, deadline: Instant) -> Result<i32> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 512];
    loop {
        if let Some(pid) = parse_child_pid(&buf, false) {
            return Ok(pid);
        }
        let now = Instant::now();
        if now >= deadline {
            bail!("info fd deadline");
        }
        let wait_ms =
            i32::try_from(deadline.saturating_duration_since(now).as_millis()).unwrap_or(i32::MAX);
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err).context("poll info fd");
        }
        if rc == 0 {
            bail!("info fd deadline");
        }
        let n = unsafe { libc::read(fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err).context("read info fd");
        }
        if n == 0 {
            if let Some(pid) = parse_child_pid(&buf, true) {
                return Ok(pid);
            }
            bail!("info fd closed before child-pid");
        }
        buf.extend_from_slice(&tmp[..n as usize]);
        if buf.len() > INFO_CAP {
            bail!("info fd oversized");
        }
    }
}

fn parse_child_pid(buf: &[u8], eof: bool) -> Option<i32> {
    if buf.is_empty() {
        let _ = eof;
        return None;
    }
    let mut de = serde_json::Deserializer::from_slice(buf);
    let pid = ChildPid::deserialize(&mut de).ok()?;
    de.end().ok()?;
    Some(pid.0)
}

struct ChildPid(i32);

impl<'de> Deserialize<'de> for ChildPid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PidVisitor;

        impl<'de> Visitor<'de> for PidVisitor {
            type Value = ChildPid;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("info object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut pid = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "child-pid" {
                        if pid.is_some() {
                            return Err(de::Error::custom("duplicate child-pid"));
                        }
                        let value = map.next_value::<serde_json::Value>()?;
                        pid = Some(child_pid_number(&value)?);
                    } else {
                        let _skip = map.next_value::<serde_json::Value>()?;
                    }
                }
                pid.map(ChildPid)
                    .ok_or_else(|| de::Error::missing_field("child-pid"))
            }
        }

        deserializer.deserialize_map(PidVisitor)
    }
}

fn child_pid_number<E: de::Error>(value: &serde_json::Value) -> Result<i32, E> {
    let number = value
        .as_number()
        .ok_or_else(|| E::custom("child-pid type"))?;
    if !(number.is_i64() || number.is_u64()) {
        return Err(E::custom("child-pid type"));
    }
    let pid = number
        .as_i64()
        .ok_or_else(|| E::custom("child-pid range"))?;
    if pid <= 0 || pid > i32::MAX as i64 {
        return Err(E::custom("child-pid range"));
    }
    Ok(pid as i32)
}

fn pidfd_open(pid: i32) -> io::Result<OwnedFd> {
    let rc = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_long, 0 as libc::c_long) };
    syscall_fd(rc)
}

fn exit_code_of(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    status.signal().map(|sig| 128 + sig).unwrap_or(-1)
}

fn cloexec_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((read, write))
}

fn dup_high(fd: RawFd) -> io::Result<OwnedFd> {
    syscall_fd(unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) } as libc::c_long)
}

fn require_high(fd: RawFd) -> Result<()> {
    if fd >= 3 {
        Ok(())
    } else {
        bail!("sandbox fd collides with stdio");
    }
}

fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

fn write_full(fd: RawFd, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let n = unsafe { libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(io::Error::new(ErrorKind::WriteZero, "short write"));
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

fn syscall_fd(rc: libc::c_long) -> io::Result<OwnedFd> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(rc as RawFd) })
    }
}

#[cfg(test)]
#[path = "workspace_runner_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "workspace_runner_lifecycle_tests.rs"]
mod lifecycle_tests;
