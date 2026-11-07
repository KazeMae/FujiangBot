//! Copy the current host resolver into the owner root before each run.
//! The only host path is the fixed `/etc/resolv.conf` walk. No fallback address is added.

use std::ffi::{CStr, CString};
use std::io::{self, ErrorKind};
use std::net::IpAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_BYTES: usize = 4096;
const MAX_NS: usize = 3;
const MAX_SEARCH: usize = 6;
const OPEN_DIR: i32 =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
const OPEN_FILE: i32 = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;

static STAGE_SEQ: AtomicU64 = AtomicU64::new(1);

fn unavailable(what: &str) -> io::Error {
    io::Error::new(
        ErrorKind::InvalidData,
        format!("unavailable: resolver {what}"),
    )
}

/// `WorkspaceStore::open` already holds `OwnerLock` for the whole store, including `run`.
/// This function does not lock again. Each stage also takes `flock` on its own marker so
/// crash orphans can be reclaimed here, while a live or inherited descriptor is skipped.
pub(super) fn install(root: RawFd) -> io::Result<()> {
    let text = read_host()?;
    let body = render(&text)?;
    write_guest(root, body.as_bytes())
}

fn read_host() -> io::Result<String> {
    let slash = open_slash()?;
    read_under(slash.as_raw_fd(), 0)
}

fn open_slash() -> io::Result<OwnedFd> {
    open_at(libc::AT_FDCWD, c"/", OPEN_DIR)
}

fn read_under(root: RawFd, uid: u32) -> io::Result<String> {
    let etc = walk_dir(root, &[c"etc"], uid)?;
    match open_at(etc.as_raw_fd(), c"resolv.conf", OPEN_FILE) {
        Ok(fd) => read_stable(fd.as_raw_fd(), uid),
        Err(err) if err.raw_os_error() == Some(libc::ELOOP) => {
            let link = read_link(etc.as_raw_fd(), c"resolv.conf", uid)?;
            let parts = canonical(link.as_str()).ok_or_else(|| unavailable("symlink"))?;
            let fd = walk_file(root, parts, uid)?;
            read_stable(fd.as_raw_fd(), uid)
        }
        Err(err) => Err(err),
    }
}

fn canonical(link: &str) -> Option<&'static [&'static CStr]> {
    match link {
        "/run/systemd/resolve/stub-resolv.conf" | "../run/systemd/resolve/stub-resolv.conf" => {
            Some(&[c"run", c"systemd", c"resolve", c"stub-resolv.conf"])
        }
        "/run/systemd/resolve/resolv.conf" | "../run/systemd/resolve/resolv.conf" => {
            Some(&[c"run", c"systemd", c"resolve", c"resolv.conf"])
        }
        _ => None,
    }
}

fn walk_dir(root: RawFd, parts: &[&CStr], uid: u32) -> io::Result<OwnedFd> {
    let mut current = root;
    let mut held: Option<OwnedFd> = None;
    for name in parts {
        let next = open_at(current, name, OPEN_DIR)?;
        if !dir_ok(next.as_raw_fd(), uid)? {
            return Err(unavailable("parent"));
        }
        current = next.as_raw_fd();
        held = Some(next);
    }
    held.ok_or_else(|| unavailable("parent"))
}

fn walk_file(root: RawFd, parts: &[&CStr], uid: u32) -> io::Result<OwnedFd> {
    let parent = walk_dir(root, &parts[..parts.len() - 1], uid)?;
    let fd = open_at(parent.as_raw_fd(), parts[parts.len() - 1], OPEN_FILE)?;
    Ok(fd)
}

fn open_at(dir: RawFd, name: &CStr, flags: i32) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn dir_ok(fd: RawFd, uid: u32) -> io::Result<bool> {
    let st = fstat(fd)?;
    Ok(st.st_mode & libc::S_IFMT == libc::S_IFDIR && st.st_uid == uid && st.st_mode & 0o022 == 0)
}

fn read_link(dir: RawFd, name: &CStr, uid: u32) -> io::Result<String> {
    let before = fstat_at(dir, name, libc::AT_SYMLINK_NOFOLLOW)?;
    if !link_ok(&before, uid) {
        return Err(unavailable("symlink"));
    }
    let mut buf = [0u8; 96];
    let n = unsafe { libc::readlinkat(dir, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 || n as usize == buf.len() {
        return Err(unavailable("symlink"));
    }
    between();
    let after = fstat_at(dir, name, libc::AT_SYMLINK_NOFOLLOW)?;
    if snap(&before) != snap(&after) {
        return Err(unavailable("symlink"));
    }
    let text = std::str::from_utf8(&buf[..n as usize]).map_err(|_| unavailable("symlink"))?;
    Ok(text.to_string())
}

fn link_ok(st: &libc::stat, uid: u32) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFLNK && st.st_uid == uid && st.st_nlink == 1
}

fn read_stable(fd: RawFd, uid: u32) -> io::Result<String> {
    let before = fstat(fd)?;
    if !source_ok(&before, uid) {
        return Err(unavailable("source"));
    }
    let first = bounded_read(fd)?;
    if before.st_size as usize != first.len() {
        return Err(unavailable("source"));
    }
    between();
    let after = fstat(fd)?;
    let second = bounded_read(fd)?;
    if snap(&before) != snap(&after) || first != second {
        return Err(unavailable("source"));
    }
    let text = std::str::from_utf8(&first).map_err(|_| unavailable("utf8"))?;
    Ok(text.to_string())
}

fn bounded_read(fd: RawFd) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; MAX_BYTES + 1];
    let mut off = 0usize;
    while off <= MAX_BYTES {
        let n = unsafe {
            libc::pread(
                fd,
                buf[off..].as_mut_ptr().cast(),
                buf.len() - off,
                off as i64,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n == 0 {
            break;
        }
        off += n as usize;
    }
    if off == 0 || off > MAX_BYTES {
        return Err(unavailable("source"));
    }
    buf.truncate(off);
    Ok(buf)
}

fn source_ok(st: &libc::stat, uid: u32) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFREG
        && st.st_uid == uid
        && st.st_nlink == 1
        && st.st_mode & 0o777 == 0o644
        && st.st_size > 0
        && st.st_size as usize <= MAX_BYTES
}

fn snap(st: &libc::stat) -> (u64, u64, i64, i64, i64, i64, i64, u32, u32, u64) {
    (
        st.st_dev,
        st.st_ino,
        st.st_size,
        st.st_mtime,
        st.st_mtime_nsec,
        st.st_ctime,
        st.st_ctime_nsec,
        st.st_uid,
        st.st_mode,
        st.st_nlink,
    )
}

fn between() {
    #[cfg(test)]
    {
        if let Some(hook) = BETWEEN.with(|slot| slot.replace(None)) {
            hook();
        }
    }
}

fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

fn fstat_at(dir: RawFd, name: &CStr, flags: i32) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

pub(super) fn render(text: &str) -> io::Result<String> {
    if text.len() > MAX_BYTES || !text.is_ascii() {
        return Err(unavailable("text"));
    }
    if text
        .bytes()
        .any(|b| b == 0x7f || (b < 0x20 && b != b'\n' && b != b'\t' && b != b'\r'))
    {
        return Err(unavailable("text"));
    }
    let mut out = String::new();
    let mut nameservers = 0usize;
    let mut saw_search = false;
    let mut saw_domain = false;
    let mut saw_options = false;
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let line = line.trim_matches([' ', '\t']);
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let line = strip_inline(line);
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let key = parts.next().ok_or_else(|| unavailable("line"))?;
        match key {
            "nameserver" => {
                let addr = parts.next().ok_or_else(|| unavailable("nameserver"))?;
                if parts.next().is_some() || addr.parse::<IpAddr>().is_err() {
                    return Err(unavailable("nameserver"));
                }
                nameservers += 1;
                if nameservers > MAX_NS {
                    return Err(unavailable("nameserver"));
                }
                out.push_str("nameserver ");
                out.push_str(addr);
                out.push('\n');
            }
            "search" => {
                if saw_search {
                    return Err(unavailable("search"));
                }
                saw_search = true;
                let domains: Vec<&str> = parts.collect();
                if domains.is_empty()
                    || domains.len() > MAX_SEARCH
                    || domains.iter().any(|d| !search_ok(d))
                {
                    return Err(unavailable("search"));
                }
                out.push_str("search ");
                out.push_str(&domains.join(" "));
                out.push('\n');
            }
            "domain" => {
                if saw_domain {
                    return Err(unavailable("domain"));
                }
                saw_domain = true;
                let name = parts.next().ok_or_else(|| unavailable("domain"))?;
                if parts.next().is_some() || !search_ok(name) {
                    return Err(unavailable("domain"));
                }
                out.push_str("domain ");
                out.push_str(name);
                out.push('\n');
            }
            "options" => {
                if saw_options {
                    return Err(unavailable("options"));
                }
                saw_options = true;
                let opts: Vec<&str> = parts.collect();
                if opts.is_empty() || opts.len() > 8 || opts.iter().any(|opt| !option_ok(opt)) {
                    return Err(unavailable("options"));
                }
                out.push_str("options ");
                out.push_str(&opts.join(" "));
                out.push('\n');
            }
            _ => return Err(unavailable("directive")),
        }
    }
    if nameservers == 0 {
        return Err(unavailable("empty"));
    }
    Ok(out)
}

fn strip_inline(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if (bytes[i] == b'#' || bytes[i] == b';')
            && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t')
        {
            return line[..i].trim_end_matches([' ', '\t']);
        }
        i += 1;
    }
    line
}

fn search_ok(name: &str) -> bool {
    if name == "." {
        return true;
    }
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.is_empty() || name.len() > 253 || name.starts_with('.') {
        return false;
    }
    name.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}

fn option_ok(opt: &str) -> bool {
    matches!(
        opt,
        "rotate"
            | "inet6"
            | "edns0"
            | "single-request"
            | "single-request-reopen"
            | "no-tld-query"
            | "use-vc"
            | "trust-ad"
            | "no-reload"
    ) || numbered(opt, "ndots:", 0, 15)
        || numbered(opt, "timeout:", 1, 30)
        || numbered(opt, "attempts:", 1, 5)
}

fn numbered(opt: &str, prefix: &str, lo: u32, hi: u32) -> bool {
    let Some(rest) = opt.strip_prefix(prefix) else {
        return false;
    };
    let Ok(n) = rest.parse::<u32>() else {
        return false;
    };
    (!rest.starts_with('0') && n >= lo && n <= hi) || (rest == "0" && lo == 0)
}

/// Exact marker. A short or missing marker is not a claim: those inodes are left, including a
/// legacy unmarked `.tmp`. Only a full marker plus `flock` is reclaimed, and the hold stays
/// until the paired `.tmp` is absent or removed.
const STAGE_MARK: &[u8] = b"fujiang-resolv-stage-v1\n";

struct Stage {
    dir: RawFd,
    tmp: CString,
    hold: CString,
    hold_fd: Option<OwnedFd>,
    tmp_dev: u64,
    tmp_ino: u64,
    armed: bool,
}

impl Stage {
    fn create(dir: RawFd) -> io::Result<(Self, OwnedFd)> {
        let id = next_stage_id();
        let hold = CString::new(format!(".resolv.{id}.hold")).map_err(|_| unavailable("tmp"))?;
        let tmp = CString::new(format!(".resolv.{id}.tmp")).map_err(|_| unavailable("tmp"))?;
        let hold_fd = open_new(dir, &hold)?;
        if unsafe { libc::flock(hold_fd.as_raw_fd(), libc::LOCK_EX) } != 0 {
            unlink_same(dir, &hold, hold_fd.as_raw_fd());
            return Err(unavailable("tmp"));
        }
        if write_full(hold_fd.as_raw_fd(), STAGE_MARK).is_err()
            || unsafe { libc::fsync(hold_fd.as_raw_fd()) } != 0
        {
            unlink_same(dir, &hold, hold_fd.as_raw_fd());
            return Err(unavailable("tmp"));
        }
        let body = match open_new(dir, &tmp) {
            Ok(fd) => fd,
            Err(err) => {
                unlink_same(dir, &hold, hold_fd.as_raw_fd());
                return Err(err);
            }
        };
        let st = match fstat(body.as_raw_fd()) {
            Ok(st) => st,
            Err(err) => {
                unlink_same(dir, &tmp, body.as_raw_fd());
                unlink_same(dir, &hold, hold_fd.as_raw_fd());
                return Err(err);
            }
        };
        Ok((
            Self {
                dir,
                tmp,
                hold,
                hold_fd: Some(hold_fd),
                tmp_dev: st.st_dev,
                tmp_ino: st.st_ino,
                armed: true,
            },
            body,
        ))
    }

    fn finish(&mut self) {
        if let Some(fd) = self.hold_fd.as_ref() {
            unlink_same(self.dir, &self.hold, fd.as_raw_fd());
        }
        self.armed = false;
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.armed {
            unlink_ino(self.dir, &self.tmp, self.tmp_dev, self.tmp_ino);
            if let Some(fd) = self.hold_fd.as_ref() {
                unlink_same(self.dir, &self.hold, fd.as_raw_fd());
            }
        }
    }
}

fn open_new(dir: RawFd, name: &CStr) -> io::Result<OwnedFd> {
    let fd = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_RDWR
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK,
            0o644,
        )
    };
    if fd < 0 {
        Err(unavailable("tmp"))
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn write_full(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    let mut off = 0usize;
    while off < bytes.len() {
        let n = unsafe { libc::write(fd, bytes[off..].as_ptr().cast(), bytes.len() - off) };
        if n <= 0 {
            return Err(unavailable("tmp"));
        }
        off += n as usize;
    }
    Ok(())
}

fn unlink_same(dir: RawFd, name: &CStr, fd: RawFd) {
    let Ok(opened) = fstat(fd) else {
        return;
    };
    unlink_ino(dir, name, opened.st_dev, opened.st_ino);
}

fn unlink_ino(dir: RawFd, name: &CStr, dev: u64, ino: u64) {
    let Ok(named) = fstat_at(dir, name, libc::AT_SYMLINK_NOFOLLOW) else {
        return;
    };
    if named.st_dev == dev && named.st_ino == ino && named.st_nlink == 1 {
        let _ = unsafe { libc::unlinkat(dir, name.as_ptr(), 0) };
    }
}

#[path = "workspace_resolv_reclaim.rs"]
mod stage_reclaim;
use stage_reclaim::reclaim;

fn write_guest(root: RawFd, body: &[u8]) -> io::Result<()> {
    let etc = open_at(root, c"etc", OPEN_DIR | libc::O_NOFOLLOW)?;
    let euid = unsafe { libc::geteuid() };
    let st = fstat(etc.as_raw_fd())?;
    if st.st_uid != euid || st.st_mode & 0o200 == 0 {
        return Err(unavailable("guest"));
    }
    let dest = c"resolv.conf";
    match fstat_at(etc.as_raw_fd(), dest, libc::AT_SYMLINK_NOFOLLOW) {
        Ok(st) => {
            let kind = st.st_mode & libc::S_IFMT;
            let owned = st.st_uid == euid && st.st_nlink == 1;
            let writable = kind == libc::S_IFREG && st.st_mode & 0o200 != 0;
            if !owned
                || (kind == libc::S_IFREG && !writable)
                || (kind != libc::S_IFREG && kind != libc::S_IFLNK)
            {
                return Err(unavailable("guest"));
            }
        }
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => {}
        Err(err) => return Err(err),
    }
    reclaim(etc.as_raw_fd(), euid)?;
    let (mut stage, tmp) = Stage::create(etc.as_raw_fd())?;
    let mut off = 0usize;
    while off < body.len() {
        let n = unsafe {
            libc::write(
                tmp.as_raw_fd(),
                body[off..].as_ptr().cast(),
                body.len() - off,
            )
        };
        if n <= 0 {
            return Err(unavailable("write"));
        }
        off += n as usize;
    }
    if unsafe { libc::fchmod(tmp.as_raw_fd(), 0o644) } != 0
        || unsafe { libc::fsync(tmp.as_raw_fd()) } != 0
    {
        return Err(unavailable("write"));
    }
    drop(tmp);
    if unsafe {
        libc::renameat(
            etc.as_raw_fd(),
            stage.tmp.as_ptr(),
            etc.as_raw_fd(),
            dest.as_ptr(),
        )
    } != 0
    {
        return Err(unavailable("rename"));
    }
    stage.finish();
    Ok(())
}

fn next_stage_id() -> String {
    #[cfg(test)]
    if let Some(id) = NEXT_STAGE_ID.with(|slot| slot.borrow_mut().take()) {
        return id;
    }
    format!(
        "{}.{}",
        std::process::id(),
        STAGE_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

fn after_lock_hook() {
    #[cfg(test)]
    {
        if let Some(hook) = SWAP.with(|slot| slot.replace(None)) {
            hook();
        }
    }
}

fn before_tmp_hook() {
    #[cfg(test)]
    {
        if let Some(hook) = TMP_SWAP.with(|slot| slot.replace(None)) {
            hook();
        }
    }
}

#[cfg(test)]
thread_local! {
    static BETWEEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static SWAP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static TMP_SWAP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static NEXT_STAGE_ID: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[path = "workspace_resolv_gc_tests.rs"]
mod gc_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn root_fd(path: &Path) -> OwnedFd {
        let c = CString::new(path.as_os_str().as_bytes()).unwrap();
        let fd = unsafe { libc::open(c.as_ptr(), OPEN_DIR) };
        assert!(fd >= 0);
        unsafe { OwnedFd::from_raw_fd(fd) }
    }

    fn scratch(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("fujiang-resolv-{}-{}", label, std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        path
    }

    fn rewrite_same(path: &Path, body: &[u8]) {
        let modified = fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        fs::write(path, body).unwrap();
        let stamp = libc::timespec {
            tv_sec: modified.as_secs() as i64,
            tv_nsec: modified.subsec_nanos() as i64,
        };
        let times = [stamp, stamp];
        let c = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::utimensat(
                    libc::AT_FDCWD,
                    c.as_ptr(),
                    times.as_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            },
            0
        );
    }

    fn public_file(path: &Path, body: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn fixture_render_preserves_servers_and_comments() {
        let got = render(
            "  # note\n\t; also\nnameserver 192.0.2.1 # keep\nsearch . example.test\ndomain example.test\noptions ndots:0\n",
        )
        .unwrap();
        assert_eq!(
            got,
            "nameserver 192.0.2.1\nsearch . example.test\ndomain example.test\noptions ndots:0\n"
        );
        let kept = render("nameserver 8.8.8.8\nnameserver 1.1.1.1\n").unwrap();
        assert_eq!(kept, "nameserver 8.8.8.8\nnameserver 1.1.1.1\n");
        assert!(!kept.contains("192.0.2.1"));
        assert!(render("nameserver 2001:db8::1\n")
            .unwrap()
            .contains("2001:db8::1"));
        assert!(render("").is_err());
        assert!(render("nameserver 192.0.2.1 extra\n").is_err());
        assert!(render("nameserver 256.0.0.1\n").is_err());
        assert!(render("nameserver 192.0.2.1\n\u{0001}").is_err());
        assert!(render("nameserver 192.0.2.1\n名字\n").is_err());
        assert!(render(&"x".repeat(MAX_BYTES + 1)).is_err());
    }

    #[test]
    fn fixture_source_walk_rejects_hostile_metadata() {
        let uid = unsafe { libc::geteuid() };
        let root = scratch("src");
        public_file(&root.join("etc/resolv.conf"), b"nameserver 192.0.2.1\n");
        let fd = root_fd(&root);
        let text = read_under(fd.as_raw_fd(), uid).unwrap();
        assert_eq!(render(&text).unwrap(), "nameserver 192.0.2.1\n");

        let fifo = root.join("etc/fifo.conf");
        fs::remove_file(root.join("etc/resolv.conf")).unwrap();
        let c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
        fs::rename(&fifo, root.join("etc/resolv.conf")).unwrap();
        assert!(read_under(fd.as_raw_fd(), uid).is_err());

        fs::remove_file(root.join("etc/resolv.conf")).unwrap();
        fs::create_dir(root.join("etc/resolv.conf")).unwrap();
        assert!(read_under(fd.as_raw_fd(), uid).is_err());
        fs::remove_dir(root.join("etc/resolv.conf")).unwrap();

        public_file(&root.join("etc/resolv.conf"), b"nameserver 192.0.2.1\n");
        fs::hard_link(root.join("etc/resolv.conf"), root.join("etc/alias")).unwrap();
        assert!(read_under(fd.as_raw_fd(), uid).is_err());
        fs::remove_file(root.join("etc/alias")).unwrap();

        assert!(read_under(fd.as_raw_fd(), uid.wrapping_add(1)).is_err());

        let mut perms = fs::metadata(root.join("etc")).unwrap().permissions();
        perms.set_mode(0o777);
        fs::set_permissions(root.join("etc"), perms.clone()).unwrap();
        assert!(read_under(fd.as_raw_fd(), uid).is_err());
        perms.set_mode(0o755);
        fs::set_permissions(root.join("etc"), perms).unwrap();

        fs::rename(root.join("etc"), root.join("real-etc")).unwrap();
        std::os::unix::fs::symlink("real-etc", root.join("etc")).unwrap();
        assert!(read_under(fd.as_raw_fd(), uid).is_err());
        fs::remove_file(root.join("etc")).unwrap();
        fs::rename(root.join("real-etc"), root.join("etc")).unwrap();

        public_file(
            &root.join("run/systemd/resolve/stub-resolv.conf"),
            b"nameserver 192.0.2.10\n",
        );
        fs::remove_file(root.join("etc/resolv.conf")).unwrap();
        std::os::unix::fs::symlink(
            "/run/systemd/resolve/stub-resolv.conf",
            root.join("etc/resolv.conf"),
        )
        .unwrap();
        let linked = read_under(fd.as_raw_fd(), uid).unwrap();
        assert!(linked.contains("192.0.2.10"));
        let etc_fd = open_at(fd.as_raw_fd(), c"etc", OPEN_DIR).unwrap();
        assert!(read_link(etc_fd.as_raw_fd(), c"resolv.conf", uid.wrapping_add(1)).is_err());
        let alias = CString::new(root.join("etc/link2").as_os_str().as_bytes()).unwrap();
        let link = CString::new(root.join("etc/resolv.conf").as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::linkat(
                    libc::AT_FDCWD,
                    link.as_ptr(),
                    libc::AT_FDCWD,
                    alias.as_ptr(),
                    0,
                )
            },
            0
        );
        assert!(read_link(etc_fd.as_raw_fd(), c"resolv.conf", uid).is_err());
        fs::remove_file(root.join("etc/link2")).unwrap();
        let link_path = root.join("etc/resolv.conf");
        BETWEEN.with(|slot| {
            let path = link_path.clone();
            *slot.borrow_mut() = Some(Box::new(move || {
                let backup = path.with_file_name("resolv.conf.bak");
                fs::rename(&path, &backup).unwrap();
                std::os::unix::fs::symlink("/run/systemd/resolve/stub-resolv.conf", &path).unwrap();
            }));
        });
        assert!(read_link(etc_fd.as_raw_fd(), c"resolv.conf", uid).is_err());
        drop(etc_fd);

        fs::remove_file(root.join("run/systemd/resolve/stub-resolv.conf")).unwrap();
        std::os::unix::fs::symlink(
            "elsewhere",
            root.join("run/systemd/resolve/stub-resolv.conf"),
        )
        .unwrap();
        assert!(read_under(fd.as_raw_fd(), uid).is_err());
        fs::remove_file(root.join("etc/resolv.conf")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("etc/resolv.conf")).unwrap();
        assert!(read_under(fd.as_raw_fd(), uid).is_err());

        public_file(&root.join("etc/live"), b"nameserver 192.0.2.1\n");
        fs::remove_file(root.join("etc/resolv.conf")).unwrap();
        fs::rename(root.join("etc/live"), root.join("etc/resolv.conf")).unwrap();
        let live = root.join("etc/resolv.conf");
        BETWEEN.with(|slot| {
            let path = live.clone();
            *slot.borrow_mut() = Some(Box::new(move || {
                let _ = fs::write(&path, b"nameserver 192.0.2.1\nnameserver 192.0.2.2\n");
            }));
        });
        assert!(read_under(fd.as_raw_fd(), uid).is_err());
        public_file(&root.join("etc/resolv.conf"), b"nameserver 192.0.2.1\n");
        let same = root.join("etc/resolv.conf");
        BETWEEN.with(|slot| {
            let path = same.clone();
            *slot.borrow_mut() = Some(Box::new(move || {
                rewrite_same(&path, b"nameserver 192.0.2.9\n");
            }));
        });
        assert!(read_under(fd.as_raw_fd(), uid).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fixture_guest_refresh_symlink_and_cleanup() {
        let root = scratch("guest");
        fs::create_dir(root.join("etc")).unwrap();
        let fd = root_fd(&root);
        let body = b"nameserver 192.0.2.1\n";
        write_guest(fd.as_raw_fd(), body).unwrap();
        write_guest(fd.as_raw_fd(), b"nameserver 198.51.100.1\n").unwrap();
        assert_eq!(
            fs::read(root.join("etc/resolv.conf")).unwrap(),
            b"nameserver 198.51.100.1\n"
        );
        let mut perms = fs::metadata(root.join("etc/resolv.conf"))
            .unwrap()
            .permissions();
        perms.set_mode(0o444);
        fs::set_permissions(root.join("etc/resolv.conf"), perms.clone()).unwrap();
        assert!(write_guest(fd.as_raw_fd(), body).is_err());
        assert_eq!(
            fs::read(root.join("etc/resolv.conf")).unwrap(),
            b"nameserver 198.51.100.1\n"
        );
        perms.set_mode(0o644);
        fs::set_permissions(root.join("etc/resolv.conf"), perms).unwrap();

        let outside = scratch("sentinel");
        fs::write(outside.join("keep"), b"UNTOUCHED").unwrap();
        fs::remove_file(root.join("etc/resolv.conf")).unwrap();
        std::os::unix::fs::symlink(outside.join("keep"), root.join("etc/resolv.conf")).unwrap();
        NEXT_STAGE_ID.with(|slot| *slot.borrow_mut() = Some("fixture.9".into()));
        let blocker = root.join("etc/.resolv.fixture.9.tmp");
        fs::write(&blocker, b"block").unwrap();
        assert!(write_guest(fd.as_raw_fd(), body).is_err());
        assert!(fs::symlink_metadata(root.join("etc/resolv.conf"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"UNTOUCHED");
        assert_eq!(fs::read(&blocker).unwrap(), b"block");
        write_guest(fd.as_raw_fd(), body).unwrap();
        assert_eq!(fs::read(&blocker).unwrap(), b"block");
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"UNTOUCHED");
        assert!(fs::symlink_metadata(root.join("etc/resolv.conf"))
            .unwrap()
            .is_file());
        assert_eq!(fs::read(root.join("etc/resolv.conf")).unwrap(), body);

        fs::hard_link(root.join("etc/resolv.conf"), root.join("etc/alias")).unwrap();
        assert!(write_guest(fd.as_raw_fd(), body).is_err());
        fs::remove_file(root.join("etc/alias")).unwrap();
        fs::write(root.join("etc/resolv.conf.fujiang"), b"leave").unwrap();

        let mut dir = fs::metadata(root.join("etc")).unwrap().permissions();
        dir.set_mode(0o555);
        fs::set_permissions(root.join("etc"), dir.clone()).unwrap();
        assert!(write_guest(fd.as_raw_fd(), body).is_err());
        assert_eq!(
            fs::read(root.join("etc/resolv.conf.fujiang")).unwrap(),
            b"leave"
        );
        dir.set_mode(0o755);
        fs::set_permissions(root.join("etc"), dir).unwrap();

        fs::rename(root.join("etc"), root.join("real")).unwrap();
        std::os::unix::fs::symlink("real", root.join("etc")).unwrap();
        assert!(write_guest(fd.as_raw_fd(), body).is_err());
        assert_eq!(fs::read(root.join("real/resolv.conf")).unwrap(), body);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }
}
