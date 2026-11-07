use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};

use sha2::{Digest, Sha256};

use super::{fstat, invalid, openat2};

const POLICY: &[u8] = b"workspace-rootfs-runtime-v2\0";
const MOUNTS: [&[u8]; 6] = [b"proc", b"dev", b"work", b"tmp", b"run", b"sys"];
const MAX_DEPTH: u32 = 64;
const MAX_NODES: u64 = 50_000;
const MAX_FILE: u64 = 256 * 1024 * 1024;
const MAX_TOTAL: u64 = 1024 * 1024 * 1024;
const MAX_PATH: usize = 4096;
const MAX_TARGET: usize = 4096;

struct FileEnt {
    perm: u16,
    size: u64,
    sha: [u8; 32],
    nlink: u64,
    paths: Vec<Vec<u8>>,
}

pub(super) fn digest(root: RawFd) -> io::Result<[u8; 32]> {
    digest_limited(root, MAX_NODES)
}

fn digest_limited(root: RawFd, nodes: u64) -> io::Result<[u8; 32]> {
    let st = fstat(root)?;
    if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(invalid("tree"));
    }
    let mut rows = Vec::new();
    let mut files: HashMap<(u64, u64), FileEnt> = HashMap::new();
    let mut left = nodes;
    let mut total = 0u64;
    walk(root, b"", 0, &mut left, &mut total, &mut files, &mut rows)?;
    for ent in files.values_mut() {
        ent.paths.sort();
        if ent.paths.len() as u64 != ent.nlink {
            return Err(invalid("inode share"));
        }
        let first = &ent.paths[0];
        let mut extra = Vec::with_capacity(2 + 8 + 32);
        extra.extend_from_slice(&ent.perm.to_be_bytes());
        extra.extend_from_slice(&ent.size.to_be_bytes());
        extra.extend_from_slice(&ent.sha);
        rows.push(frame(b'f', first, &extra));
        let mut alias = Vec::with_capacity(4 + first.len());
        alias.extend_from_slice(&(first.len() as u32).to_be_bytes());
        alias.extend_from_slice(first);
        for path in ent.paths.iter().skip(1) {
            rows.push(frame(b'h', path, &alias));
        }
    }
    rows.sort_by(|a, b| path_of(a).cmp(path_of(b)));
    let mut hasher = Sha256::new();
    hasher.update(POLICY);
    for rec in &rows {
        hasher.update(rec);
    }
    Ok(hasher.finalize().into())
}

fn walk(
    dir: RawFd,
    prefix: &[u8],
    depth: u32,
    left: &mut u64,
    total: &mut u64,
    files: &mut HashMap<(u64, u64), FileEnt>,
    rows: &mut Vec<Vec<u8>>,
) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(invalid("depth"));
    }
    let names = list_names(dir, left)?;
    if depth == MAX_DEPTH && !names.is_empty() {
        return Err(invalid("depth"));
    }
    for name in names {
        let path = join_path(prefix, name.as_bytes())?;
        let st = fstat_at(dir, &name)?;
        let mode = st.st_mode;
        let kind = mode & libc::S_IFMT;
        if path.len() > MAX_PATH
            || st.st_uid != euid()
            || (depth == 0 && MOUNTS.contains(&name.as_bytes()) && kind != libc::S_IFDIR)
        {
            return Err(invalid("tree"));
        }
        if kind == libc::S_IFLNK {
            let target = read_link(dir, &name)?;
            if bad_bytes(&target, false)
                || target.len() > MAX_TARGET
                || link_escapes(depth, &target)
            {
                return Err(invalid("link"));
            }
            let mut extra = Vec::with_capacity(4 + target.len());
            extra.extend_from_slice(&(target.len() as u32).to_be_bytes());
            extra.extend_from_slice(&target);
            rows.push(frame(b's', &path, &extra));
            continue;
        }
        if kind == libc::S_IFDIR {
            let perm = ordinary_mode(mode, true)?;
            rows.push(frame(b'd', &path, &perm.to_be_bytes()));
            let child = openat2(
                dir,
                &name,
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                0,
            )?;
            walk(
                child.as_raw_fd(),
                &path,
                depth + 1,
                left,
                total,
                files,
                rows,
            )?;
            continue;
        }
        let size = u64::try_from(st.st_size).map_err(|_| invalid("size"))?;
        if kind != libc::S_IFREG || size > MAX_FILE {
            return Err(invalid("tree"));
        }
        let perm = ordinary_mode(mode, false)?;
        let key = (st.st_dev as u64, st.st_ino as u64);
        if let Some(ent) = files.get_mut(&key) {
            if ent.size != size || ent.nlink != st.st_nlink as u64 {
                return Err(invalid("inode share"));
            }
            ent.paths.push(path);
        } else {
            if *total > MAX_TOTAL - size {
                return Err(invalid("tree"));
            }
            *total += size;
            let sha = hash_file(dir, &name, &st, size)?;
            files.insert(
                key,
                FileEnt {
                    perm,
                    size,
                    sha,
                    nlink: st.st_nlink as u64,
                    paths: vec![path],
                },
            );
        }
    }
    Ok(())
}

fn list_names(dir: RawFd, left: &mut u64) -> io::Result<Vec<CString>> {
    let listed = openat2(
        dir,
        &CString::new(".").map_err(|_| invalid("name"))?,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )?;
    let raw = listed.into_raw_fd();
    let handle = unsafe { libc::fdopendir(raw) };
    if handle.is_null() {
        unsafe { libc::close(raw) };
        return Err(io::Error::last_os_error());
    }
    let mut names = Vec::new();
    let read = (|| {
        loop {
            unsafe { *libc::__errno_location() = 0 };
            let ent = unsafe { libc::readdir(handle) };
            if ent.is_null() {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(0) {
                    break;
                }
                return Err(err);
            }
            let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
            let bytes = name.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if *left == 0 || bad_bytes(bytes, true) {
                return Err(invalid("node budget"));
            }
            *left -= 1;
            names.push(name.to_owned());
        }
        Ok(())
    })();
    unsafe { libc::closedir(handle) };
    read?;
    names.sort();
    Ok(names)
}

fn hash_file(dir: RawFd, name: &CString, want: &libc::stat, size: u64) -> io::Result<[u8; 32]> {
    let fd = openat2(
        dir,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )?;
    let pre = fstat(fd.as_raw_fd())?;
    if pre.st_mode & libc::S_IFMT != libc::S_IFREG
        || pre.st_size as u64 != size
        || pre.st_ino != want.st_ino
        || pre.st_dev != want.st_dev
    {
        return Err(invalid("tree"));
    }
    let mut hasher = Sha256::new();
    let mut left = size;
    let mut buf = [0u8; 8192];
    while left > 0 {
        let want_n = buf.len().min(left as usize);
        let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), want_n) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(invalid("tree"));
        }
        hasher.update(&buf[..n as usize]);
        left -= n as u64;
    }
    let mut extra = [0u8; 1];
    loop {
        let n = unsafe { libc::read(fd.as_raw_fd(), extra.as_mut_ptr().cast(), 1) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n != 0 {
            return Err(invalid("tree"));
        }
        break;
    }
    let post = fstat(fd.as_raw_fd())?;
    if post.st_ino != pre.st_ino || post.st_size != pre.st_size {
        return Err(invalid("tree"));
    }
    Ok(hasher.finalize().into())
}

fn read_link(dir: RawFd, name: &CString) -> io::Result<Vec<u8>> {
    let mut buf = [0u8; MAX_TARGET + 1];
    let n = unsafe { libc::readlinkat(dir, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let n = n as usize;
    if n == 0 || n > MAX_TARGET {
        return Err(invalid("link"));
    }
    Ok(buf[..n].to_vec())
}

fn join_path(prefix: &[u8], name: &[u8]) -> io::Result<Vec<u8>> {
    let mut path = Vec::with_capacity(prefix.len() + 1 + name.len());
    if !prefix.is_empty() {
        path.extend_from_slice(prefix);
        path.push(b'/');
    }
    path.extend_from_slice(name);
    if path.len() > MAX_PATH {
        return Err(invalid("path"));
    }
    Ok(path)
}

fn frame(kind: u8, path: &[u8], extra: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 4 + path.len() + extra.len());
    out.push(kind);
    out.extend_from_slice(&(path.len() as u32).to_be_bytes());
    out.extend_from_slice(path);
    out.extend_from_slice(extra);
    out
}

fn path_of(rec: &[u8]) -> &[u8] {
    let n = u32::from_be_bytes([rec[1], rec[2], rec[3], rec[4]]) as usize;
    &rec[5..5 + n]
}

fn bad_bytes(buf: &[u8], slash: bool) -> bool {
    buf.is_empty()
        || buf
            .iter()
            .any(|b| *b < 32 || *b == 127 || *b == b'\\' || (slash && *b == b'/'))
}

fn link_escapes(depth: u32, target: &[u8]) -> bool {
    if target.first() == Some(&b'/') {
        return false;
    }
    let mut d = depth;
    for part in target.split(|b| *b == b'/') {
        if part.is_empty() || part == b"." {
            continue;
        }
        if part == b".." {
            if d == 0 {
                return true;
            }
            d -= 1;
        } else {
            d += 1;
        }
    }
    false
}

fn fstat_at(dir: RawFd, name: &CString) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

fn ordinary_mode(mode: u32, dir: bool) -> io::Result<u16> {
    if mode & (libc::S_ISUID | libc::S_ISGID) != 0 {
        return Err(invalid("mode"));
    }
    let perm = mode & 0o7777;
    if dir {
        if perm & 0o500 != 0o500 {
            return Err(invalid("mode"));
        }
        return Ok((perm & 0o1777) as u16);
    }
    if perm & 0o1000 != 0 || perm & 0o400 == 0 {
        return Err(invalid("mode"));
    }
    Ok((perm & 0o777) as u16)
}

fn euid() -> libc::uid_t {
    unsafe { libc::geteuid() }
}

#[cfg(test)]
#[path = "workspace_tree_tests.rs"]
mod tests;
