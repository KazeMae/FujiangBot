use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, RawFd};

use super::{
    after_lock_hook, before_tmp_hook, fstat, fstat_at, open_at, unavailable, OPEN_DIR, STAGE_MARK,
};

pub(super) fn reclaim(dir: RawFd, uid: u32) -> io::Result<()> {
    for name in dir_names(dir, 8192)? {
        let Some(id) = hold_id(&name).map(str::to_string) else {
            continue;
        };
        let c_name = CString::new(name).map_err(|_| unavailable("scan"))?;
        let _ = reclaim_hold(dir, &c_name, &id, uid)?;
    }
    Ok(())
}

fn hold_id(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(".resolv.")?.strip_suffix(".hold")?;
    let (pid, seq) = rest.split_once('.')?;
    if plain_id(pid) && plain_id(seq) {
        Some(rest)
    } else {
        None
    }
}

fn plain_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 20
        && bytes.iter().all(|b| b.is_ascii_digit())
        && (bytes.len() == 1 || bytes[0] != b'0')
}

pub(super) fn dir_names(dir: RawFd, limit: usize) -> io::Result<Vec<String>> {
    let fd = unsafe { libc::openat(dir, c".".as_ptr(), OPEN_DIR) };
    if fd < 0 {
        return Err(unavailable("scan"));
    }
    let dirp = unsafe { libc::fdopendir(fd) };
    if dirp.is_null() {
        let _ = unsafe { libc::close(fd) };
        return Err(unavailable("scan"));
    }
    let mut names = Vec::new();
    let mut seen = 0usize;
    let result = loop {
        unsafe { *libc::__errno_location() = 0 };
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            if unsafe { *libc::__errno_location() } != 0 {
                break Err(unavailable("scan"));
            }
            break Ok(names);
        }
        let raw = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        let bytes = raw.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        seen += 1;
        if seen > limit {
            break Err(unavailable("scan"));
        }
        let Ok(text) = raw.to_str() else {
            continue;
        };
        names.push(text.to_string());
    };
    unsafe { libc::closedir(dirp) };
    result
}

fn reclaim_hold(dir: RawFd, name: &CStr, id: &str, uid: u32) -> io::Result<bool> {
    let fd = match open_at(
        dir,
        name,
        libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
    ) {
        Ok(fd) => fd,
        Err(_) => return Ok(false),
    };
    let opened = fstat(fd.as_raw_fd())?;
    if !marked_hold(&opened, fd.as_raw_fd(), uid)? {
        return Ok(false);
    }
    if unsafe { libc::flock(fd.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(false);
    }
    after_lock_hook();
    let named = fstat_at(dir, name, libc::AT_SYMLINK_NOFOLLOW)?;
    if named.st_dev != opened.st_dev || named.st_ino != opened.st_ino || named.st_nlink != 1 {
        return Ok(false);
    }
    let tmp = CString::new(format!(".resolv.{id}.tmp")).map_err(|_| unavailable("gc"))?;
    match reclaim_tmp(dir, &tmp, uid)? {
        TmpEnd::Absent | TmpEnd::Removed => {}
        TmpEnd::Refused => return Ok(false),
    }
    let again = fstat_at(dir, name, libc::AT_SYMLINK_NOFOLLOW)?;
    if again.st_dev != opened.st_dev || again.st_ino != opened.st_ino || again.st_nlink != 1 {
        return Ok(false);
    }
    if unsafe { libc::unlinkat(dir, name.as_ptr(), 0) } != 0 {
        return Err(unavailable("gc"));
    }
    Ok(true)
}

enum TmpEnd {
    Absent,
    Removed,
    Refused,
}

fn marked_hold(st: &libc::stat, fd: RawFd, uid: u32) -> io::Result<bool> {
    if st.st_mode & libc::S_IFMT != libc::S_IFREG || st.st_nlink != 1 || st.st_uid != uid {
        return Ok(false);
    }
    let mut buf = [0u8; STAGE_MARK.len() + 1];
    let n = unsafe { libc::pread(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(n as usize == STAGE_MARK.len() && &buf[..STAGE_MARK.len()] == STAGE_MARK)
}

fn reclaim_tmp(dir: RawFd, name: &CStr, uid: u32) -> io::Result<TmpEnd> {
    let fd = match open_at(
        dir,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
    ) {
        Ok(fd) => fd,
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => return Ok(TmpEnd::Absent),
        Err(_) => return Ok(TmpEnd::Refused),
    };
    let opened = fstat(fd.as_raw_fd())?;
    if opened.st_mode & libc::S_IFMT != libc::S_IFREG
        || opened.st_nlink != 1
        || opened.st_uid != uid
    {
        return Ok(TmpEnd::Refused);
    }
    before_tmp_hook();
    let named = fstat_at(dir, name, libc::AT_SYMLINK_NOFOLLOW)?;
    if named.st_dev != opened.st_dev || named.st_ino != opened.st_ino || named.st_nlink != 1 {
        return Ok(TmpEnd::Refused);
    }
    if unsafe { libc::unlinkat(dir, name.as_ptr(), 0) } != 0 {
        return Ok(TmpEnd::Refused);
    }
    Ok(TmpEnd::Removed)
}
