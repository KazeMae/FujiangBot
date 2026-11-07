use std::ffi::{CStr, CString};
use std::fs;
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::time::{Duration, Instant};

const SUB_START: u32 = 100000;
const SUB_COUNT: u32 = 65536;
const HELPER_MODE: u32 = 0o4755;
const NEWUIDMAP: &CStr = c"/usr/bin/newuidmap";
const NEWGIDMAP: &CStr = c"/usr/bin/newgidmap";

/// Full map for apt inside the sandbox. Container root is the nonzero host
/// euid/egid. The next 65536 ids are the fixed subordinate range.
pub(super) fn bootstrap() -> io::Result<OwnedFd> {
    if unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 0 {
        return Err(invalid("nnp"));
    }
    if unsafe { libc::geteuid() } == 0 || unsafe { libc::getegid() } == 0 {
        return Err(invalid("host id is zero"));
    }
    let user = current_user()?;
    require_subid_file("/etc/subuid", &user)?;
    require_subid_file("/etc/subgid", &user)?;
    let uid_helper = pin_helper(NEWUIDMAP)?;
    let gid_helper = pin_helper(NEWGIDMAP)?;
    if !path_is_same(NEWUIDMAP, uid_helper.as_raw_fd())?
        || !path_is_same(NEWGIDMAP, gid_helper.as_raw_fd())?
    {
        return Err(invalid("helper replaced"));
    }
    let mut holder = fork_holder()?;
    let proc_dir = pin_proc_dir(holder.pidfd.as_raw_fd(), holder.pid)?;
    let spec = format!("fd:{}", proc_dir.as_raw_fd());
    let euid = unsafe { libc::geteuid() }.to_string();
    let egid = unsafe { libc::getegid() }.to_string();
    let uid_args = ["0", euid.as_str(), "1", "1", "100000", "65536"];
    let gid_args = ["0", egid.as_str(), "1", "1", "100000", "65536"];
    let mapped = (|| {
        run_pinned(
            &gid_helper,
            Some(proc_dir.as_raw_fd()),
            "newgidmap",
            &spec,
            &gid_args,
            Duration::from_secs(2),
            true,
        )?;
        run_pinned(
            &uid_helper,
            Some(proc_dir.as_raw_fd()),
            "newuidmap",
            &spec,
            &uid_args,
            Duration::from_secs(2),
            true,
        )?;
        if !maps_from_dir(proc_dir.as_raw_fd())? {
            return Err(invalid("kernel map mismatch"));
        }
        let ns = open_verified_userns(proc_dir.as_raw_fd())?;
        holder.finish()?;
        require_user_ns(ns.as_raw_fd())?;
        require_owner(ns.as_raw_fd())?;
        Ok(ns)
    })();
    if mapped.is_err() {
        holder.kill();
    }
    mapped
}

fn require_subid_file(path: &str, user: &str) -> io::Result<()> {
    let text = read_bounded(Path::new(path), 1 << 20)?;
    parse_subid(&text, user)
}

fn parse_subid(text: &str, user: &str) -> io::Result<()> {
    if user.is_empty() || user.contains(':') {
        return Err(invalid("user"));
    }
    let mut found = false;
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split(':');
        let name = parts.next().unwrap_or("");
        let start = parts.next();
        let count = parts.next();
        if name != user {
            continue;
        }
        if found || parts.next().is_some() {
            return Err(invalid("subid"));
        }
        if start != Some("100000") || count != Some("65536") {
            return Err(invalid("subid allocation"));
        }
        found = true;
    }
    if found {
        Ok(())
    } else {
        Err(invalid("subid"))
    }
}

fn pin_helper(path: &CStr) -> io::Result<OwnedFd> {
    let opened = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if opened < 0 {
        return Err(invalid("helper"));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(opened) };
    let st = fstat(fd.as_raw_fd())?;
    if !secure_helper(&st) || !path_is_same(path, fd.as_raw_fd())? {
        return Err(invalid("helper"));
    }
    Ok(fd)
}

fn secure_helper(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFREG
        && st.st_uid == 0
        && st.st_gid == 0
        && st.st_nlink == 1
        && st.st_mode & 0o7777 == HELPER_MODE
}

fn path_is_same(path: &CStr, fd: RawFd) -> io::Result<bool> {
    let mut named: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            path.as_ptr(),
            &mut named,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let opened = fstat(fd)?;
    Ok(named.st_dev == opened.st_dev && named.st_ino == opened.st_ino)
}

struct Holder {
    pid: i32,
    pidfd: OwnedFd,
    release: OwnedFd,
    alive: bool,
}

impl Holder {
    fn finish(&mut self) -> io::Result<()> {
        if !self.alive {
            return Ok(());
        }
        let byte = [0u8; 1];
        let _ = unsafe {
            libc::write(
                self.release.as_raw_fd(),
                byte.as_ptr() as *const libc::c_void,
                1,
            )
        };
        let status = wait_timeout(self.pid, Duration::from_secs(2), false)?;
        self.alive = false;
        if status == 0 {
            Ok(())
        } else {
            Err(invalid("userns holder"))
        }
    }

    fn kill(&mut self) {
        if !self.alive {
            return;
        }
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd() as libc::c_long,
                libc::SIGKILL as libc::c_long,
                0 as libc::c_long,
                0 as libc::c_long,
            )
        };
        let _ = wait_timeout(self.pid, Duration::from_secs(2), false);
        self.alive = false;
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Close every FD >= 3 except one kept FD, or two when the arguments differ.
/// Flags 0 actually close; CLOEXEC would not.
fn close_except(keep_a: RawFd, keep_b: RawFd) -> bool {
    if keep_a < 3 || keep_b < 3 {
        return false;
    }
    let low = u32::try_from(keep_a.min(keep_b)).unwrap_or(u32::MAX);
    let high = u32::try_from(keep_a.max(keep_b)).unwrap_or(0);
    if low > high {
        return false;
    }
    let above = match high.checked_add(1) {
        Some(first) => close_span(first, u32::MAX),
        None => true,
    };
    close_span(3, low.saturating_sub(1))
        && close_span(low.saturating_add(1), high.saturating_sub(1))
        && above
}

fn close_span(first: u32, last: u32) -> bool {
    if first > last {
        return true;
    }
    unsafe {
        libc::syscall(
            libc::SYS_close_range,
            first as libc::c_long,
            last as libc::c_long,
            0 as libc::c_long,
        ) == 0
    }
}

fn fork_holder() -> io::Result<Holder> {
    let mut ready = [-1; 2];
    let mut release = [-1; 2];
    if unsafe { libc::pipe2(ready.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::pipe2(release.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        unsafe {
            libc::close(ready[0]);
            libc::close(ready[1]);
        }
        return Err(io::Error::last_os_error());
    }
    let expect_ppid = unsafe { libc::getpid() };
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        for fd in [ready[0], ready[1], release[0], release[1]] {
            unsafe { libc::close(fd) };
        }
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        unsafe {
            if !close_except(ready[1], release[0]) {
                libc::_exit(70);
            }
            libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0,
                0,
                0,
            );
            if libc::getppid() != expect_ppid {
                libc::_exit(71);
            }
            if libc::unshare(libc::CLONE_NEWUSER) != 0 {
                libc::_exit(72);
            }
            let one = [1u8; 1];
            if libc::write(ready[1], one.as_ptr() as *const libc::c_void, 1) != 1 {
                libc::_exit(75);
            }
            let mut got = [0u8; 1];
            loop {
                let n = libc::read(release[0], got.as_mut_ptr() as *mut libc::c_void, 1);
                if n == 1 {
                    break;
                }
                if n < 0 && *libc::__errno_location() == libc::EINTR {
                    continue;
                }
                libc::_exit(76);
            }
            libc::_exit(0);
        }
    }
    unsafe {
        libc::close(ready[1]);
        libc::close(release[0]);
    }
    let ready_r = unsafe { OwnedFd::from_raw_fd(ready[0]) };
    let release_w = unsafe { OwnedFd::from_raw_fd(release[1]) };
    let pidfd = match pidfd_open(pid) {
        Ok(fd) => fd,
        Err(err) => {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let _ = wait_timeout(pid, Duration::from_secs(2), false);
            return Err(err);
        }
    };
    let mut holder = Holder {
        pid,
        pidfd,
        release: release_w,
        alive: true,
    };
    if !wait_byte(ready_r.as_raw_fd(), Duration::from_secs(2))? {
        let status = match wait_timeout(pid, Duration::from_secs(2), false) {
            Ok(code) => format!("exit {code}"),
            Err(err) => err.to_string(),
        };
        holder.alive = false;
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("userns holder {status}"),
        ));
    }
    if fdinfo_pid(holder.pidfd.as_raw_fd())? != pid {
        holder.kill();
        return Err(invalid("pidfd"));
    }
    Ok(holder)
}

fn run_pinned(
    fd: &OwnedFd,
    proc_dir: Option<RawFd>,
    argv0: &str,
    pid: &str,
    args: &[&str],
    limit: Duration,
    group: bool,
) -> io::Result<()> {
    let mut texts = vec![CString::new(argv0).map_err(|_| invalid("helper"))?];
    texts.push(CString::new(pid).map_err(|_| invalid("helper"))?);
    for arg in args {
        texts.push(CString::new(*arg).map_err(|_| invalid("helper"))?);
    }
    let mut argv: Vec<*const libc::c_char> = texts.iter().map(|s| s.as_ptr()).collect();
    argv.push(std::ptr::null());
    let env = CString::new("PATH=/usr/bin:/bin").map_err(|_| invalid("helper"))?;
    let envp = [env.as_ptr(), std::ptr::null()];
    let expect_ppid = unsafe { libc::getpid() };
    let child = unsafe { libc::fork() };
    if child < 0 {
        return Err(io::Error::last_os_error());
    }
    if child == 0 {
        unsafe {
            libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0,
                0,
                0,
            );
            if libc::getppid() != expect_ppid || libc::setpgid(0, 0) != 0 {
                libc::_exit(81);
            }
            let helper_fd = fd.as_raw_fd();
            let keep_b = proc_dir.unwrap_or(helper_fd);
            if !close_except(helper_fd, keep_b) {
                libc::_exit(82);
            }
            if let Some(dir) = proc_dir {
                let flags = libc::fcntl(dir, libc::F_GETFD);
                if flags < 0 || libc::fcntl(dir, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    libc::_exit(83);
                }
            }
            if libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 0 {
                libc::_exit(84);
            }
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
            if null < 0
                || libc::dup2(null, 0) < 0
                || libc::dup2(null, 1) < 0
                || libc::dup2(null, 2) < 0
            {
                libc::_exit(85);
            }
            if null > 2 {
                libc::close(null);
            }
            libc::fexecve(fd.as_raw_fd(), argv.as_ptr(), envp.as_ptr());
            libc::_exit(127);
        }
    }
    let status = wait_timeout(child, limit, group)?;
    if status == 0 {
        Ok(())
    } else {
        Err(invalid("helper"))
    }
}

fn maps_from_dir(dir: RawFd) -> io::Result<bool> {
    let uid = read_at(dir, c"uid_map")?;
    let gid = read_at(dir, c"gid_map")?;
    let groups = read_at(dir, c"setgroups")?;
    Ok(maps_match(
        &uid,
        &gid,
        &groups,
        unsafe { libc::geteuid() },
        unsafe { libc::getegid() },
    ))
}

fn maps_match(uid: &str, gid: &str, setgroups: &str, euid: u32, egid: u32) -> bool {
    extents_are(uid, euid) && extents_are(gid, egid) && setgroups == "allow\n"
}

fn extents_are(text: &str, host_root: u32) -> bool {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            continue;
        }
        let mut cols = line.split_whitespace();
        let Some(inside) = cols.next().and_then(|v| v.parse::<u32>().ok()) else {
            return false;
        };
        let Some(outside) = cols.next().and_then(|v| v.parse::<u32>().ok()) else {
            return false;
        };
        let Some(len) = cols.next().and_then(|v| v.parse::<u32>().ok()) else {
            return false;
        };
        if cols.next().is_some() {
            return false;
        }
        rows.push((inside, outside, len));
    }
    rows == [(0, host_root, 1), (1, SUB_START, SUB_COUNT)]
}

fn pin_proc_dir(pidfd: RawFd, pid: i32) -> io::Result<OwnedFd> {
    if fdinfo_pid(pidfd)? != pid {
        return Err(invalid("stale pid"));
    }
    let path = CString::new(format!("/proc/{pid}")).map_err(|_| invalid("proc"))?;
    let fd = raw_owned(unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        ) as libc::c_long
    })?;
    if fdinfo_pid(pidfd)? != pid {
        return Err(invalid("stale pid"));
    }
    let status = read_at(fd.as_raw_fd(), c"status")?;
    let needle = format!("Pid:\t{pid}");
    if !status.lines().any(|line| line == needle) {
        return Err(invalid("stale pid"));
    }
    Ok(fd)
}

fn open_verified_userns(dir: RawFd) -> io::Result<OwnedFd> {
    let fd = open_user_from_proc(dir)?;
    require_user_ns(fd.as_raw_fd())?;
    require_owner(fd.as_raw_fd())?;
    let parent = ns_parent(fd.as_raw_fd())?;
    let mine = raw_owned(unsafe {
        libc::open(
            c"/proc/self/ns/user".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        ) as libc::c_long
    })?;
    let parent_st = fstat(parent.as_raw_fd())?;
    let mine_st = fstat(mine.as_raw_fd())?;
    if parent_st.st_dev != mine_st.st_dev || parent_st.st_ino != mine_st.st_ino {
        return Err(invalid("userns parent"));
    }
    if !maps_from_dir(dir)? {
        return Err(invalid("kernel map mismatch"));
    }
    Ok(fd)
}

fn open_user_from_proc(dir: RawFd) -> io::Result<OwnedFd> {
    raw_owned(unsafe {
        libc::openat(dir, c"ns/user".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) as libc::c_long
    })
}

fn read_at(dir: RawFd, name: &CStr) -> io::Result<String> {
    let fd = raw_owned(unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        ) as libc::c_long
    })?;
    read_fd(fd.as_raw_fd(), 4096)
}

fn require_owner(fd: RawFd) -> io::Result<()> {
    let mut uid: libc::uid_t = 0;
    let rc = unsafe { libc::ioctl(fd, libc::NS_GET_OWNER_UID, &mut uid) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if uid != unsafe { libc::geteuid() } {
        return Err(invalid("ns owner"));
    }
    Ok(())
}

fn require_user_ns(fd: RawFd) -> io::Result<()> {
    let kind = unsafe { libc::ioctl(fd, libc::NS_GET_NSTYPE) };
    if kind < 0 {
        return Err(io::Error::last_os_error());
    }
    if kind as libc::c_int != libc::CLONE_NEWUSER {
        return Err(invalid("ns type"));
    }
    Ok(())
}

fn ns_parent(fd: RawFd) -> io::Result<OwnedFd> {
    let parent = unsafe { libc::ioctl(fd, libc::NS_GET_PARENT) };
    raw_owned(parent as libc::c_long)
}

fn read_fd(fd: RawFd, cap: usize) -> io::Result<String> {
    let mut buf = vec![0u8; cap + 1];
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if n as usize > cap || buf[..n as usize].contains(&0) {
        return Err(invalid("proc"));
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).map_err(|_| invalid("proc"))
}

fn read_bounded(path: &Path, cap: usize) -> io::Result<String> {
    let bytes = fs::read(path)?;
    if bytes.len() > cap || bytes.contains(&0) {
        return Err(invalid("proc"));
    }
    String::from_utf8(bytes).map_err(|_| invalid("proc"))
}

fn fdinfo_pid(pidfd: RawFd) -> io::Result<i32> {
    let path = format!("/proc/self/fdinfo/{pidfd}");
    let text = read_bounded(Path::new(&path), 4096)?;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("Pid:") else {
            continue;
        };
        return rest.trim().parse::<i32>().map_err(|_| invalid("pidfd"));
    }
    Err(invalid("pidfd"))
}

fn pidfd_open(pid: i32) -> io::Result<OwnedFd> {
    raw_owned(unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_long, 0) })
}

fn wait_byte(fd: RawFd, limit: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + limit;
    let mut one = [0u8; 1];
    while Instant::now() < deadline {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, 50) };
        if rc > 0 {
            let n = unsafe { libc::read(fd, one.as_mut_ptr() as *mut libc::c_void, 1) };
            return Ok(n == 1);
        }
        if rc < 0 && io::Error::last_os_error().kind() != ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(false)
}

fn wait_timeout(pid: i32, limit: Duration, group: bool) -> io::Result<i32> {
    let deadline = Instant::now() + limit;
    loop {
        let mut status = 0;
        let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if rc == pid {
            return Ok(libc_status(status));
        }
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if Instant::now() >= deadline {
            if group {
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let reap_end = Instant::now() + Duration::from_secs(2);
            loop {
                let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                let err = io::Error::last_os_error().raw_os_error();
                if rc == pid || (rc < 0 && err == Some(libc::ECHILD)) {
                    return Err(invalid("helper timeout"));
                }
                if rc < 0 && err == Some(libc::EINTR) {
                    continue;
                }
                if rc < 0 || Instant::now() >= reap_end {
                    return Err(invalid("helper timeout"));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn libc_status(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        128
    }
}

fn current_user() -> io::Result<String> {
    let mut buf = vec![0u8; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            &mut pwd,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return Err(invalid("user"));
    }
    let name = unsafe { CStr::from_ptr((*result).pw_name) };
    let text = name.to_string_lossy();
    if text.as_bytes().iter().any(|b| *b == 0 || *b == b'\n') {
        return Err(invalid("user"));
    }
    Ok(text.into_owned())
}

fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

fn raw_owned(fd: libc::c_long) -> io::Result<OwnedFd> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn subid_parser_requires_the_fixed_range_once() {
        parse_subid("box:100000:65536\n", "box").unwrap();
        parse_subid("# c\nother:1:1\nbox:100000:65536\n", "box").unwrap();
        assert!(parse_subid("box:100000:65535\n", "box").is_err());
        assert!(parse_subid("box:100000:65536\nbox:100000:65536\n", "box").is_err());
        assert!(parse_subid("box:100000:65536:extra\n", "box").is_err());
        assert!(parse_subid("other:100000:65536\n", "box").is_err());
    }

    #[test]
    fn full_map_rejects_single_uid_and_foreign_root() {
        let full_uid = "         0       1000          1\n         1     100000      65536\n";
        let full_gid = "         0       1000          1\n         1     100000      65536\n";
        assert!(maps_match(full_uid, full_gid, "allow\n", 1000, 1000));
        assert!(!maps_match("0 1000 1\n", full_gid, "allow\n", 1000, 1000));
        assert!(!maps_match(full_uid, full_gid, "deny\n", 1000, 1000));
        assert!(!maps_match(full_uid, full_gid, "allow\n", 1001, 1000));
        assert!(!maps_match(
            "0 0 1\n1 100000 65536\n",
            full_gid,
            "allow\n",
            1000,
            1000
        ));
    }

    #[test]
    fn box_allocation_matches_when_current_user_is_box() {
        let user = current_user().unwrap();
        if user == "box" {
            require_subid_file("/etc/subuid", "box").unwrap();
            require_subid_file("/etc/subgid", "box").unwrap();
        }
    }

    #[test]
    fn non_root_helper_and_replacement_fail_closed() {
        let dir = std::env::temp_dir().join(format!("userns-helper-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("newuidmap");
        fs::write(&path, b"#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert!(pin_helper(&c_path).is_err());
        let fd = raw_owned(unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            ) as libc::c_long
        })
        .unwrap();
        assert!(path_is_same(&c_path, fd.as_raw_fd()).unwrap());
        let other = dir.join("other");
        fs::write(&other, b"replaced").unwrap();
        std::fs::rename(&other, &path).unwrap();
        assert!(!path_is_same(&c_path, fd.as_raw_fd()).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn helper_timeout_kills_the_process_group() {
        let fd = open_public_tool(c"/bin/sleep");
        let started = Instant::now();
        let err = run_pinned(
            &fd,
            None,
            "sleep",
            "30",
            &[],
            Duration::from_millis(400),
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("timeout"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn helper_failure_is_reaped() {
        let fd = open_public_tool(c"/bin/false");
        let err =
            run_pinned(&fd, None, "false", "1", &[], Duration::from_secs(2), true).unwrap_err();
        assert!(err.to_string().contains("helper"), "{err}");
    }

    #[test]
    fn production_holder_leaves_setgroups_allow_and_drop_reaps() {
        let holder = fork_holder().unwrap();
        let pid = holder.pid;
        let dir = pin_proc_dir(holder.pidfd.as_raw_fd(), pid).unwrap();
        assert_eq!(read_at(dir.as_raw_fd(), c"setgroups").unwrap(), "allow\n");
        assert!(!maps_from_dir(dir.as_raw_fd()).unwrap());
        assert!(pin_proc_dir(holder.pidfd.as_raw_fd(), pid.wrapping_add(1)).is_err());
        drop(holder);
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    #[test]
    fn kernel_single_map_is_not_the_full_map_and_ns_fd_is_checked() {
        let mut holder = fork_holder().expect("unshare");
        let dir = pin_proc_dir(holder.pidfd.as_raw_fd(), holder.pid).unwrap();
        let euid = unsafe { libc::geteuid() };
        let egid = unsafe { libc::getegid() };
        write_at(dir.as_raw_fd(), c"setgroups", "deny\n").unwrap();
        write_at(dir.as_raw_fd(), c"uid_map", &format!("0 {euid} 1\n")).unwrap();
        write_at(dir.as_raw_fd(), c"gid_map", &format!("0 {egid} 1\n")).unwrap();
        assert_eq!(read_at(dir.as_raw_fd(), c"setgroups").unwrap(), "deny\n");
        assert!(!maps_from_dir(dir.as_raw_fd()).unwrap());
        let uid = read_at(dir.as_raw_fd(), c"uid_map").unwrap();
        assert!(uid.split_whitespace().any(|col| col == "1"));
        assert!(!uid.contains("65536"));
        let ns = open_user_from_proc(dir.as_raw_fd()).unwrap();
        require_user_ns(ns.as_raw_fd()).unwrap();
        require_owner(ns.as_raw_fd()).unwrap();
        let parent = ns_parent(ns.as_raw_fd()).unwrap();
        let mine = raw_owned(unsafe {
            libc::open(
                c"/proc/self/ns/user".as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            ) as libc::c_long
        })
        .unwrap();
        assert_eq!(
            fstat(parent.as_raw_fd()).unwrap().st_ino,
            fstat(mine.as_raw_fd()).unwrap().st_ino
        );
        let mut pipes = [0; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipes.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let pipe_fd = unsafe { OwnedFd::from_raw_fd(pipes[0]) };
        unsafe { libc::close(pipes[1]) };
        assert!(require_user_ns(pipe_fd.as_raw_fd()).is_err());
        assert!(read_at(pipe_fd.as_raw_fd(), c"uid_map").is_err());
        let pidns = raw_owned(unsafe {
            libc::open(
                c"/proc/self/ns/pid".as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            ) as libc::c_long
        })
        .unwrap();
        assert!(require_user_ns(pidns.as_raw_fd()).is_err());
        holder.finish().unwrap();
    }

    #[test]
    fn holder_releases_unrelated_flock_while_still_alive() {
        if std::env::var_os("FUJIANG_FLOCK_ISOLATED").is_none() {
            let current = std::thread::current();
            let name = current.name().expect("test name");
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--test-threads=1"])
                .env("FUJIANG_FLOCK_ISOLATED", "1")
                .status()
                .unwrap();
            assert!(status.success(), "{name}");
            return;
        }
        let path = std::env::temp_dir().join(format!("userns-flock-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let held = raw_owned(unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC,
                0o600,
            ) as libc::c_long
        })
        .unwrap();
        assert_eq!(
            unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let holder = fork_holder().unwrap();
        drop(held);
        let again = raw_owned(unsafe {
            libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) as libc::c_long
        })
        .unwrap();
        let rc = unsafe { libc::flock(again.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "{}", io::Error::last_os_error());
        assert_eq!(unsafe { libc::kill(holder.pid, 0) }, 0);
        drop(holder);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn timeout_reap_belongs_only_to_wait_timeout() {
        let mut ready = [0; 2];
        assert_eq!(
            unsafe { libc::pipe2(ready.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                libc::close(ready[0]);
                if libc::setpgid(0, 0) != 0 || !close_except(ready[1], ready[1]) {
                    libc::_exit(81);
                }
                let one = [1u8];
                if libc::write(ready[1], one.as_ptr() as *const libc::c_void, 1) != 1 {
                    libc::_exit(82);
                }
                loop {
                    libc::pause();
                }
            }
        }
        unsafe { libc::close(ready[1]) };
        let mut saw = [0u8; 1];
        let n = unsafe { libc::read(ready[0], saw.as_mut_ptr() as *mut libc::c_void, 1) };
        if n != 1 {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let _ = wait_timeout(pid, Duration::from_secs(2), false);
            assert_eq!(n, 1);
        }
        unsafe { libc::close(ready[0]) };
        let err = wait_timeout(pid, Duration::from_millis(40), true).unwrap_err();
        assert!(err.to_string().contains("timeout"), "{err}");
        let rc = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
        assert_eq!(rc, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn bootstrap_records_a_real_namespace_or_the_closed_error() {
        match bootstrap() {
            Err(err) => {
                let text = err.to_string();
                assert!(
                    text.contains("helper")
                        || text.contains("subid")
                        || text.contains("map")
                        || text.contains("nnp"),
                    "{text}"
                );
            }
            Ok(fd) => {
                require_user_ns(fd.as_raw_fd()).unwrap();
                require_owner(fd.as_raw_fd()).unwrap();
                let mine = raw_owned(unsafe {
                    libc::open(
                        c"/proc/self/ns/user".as_ptr(),
                        libc::O_RDONLY | libc::O_CLOEXEC,
                    ) as libc::c_long
                })
                .unwrap();
                let got = fstat(fd.as_raw_fd()).unwrap();
                let host = fstat(mine.as_raw_fd()).unwrap();
                assert_ne!(got.st_ino, host.st_ino);
                eprintln!("bootstrap returned a verified user namespace");
            }
        }
    }

    fn open_public_tool(path: &CStr) -> OwnedFd {
        let fd = raw_owned(unsafe {
            libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) as libc::c_long
        })
        .unwrap();
        let st = fstat(fd.as_raw_fd()).unwrap();
        assert_eq!(st.st_mode & libc::S_IFMT, libc::S_IFREG);
        fd
    }

    fn write_at(dir: RawFd, name: &CStr, data: &str) -> io::Result<()> {
        let fd = raw_owned(unsafe {
            libc::openat(
                dir,
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            ) as libc::c_long
        })?;
        let n = unsafe {
            libc::write(
                fd.as_raw_fd(),
                data.as_ptr() as *const libc::c_void,
                data.len(),
            )
        };
        if n as usize != data.len() {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
