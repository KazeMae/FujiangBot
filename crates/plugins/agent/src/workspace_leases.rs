use std::ffi::{CStr, CString};
use std::io::{self, ErrorKind};
use std::mem::ManuallyDrop;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd, RawFd};
#[cfg(test)]
use std::os::unix::process::CommandExt;
#[cfg(test)]
use std::process::{Child, Command, Stdio};

use super::super::workspace_seed::{self, VerifiedBase};
use super::super::{fstat, invalid, one_component, openat2, raw_owned};
use super::open_dir;

const MARKER: &[u8] = b"fujiang-import-lease-v1\n";
const LIST_CAP: usize = 256;

#[derive(Debug)]
pub(super) struct Report {
    pub reclaimed: u32,
    pub preserved: u32,
    pub truncated: bool,
}

pub(super) struct Held {
    parent: OwnedFd,
    dir: OwnedFd,
    name: CString,
    lease: OwnedFd,
    keep: bool,
}

impl Drop for Held {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = workspace_seed::remove_tree(self.parent.as_raw_fd(), &self.name);
    }
}

impl Held {
    pub(super) fn dir_fd(&self) -> RawFd {
        self.dir.as_raw_fd()
    }

    pub(super) fn lease_fd(&self) -> RawFd {
        self.lease.as_raw_fd()
    }

    pub(super) fn into_base(
        mut self,
        root: OwnedFd,
        identity: [u8; 32],
    ) -> io::Result<VerifiedBase> {
        self.keep = true;
        let this = ManuallyDrop::new(self);
        let parent = unsafe { std::ptr::read(&this.parent) };
        let name = unsafe { std::ptr::read(&this.name) };
        let lease = unsafe { std::ptr::read(&this.lease) };
        drop(unsafe { std::ptr::read(&this.dir) });
        let backup = dup_fd(parent.as_raw_fd())?;
        let saved = name.clone();
        match VerifiedBase::from_sealed(root, identity, parent, name, lease) {
            Ok(base) => Ok(base),
            Err(err) => {
                let _ = workspace_seed::remove_tree(backup.as_raw_fd(), &saved);
                Err(err)
            }
        }
    }

    /// Test helper: parent exit must not delete the dir. Closes nothing until process exit.
    #[cfg(test)]
    pub(super) fn leak_for_child(mut self) {
        self.keep = true;
        std::mem::forget(self);
    }

    /// Drop the lock without deleting. The directory is then a proven stale lease.
    #[cfg(test)]
    pub(super) fn abandon(self) {
        let this = ManuallyDrop::new(self);
        drop(unsafe { std::ptr::read(&this.parent) });
        drop(unsafe { std::ptr::read(&this.dir) });
        drop(unsafe { std::ptr::read(&this.name) });
        drop(unsafe { std::ptr::read(&this.lease) });
    }
}

pub(super) fn create_owned(trust: RawFd, name: &str) -> io::Result<Held> {
    let _guard = lock_import(trust)?;
    let cname = one_component(name)?;
    if unsafe { libc::mkdirat(trust, cname.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let built = (|| {
        let dir = open_dir(trust, &cname)?;
        let lease = write_lease(dir.as_raw_fd())?;
        Ok(Held {
            parent: dup_fd(trust)?,
            dir,
            name: cname.clone(),
            lease,
            keep: false,
        })
    })();
    if built.is_err() {
        let _ = workspace_seed::remove_tree(trust, &cname);
    }
    built
}

pub(super) fn reclaim(trust: RawFd) -> io::Result<Report> {
    let _guard = lock_import(trust)?;
    let mut names = list_names(trust)?;
    let truncated = names.len() > LIST_CAP;
    if truncated {
        names.truncate(LIST_CAP);
    }
    let mut report = Report {
        reclaimed: 0,
        preserved: truncated as u32,
        truncated,
    };
    for name in names {
        let label = match name.to_str() {
            Ok(text) => text.to_string(),
            Err(_) => {
                report.preserved += 1;
                continue;
            }
        };
        if !versioned(&label) {
            if is_dir_nofollow(trust, &name) {
                report.preserved += 1;
            }
            continue;
        }
        match try_reclaim(trust, &name) {
            Ok(true) => report.reclaimed += 1,
            Ok(false) => report.preserved += 1,
            Err(_) => report.preserved += 1,
        }
    }
    Ok(report)
}

pub(super) fn owned_private_dir(st: &libc::stat) -> bool {
    st.st_uid == euid()
        && st.st_mode & libc::S_IFMT == libc::S_IFDIR
        && st.st_mode & 0o7777 == 0o700
}

#[cfg(test)]
pub(super) fn spawn_writer(lease: RawFd, pidfile: &std::path::Path) -> io::Result<Child> {
    let code =
        "import os,sys,time\nopen(sys.argv[1],'w').write(str(os.getpid()))\ntime.sleep(60)\n";
    let mut cmd = Command::new("/usr/bin/python3");
    cmd.arg("-I")
        .arg("-c")
        .arg(code)
        .arg(pidfile)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    unsafe {
        cmd.pre_exec(move || clear_fd(lease));
    }
    cmd.spawn().map_err(|_| invalid("verifier"))
}

#[cfg(test)]
fn clear_fd(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn try_reclaim(trust: RawFd, name: &CString) -> io::Result<bool> {
    let looked = fstat_at(trust, name)?;
    if looked.st_mode & libc::S_IFMT == libc::S_IFLNK || !owned_private_dir(&looked) {
        return Ok(false);
    }
    let dir = open_dir(trust, name)?;
    let opened = fstat(dir.as_raw_fd())?;
    if opened.st_ino != looked.st_ino || !owned_private_dir(&opened) {
        return Ok(false);
    }
    let Some(lease) = open_marker(dir.as_raw_fd())? else {
        return Ok(false);
    };
    let rc = unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) || err.kind() == ErrorKind::WouldBlock {
            return Ok(false);
        }
        return Err(err);
    }
    let locked = fstat(lease.as_raw_fd())?;
    let named = fstat_at(trust, name)?;
    if named.st_dev != opened.st_dev
        || named.st_ino != opened.st_ino
        || !owned_private_dir(&named)
        || !same_lease(
            &locked,
            &fstat_at(dir.as_raw_fd(), &one_component("lease")?)?,
        )
    {
        return Ok(false);
    }
    workspace_seed::remove_tree(trust, name)?;
    drop(lease);
    drop(dir);
    Ok(true)
}

fn same_lease(locked: &libc::stat, named: &libc::stat) -> bool {
    locked.st_dev == named.st_dev && locked.st_ino == named.st_ino && marker_file(locked)
}

fn marker_file(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFREG
        && st.st_nlink == 1
        && st.st_uid == euid()
        && st.st_mode & 0o7777 == 0o600
        && st.st_size == MARKER.len() as i64
}

fn open_marker(dir: RawFd) -> io::Result<Option<OwnedFd>> {
    let name = one_component("lease")?;
    let looked = match fstat_at(dir, &name) {
        Ok(st) => st,
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => return Ok(None),
        Err(err) => return Err(err),
    };
    if looked.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Ok(None);
    }
    let fd = match openat2(
        dir,
        &name,
        libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    ) {
        Ok(fd) => fd,
        Err(err)
            if err.kind() == ErrorKind::NotFound || err.raw_os_error() == Some(libc::ENOENT) =>
        {
            return Ok(None);
        }
        Err(err) => return Err(err),
    };
    let st = fstat(fd.as_raw_fd())?;
    if st.st_dev != looked.st_dev || st.st_ino != looked.st_ino || !marker_file(&st) {
        return Ok(None);
    }
    let mut buf = vec![0u8; MARKER.len()];
    let mut off = 0;
    while off < buf.len() {
        let n = unsafe {
            libc::read(
                fd.as_raw_fd(),
                buf[off..].as_mut_ptr().cast(),
                buf.len() - off,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Ok(None);
        }
        off += n as usize;
    }
    if buf != MARKER {
        return Ok(None);
    }
    Ok(Some(fd))
}

fn write_lease(dir: RawFd) -> io::Result<OwnedFd> {
    let name = one_component("lease")?;
    let fd = openat2(
        dir,
        &name,
        libc::O_RDWR
            | libc::O_CREAT
            | libc::O_EXCL
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK,
        0o600,
    )?;
    let mut off = 0;
    while off < MARKER.len() {
        let n = unsafe {
            libc::write(
                fd.as_raw_fd(),
                MARKER[off..].as_ptr().cast(),
                MARKER.len() - off,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(invalid("lease"));
        }
        off += n as usize;
    }
    let st = fstat(fd.as_raw_fd())?;
    if st.st_nlink != 1
        || st.st_uid != euid()
        || st.st_mode & 0o7777 != 0o600
        || st.st_size != MARKER.len() as i64
    {
        return Err(invalid("lease"));
    }
    if unsafe { libc::flock(fd.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

fn lock_file(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFREG
        && st.st_nlink == 1
        && st.st_uid == euid()
        && st.st_mode & 0o7777 == 0o600
}

fn lock_import(trust: RawFd) -> io::Result<OwnedFd> {
    let name = one_component("import.lock")?;
    match fstat_at(trust, &name) {
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => create_import_lock(trust, &name),
        Err(err) => Err(err),
        Ok(st) if st.st_mode & libc::S_IFMT != libc::S_IFREG => Err(invalid("lease")),
        Ok(st) => open_import_lock(trust, &name, &st),
    }
}

fn create_import_lock(trust: RawFd, name: &CString) -> io::Result<OwnedFd> {
    let fd = match openat2(
        trust,
        name,
        libc::O_RDWR
            | libc::O_CREAT
            | libc::O_EXCL
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK,
        0o600,
    ) {
        Ok(fd) => fd,
        Err(err) if err.raw_os_error() == Some(libc::EEXIST) => {
            let st = fstat_at(trust, name)?;
            if st.st_mode & libc::S_IFMT != libc::S_IFREG {
                return Err(invalid("lease"));
            }
            return open_import_lock(trust, name, &st);
        }
        Err(err) => return Err(err),
    };
    let st = fstat(fd.as_raw_fd())?;
    if !lock_file(&st) {
        return Err(invalid("lease"));
    }
    flock_nb(fd)
}

fn open_import_lock(trust: RawFd, name: &CString, looked: &libc::stat) -> io::Result<OwnedFd> {
    let fd = openat2(
        trust,
        name,
        libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )?;
    let st = fstat(fd.as_raw_fd())?;
    if st.st_dev != looked.st_dev || st.st_ino != looked.st_ino || !lock_file(&st) {
        return Err(invalid("lease"));
    }
    flock_nb(fd)
}

fn flock_nb(fd: OwnedFd) -> io::Result<OwnedFd> {
    let rc = unsafe { libc::flock(fd.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) || err.kind() == ErrorKind::WouldBlock {
            return Err(invalid("lease"));
        }
        return Err(err);
    }
    Ok(fd)
}

fn versioned(name: &str) -> bool {
    pair_after(name, "r") || pair_after(name, "live-")
}

fn pair_after(name: &str, prefix: &str) -> bool {
    let Some(rest) = name.strip_prefix(prefix) else {
        return false;
    };
    let mut parts = rest.split('-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) => {
            !a.is_empty()
                && !b.is_empty()
                && a.bytes().all(|c| c.is_ascii_digit())
                && b.bytes().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

fn is_dir_nofollow(dir: RawFd, name: &CString) -> bool {
    fstat_at(dir, name)
        .map(|st| st.st_mode & libc::S_IFMT == libc::S_IFDIR)
        .unwrap_or(false)
}

fn list_names(dir: RawFd) -> io::Result<Vec<CString>> {
    let dot = CString::new(".").map_err(|_| invalid("lease"))?;
    let owned = openat2(
        dir,
        &dot,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )?;
    let handle = unsafe { libc::fdopendir(owned.into_raw_fd()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut names = Vec::new();
    let listed = (|| {
        while names.len() <= LIST_CAP {
            unsafe { *libc::__errno_location() = 0 };
            let ent = unsafe { libc::readdir(handle) };
            if ent.is_null() {
                if io::Error::last_os_error().raw_os_error() == Some(0) {
                    break;
                }
                return Err(io::Error::last_os_error());
            }
            let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            names.push(name.to_owned());
        }
        Ok(())
    })();
    unsafe { libc::closedir(handle) };
    listed?;
    Ok(names)
}

fn fstat_at(dir: RawFd, name: &CString) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(st)
    }
}

fn dup_fd(fd: RawFd) -> io::Result<OwnedFd> {
    raw_owned(unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) })
}

fn euid() -> libc::uid_t {
    unsafe { libc::geteuid() }
}
