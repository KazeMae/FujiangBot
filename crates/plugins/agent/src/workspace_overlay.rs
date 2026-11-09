//! Host-overlay owner layout. Directories only; no file copy except dpkg status.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use super::{fstat, invalid, one_component, openat2, raw_owned};

const STATUS_CAP: u64 = 64 * 1024 * 1024;
const SHADOWS: [&str; 5] = [
    "var/lib/dpkg/lock",
    "var/lib/dpkg/lock-frontend",
    "var/lib/dpkg/triggers/Lock",
    "var/lib/apt/lists/lock",
    "var/cache/debconf/passwords.dat",
];

pub(super) fn prepare(owner: RawFd) -> io::Result<()> {
    for name in ["work", "varlog", "aptlists-partial"] {
        owned_dir(owner, name)?;
    }
    let apt = owned_dir(owner, "aptcache")?;
    owned_dir(apt.as_raw_fd(), "archives")?;
    let archives = open_dir(apt.as_raw_fd(), "archives")?;
    owned_dir(archives.as_raw_fd(), "partial")?;
    let upper = owned_dir(owner, "upper")?;
    let ovl = owned_dir(owner, "ovlwork")?;
    let fresh = host_stamp()?;
    if needs_walk(read_stamp(owner)?, fresh) {
        for top in [c"usr".as_ptr(), c"etc".as_ptr(), c"var".as_ptr()] {
            let name = unsafe { CStr::from_ptr(top) };
            refresh_top(upper.as_raw_fd(), ovl.as_raw_fd(), name)?;
        }
        write_stamp(owner, &encode(&fresh))?;
    } else {
        for top in [c"usr".as_ptr(), c"etc".as_ptr(), c"var".as_ptr()] {
            let name = unsafe { CStr::from_ptr(top) };
            ensure_top(upper.as_raw_fd(), ovl.as_raw_fd(), name)?;
        }
    }
    for rel in SHADOWS {
        touch_absent(&upper, rel)?;
    }
    copy_status_once(&upper)?;
    write_apt_conf(&upper)?;
    create_empty(owner, c"empty-auth")?;
    Ok(())
}

fn create_empty(dir: RawFd, name: &CStr) -> io::Result<()> {
    let fd = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o644,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { libc::close(fd) };
    Ok(())
}

pub(crate) fn include_dir(
    is_lnk: bool,
    is_dir: bool,
    dev: u64,
    top_dev: u64,
    readable: bool,
) -> bool {
    !is_lnk && is_dir && dev == top_dev && readable
}

pub(crate) fn landlock_scope_ok(abi: i32) -> io::Result<()> {
    if abi < 6 {
        Err(io::Error::other("unavailable: landlock scope"))
    } else {
        Ok(())
    }
}

fn refresh_top(upper: RawFd, ovl: RawFd, name: &CStr) -> io::Result<()> {
    let host = ensure_top(upper, ovl, name)?;
    let st = fstat(host.as_raw_fd())?;
    let up = open_dir(upper, name.to_str().map_err(|_| invalid("top"))?)?;
    walk(host.as_raw_fd(), up.as_raw_fd(), st.st_dev as u64, 0)?;
    Ok(())
}

fn ensure_top(upper: RawFd, ovl: RawFd, name: &CStr) -> io::Result<OwnedFd> {
    let host = open_host_top(name)?;
    let st = fstat(host.as_raw_fd())?;
    if !include_dir(false, is_dir(&st), st.st_dev as u64, st.st_dev as u64, true) {
        return Err(invalid("overlay top"));
    }
    mkdir_at(upper, name, dir_mode(&st))?;
    mkdir_at(ovl, name, 0o700)?;
    Ok(host)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileMark {
    dev: u64,
    ino: u64,
    size: u64,
    sec: i64,
    nsec: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirMark {
    dev: u64,
    ino: u64,
    sec: i64,
    nsec: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    status: FileMark,
    usr: DirMark,
    etc: DirMark,
    var: DirMark,
}

fn needs_walk(old: Option<Stamp>, new: Stamp) -> bool {
    old != Some(new)
}

fn host_stamp() -> io::Result<Stamp> {
    Ok(Stamp {
        status: status_mark()?,
        usr: dir_mark(c"usr")?,
        etc: dir_mark(c"etc")?,
        var: dir_mark(c"var")?,
    })
}

fn status_mark() -> io::Result<FileMark> {
    let fd = raw_owned(unsafe {
        libc::open(
            c"/var/lib/dpkg/status".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    })?;
    let st = fstat(fd.as_raw_fd())?;
    require_regular(st.st_mode)?;
    if st.st_size < 0 {
        return Err(invalid("dpkg status size"));
    }
    Ok(FileMark {
        dev: st.st_dev,
        ino: st.st_ino,
        size: st.st_size as u64,
        sec: st.st_mtime,
        nsec: st.st_mtime_nsec,
    })
}

fn dir_mark(name: &CStr) -> io::Result<DirMark> {
    let host = open_host_top(name)?;
    let st = fstat(host.as_raw_fd())?;
    if !is_dir(&st) {
        return Err(invalid("overlay top"));
    }
    Ok(DirMark {
        dev: st.st_dev,
        ino: st.st_ino,
        sec: st.st_mtime,
        nsec: st.st_mtime_nsec,
    })
}

fn encode(stamp: &Stamp) -> String {
    let s = stamp.status;
    format!(
        "status {} {} {} {} {}\nusr {} {} {} {}\netc {} {} {} {}\nvar {} {} {} {}\n",
        s.dev,
        s.ino,
        s.size,
        s.sec,
        s.nsec,
        stamp.usr.dev,
        stamp.usr.ino,
        stamp.usr.sec,
        stamp.usr.nsec,
        stamp.etc.dev,
        stamp.etc.ino,
        stamp.etc.sec,
        stamp.etc.nsec,
        stamp.var.dev,
        stamp.var.ino,
        stamp.var.sec,
        stamp.var.nsec
    )
}

fn parse_stamp(text: &str) -> Option<Stamp> {
    let mut lines = text.lines();
    let status = parse_file(lines.next()?)?;
    let usr = parse_dir(lines.next()?, "usr")?;
    let etc = parse_dir(lines.next()?, "etc")?;
    let var = parse_dir(lines.next()?, "var")?;
    if lines.next().is_some() {
        return None;
    }
    Some(Stamp {
        status,
        usr,
        etc,
        var,
    })
}

fn parse_file(line: &str) -> Option<FileMark> {
    let parts: Vec<&str> = line.split(' ').collect();
    if parts.len() != 6 || parts[0] != "status" {
        return None;
    }
    Some(FileMark {
        dev: parts[1].parse().ok()?,
        ino: parts[2].parse().ok()?,
        size: parts[3].parse().ok()?,
        sec: parts[4].parse().ok()?,
        nsec: parts[5].parse().ok()?,
    })
}

fn parse_dir(line: &str, label: &str) -> Option<DirMark> {
    let parts: Vec<&str> = line.split(' ').collect();
    if parts.len() != 5 || parts[0] != label {
        return None;
    }
    Some(DirMark {
        dev: parts[1].parse().ok()?,
        ino: parts[2].parse().ok()?,
        sec: parts[3].parse().ok()?,
        nsec: parts[4].parse().ok()?,
    })
}

fn read_stamp(owner: RawFd) -> io::Result<Option<Stamp>> {
    let fd = unsafe {
        libc::openat(
            owner,
            c"host-stamp".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        let err = io::Error::last_os_error();
        if matches!(err.raw_os_error(), Some(libc::ENOENT | libc::ELOOP)) {
            return Ok(None);
        }
        return Err(err);
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let mut body = String::new();
    file.read_to_string(&mut body)?;
    Ok(parse_stamp(&body))
}

fn write_stamp(owner: RawFd, body: &str) -> io::Result<()> {
    unlink_stamp_tmp(owner)?;
    let fd = unsafe {
        libc::openat(
            owner,
            c".host-stamp.tmp".as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o644,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(body.as_bytes())?;
    drop(file);
    if unsafe {
        libc::renameat(
            owner,
            c".host-stamp.tmp".as_ptr(),
            owner,
            c"host-stamp".as_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn unlink_stamp_tmp(owner: RawFd) -> io::Result<()> {
    if unsafe { libc::unlinkat(owner, c".host-stamp.tmp".as_ptr(), 0) } != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ENOENT) {
            return Err(err);
        }
    }
    Ok(())
}

fn walk(host: RawFd, upper: RawFd, top_dev: u64, depth: u32) -> io::Result<()> {
    if depth > 64 {
        return Err(invalid("overlay depth"));
    }
    let dup = raw_owned(unsafe { libc::dup(host) })?;
    let dir = unsafe { libc::fdopendir(dup.as_raw_fd()) };
    if dir.is_null() {
        return Err(io::Error::last_os_error());
    }
    std::mem::forget(dup);
    let mut names = Vec::new();
    loop {
        errno_clear();
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            if std::io::Error::last_os_error().raw_os_error().unwrap_or(0) != 0 {
                unsafe { libc::closedir(dir) };
                return Err(io::Error::last_os_error());
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        if name == c"." || name == c".." {
            continue;
        }
        names.push(name.to_owned());
    }
    unsafe { libc::closedir(dir) };
    for name in names {
        let child = match open_child(host, &name) {
            Ok(fd) => fd,
            Err(_) => continue,
        };
        let st = fstat(child.as_raw_fd())?;
        if !include_dir(false, is_dir(&st), st.st_dev as u64, top_dev, true) {
            continue;
        }
        mkdir_at(upper, &name, dir_mode(&st))?;
        let up = match open_dir_c(upper, &name) {
            Ok(fd) => fd,
            Err(err) if skip_replaced(&err) => continue,
            Err(err) => return Err(err),
        };
        walk(child.as_raw_fd(), up.as_raw_fd(), top_dev, depth + 1)?;
    }
    Ok(())
}

fn open_child(dir: RawFd, name: &CString) -> io::Result<OwnedFd> {
    openat2(
        dir,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )
}

fn open_host_top(name: &CStr) -> io::Result<OwnedFd> {
    let name = CString::new(name.to_bytes()).map_err(|_| invalid("top"))?;
    let root = raw_owned(unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })?;
    openat2(
        root.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )
}

fn is_dir(st: &libc::stat) -> bool {
    (st.st_mode & libc::S_IFMT) == libc::S_IFDIR
}

fn dir_mode(st: &libc::stat) -> u32 {
    (st.st_mode & 0o7777) & !libc::S_ISUID & !libc::S_ISGID
}

fn mkdir_at(dir: RawFd, name: &CStr, mode: u32) -> io::Result<()> {
    if unsafe { libc::mkdirat(dir, name.as_ptr(), mode) } != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EEXIST) {
            return Err(err);
        }
    } else if unsafe { libc::fchmodat(dir, name.as_ptr(), mode, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn touch_absent(upper: &OwnedFd, rel: &str) -> io::Result<()> {
    let Some((parent, leaf)) = descend(upper.as_raw_fd(), rel)? else {
        return Ok(());
    };
    if exists_leaf(parent.as_raw_fd(), &leaf)? {
        return Ok(());
    }
    if let Some(fd) = create_exclusive(parent.as_raw_fd(), &leaf)? {
        unsafe { libc::close(fd) };
    }
    Ok(())
}

fn copy_status_once(upper: &OwnedFd) -> io::Result<()> {
    for name in ["var/lib/dpkg/status", "var/lib/dpkg/available"] {
        copy_regular(name, upper.as_raw_fd())?;
    }
    Ok(())
}

pub(crate) fn require_regular(mode: u32) -> io::Result<()> {
    if mode & libc::S_IFMT != libc::S_IFREG {
        return Err(invalid("dpkg status must be regular"));
    }
    Ok(())
}

pub(crate) fn copy_regular(rel: &str, upper: RawFd) -> io::Result<()> {
    let Some((parent, leaf)) = descend(upper, rel)? else {
        return Ok(());
    };
    if exists_leaf(parent.as_raw_fd(), &leaf)? {
        return Ok(());
    }
    let c = CString::new(format!("/{rel}")).map_err(|_| invalid("rel"))?;
    let src_fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if src_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let src = unsafe { File::from_raw_fd(src_fd) };
    let st = fstat(src.as_raw_fd())?;
    require_regular(st.st_mode)?;
    if st.st_size < 0 || st.st_size as u64 > STATUS_CAP {
        return Err(invalid("dpkg status size"));
    }
    let mut buf = Vec::new();
    (&src).take(STATUS_CAP).read_to_end(&mut buf)?;
    let Some(fd) = create_exclusive(parent.as_raw_fd(), &leaf)? else {
        return Ok(());
    };
    let mut out = unsafe { File::from_raw_fd(fd) };
    out.write_all(&buf)?;
    Ok(())
}

fn write_apt_conf(upper: &OwnedFd) -> io::Result<()> {
    let Some((parent, leaf)) = descend(upper.as_raw_fd(), "etc/apt/apt.conf.d/00fujiang")? else {
        return Ok(());
    };
    if exists_leaf(parent.as_raw_fd(), &leaf)? {
        return Ok(());
    }
    let Some(fd) = create_exclusive(parent.as_raw_fd(), &leaf)? else {
        return Ok(());
    };
    let mut out = unsafe { File::from_raw_fd(fd) };
    out.write_all(b"APT::Sandbox::User \"root\";\n")?;
    Ok(())
}

/// Parent directory via openat2 (beneath, no symlinks, no magic links).
/// `None` means a component was a symlink or not a directory; the caller skips it.
fn descend(start: RawFd, rel: &str) -> io::Result<Option<(OwnedFd, CString)>> {
    let (parent, leaf) = rel.rsplit_once('/').unwrap_or(("", rel));
    let leaf = one_component(leaf)?;
    let dir = if parent.is_empty() {
        raw_owned(unsafe { libc::dup(start) })?
    } else {
        match openat2(
            start,
            &rel_path(parent)?,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        ) {
            Ok(fd) => fd,
            Err(err) if skip_replaced(&err) => return Ok(None),
            Err(err) => return Err(err),
        }
    };
    Ok(Some((dir, leaf)))
}

fn rel_path(rel: &str) -> io::Result<CString> {
    if rel.is_empty()
        || rel.starts_with('/')
        || rel
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid("overlay rel"));
    }
    CString::new(rel).map_err(|_| invalid("overlay rel"))
}

fn exists_leaf(dir: RawFd, leaf: &CStr) -> io::Result<bool> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatat(dir, leaf.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
    if rc == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ENOENT) {
        Ok(false)
    } else {
        Err(err)
    }
}

fn create_exclusive(dir: RawFd, leaf: &CStr) -> io::Result<Option<RawFd>> {
    let fd = unsafe {
        libc::openat(
            dir,
            leaf.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o644,
        )
    };
    if fd >= 0 {
        return Ok(Some(fd));
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EEXIST) || skip_replaced(&err) {
        Ok(None)
    } else {
        Err(err)
    }
}

fn skip_replaced(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(libc::ELOOP | libc::ENOTDIR | libc::ENXIO)
    )
}

fn owned_dir(parent: RawFd, name: &str) -> io::Result<OwnedFd> {
    let name = one_component(name)?;
    if unsafe { libc::mkdirat(parent, name.as_ptr(), 0o700) } != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EEXIST) {
            return Err(err);
        }
    }
    open_dir_c(parent, &name)
}

fn open_dir(parent: RawFd, name: &str) -> io::Result<OwnedFd> {
    open_dir_c(parent, &one_component(name)?)
}

fn open_dir_c(parent: RawFd, name: &CString) -> io::Result<OwnedFd> {
    openat2(
        parent,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )
}

fn errno_clear() {
    unsafe { *libc::__errno_location() = 0 };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn skeleton_predicates_skip_bad_dirs() {
        assert!(!include_dir(true, true, 1, 1, true));
        assert!(!include_dir(false, true, 2, 1, true));
        assert!(!include_dir(false, true, 1, 1, false));
        assert!(!include_dir(false, false, 1, 1, true));
        assert!(include_dir(false, true, 1, 1, true));
    }

    #[test]
    fn landlock_below_abi_six_fails_closed() {
        let err = landlock_scope_ok(5).unwrap_err();
        assert!(err.to_string().contains("unavailable: landlock scope"));
        assert!(landlock_scope_ok(6).is_ok());
    }

    #[test]
    fn status_copy_refuses_non_regular() {
        assert!(require_regular(libc::S_IFIFO | 0o644).is_err());
        assert!(require_regular(libc::S_IFREG | 0o644).is_ok());
    }

    fn sample(size: u64, sec: i64) -> Stamp {
        Stamp {
            status: FileMark {
                dev: 1,
                ino: 2,
                size,
                sec,
                nsec: 3,
            },
            usr: DirMark {
                dev: 1,
                ino: 4,
                sec,
                nsec: 5,
            },
            etc: DirMark {
                dev: 1,
                ino: 6,
                sec,
                nsec: 7,
            },
            var: DirMark {
                dev: 1,
                ino: 8,
                sec,
                nsec: 9,
            },
        }
    }

    #[test]
    fn walk_only_when_stamp_changes() {
        let now = sample(10, 20);
        assert!(needs_walk(None, now));
        assert!(!needs_walk(Some(now), now));
        assert!(needs_walk(Some(now), sample(11, 20)));
        let mut top = now;
        top.usr.sec += 1;
        assert!(needs_walk(Some(now), top));
    }

    #[test]
    fn second_prepare_keeps_the_host_stamp() {
        let dir = std::env::temp_dir().join(format!(
            "ovl-stamp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let fd = std::fs::File::open(&dir).unwrap();
        let first = prepare(fd.as_raw_fd());
        let second = first.and_then(|()| prepare(fd.as_raw_fd()));
        let body = std::fs::read(dir.join("host-stamp"));
        let _ = std::fs::remove_dir_all(&dir);
        second.unwrap();
        let expect = encode(&host_stamp().unwrap());
        assert_eq!(String::from_utf8(body.unwrap()).unwrap(), expect);
    }

    #[test]
    fn refresh_skips_symlink() {
        let host = std::env::temp_dir().join(format!("ovl-host-{}", std::process::id()));
        let up = std::env::temp_dir().join(format!("ovl-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&host);
        let _ = std::fs::remove_dir_all(&up);
        std::fs::create_dir_all(host.join("keep")).unwrap();
        symlink("keep", host.join("skip")).unwrap();
        std::fs::create_dir_all(&up).unwrap();
        let h = std::fs::File::open(&host).unwrap();
        let u = std::fs::File::open(&up).unwrap();
        let st = fstat(h.as_raw_fd()).unwrap();
        super::walk(h.as_raw_fd(), u.as_raw_fd(), st.st_dev as u64, 0).unwrap();
        assert!(up.join("keep").is_dir());
        assert!(!up.join("skip").exists());
        let _ = std::fs::remove_dir_all(&host);
        let _ = std::fs::remove_dir_all(&up);
    }
}
