use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd, RawFd};

use crate::workspace_store::{fstat, invalid, one_component, openat2};

const MAX_DEPTH: u32 = 64;
const MAX_NODES: u64 = 50_000;
const MAX_TOTAL: u64 = 1024 * 1024 * 1024;
const MAX_FILE: u64 = 256 * 1024 * 1024;
const MAX_LINK: usize = 4096;
const CLEAN_DEPTH: u32 = MAX_DEPTH + 8;
const CLEAN_NODES: u64 = MAX_NODES + 4096;

const DIR_FLAGS: i32 = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
const SRC_FILE_FLAGS: i32 = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
const DST_FILE_FLAGS: i32 =
    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW;

struct Budget {
    nodes: u64,
    bytes: u64,
    max_nodes: u64,
    max_bytes: u64,
}

impl Budget {
    fn copy() -> Self {
        Self {
            nodes: 0,
            bytes: 0,
            max_nodes: MAX_NODES,
            max_bytes: MAX_TOTAL,
        }
    }

    fn clean() -> Self {
        Self {
            nodes: 0,
            bytes: 0,
            max_nodes: CLEAN_NODES,
            max_bytes: 0,
        }
    }

    fn charge_node(&mut self) -> io::Result<()> {
        if self.nodes >= self.max_nodes {
            return Err(invalid("node budget"));
        }
        self.nodes += 1;
        Ok(())
    }

    fn charge_file(&mut self, size: u64) -> io::Result<()> {
        if size > MAX_FILE || self.bytes > self.max_bytes || size > self.max_bytes - self.bytes {
            return Err(invalid("byte budget"));
        }
        self.bytes += size;
        Ok(())
    }
}

struct DeferredLink {
    parent: Vec<CString>,
    name: CString,
    target: CString,
}

#[derive(Clone, Copy)]
struct CopyFds {
    src: RawFd,
    dst: RawFd,
}

struct CopyCtx<'a> {
    root: RawFd,
    budget: &'a mut Budget,
    links: &'a mut HashMap<(libc::dev_t, libc::ino_t), Vec<CString>>,
    deferred: &'a mut Vec<DeferredLink>,
}

pub(super) fn populate(source: RawFd, dest: RawFd) -> io::Result<()> {
    let src_st = fstat(source)?;
    let dst_st = fstat(dest)?;
    if !is_dir(&src_st) || !is_dir(&dst_st) {
        return Err(invalid("seed endpoints must be directories"));
    }
    if src_st.st_dev == dst_st.st_dev && src_st.st_ino == dst_st.st_ino {
        return Err(invalid("seed source and dest are the same directory"));
    }
    let mut budget = Budget::copy();
    let mut links: HashMap<(libc::dev_t, libc::ino_t), Vec<CString>> = HashMap::new();
    let mut deferred = Vec::new();
    {
        let mut ctx = CopyCtx {
            root: dest,
            budget: &mut budget,
            links: &mut links,
            deferred: &mut deferred,
        };
        copy_dir(
            CopyFds {
                src: source,
                dst: dest,
            },
            0,
            &[],
            &mut ctx,
        )?;
    }
    for link in deferred {
        let parent = open_rel(dest, &link.parent)?;
        if unsafe { libc::symlinkat(link.target.as_ptr(), parent.as_raw_fd(), link.name.as_ptr()) }
            != 0
        {
            return Err(io::Error::last_os_error());
        }
        fsync_fd(parent.as_raw_fd())?;
    }
    fsync_fd(dest)
}

pub(super) fn remove_partial(parent: RawFd, name: &CString) -> io::Result<()> {
    match fstat_at(parent, name) {
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => Ok(()),
        Err(err) => Err(err),
        Ok(st) => {
            if !is_dir(&st) {
                return Err(invalid("stage is not a directory"));
            }
            require_private_dir(&st)?;
            let mut budget = Budget::clean();
            remove_one(parent, name, 1, &mut budget)?;
            fsync_fd(parent)
        }
    }
}

fn copy_dir(fds: CopyFds, depth: u32, rel: &[CString], ctx: &mut CopyCtx<'_>) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(invalid("depth"));
    }
    let names = list_dir(fds.src, ctx.budget)?;
    for name in names {
        if depth >= MAX_DEPTH {
            return Err(invalid("depth"));
        }
        let st = fstat_at(fds.src, &name)?;
        if has_setid(&st) {
            return Err(invalid("setid"));
        }
        let kind = st.st_mode & libc::S_IFMT;
        if kind == libc::S_IFDIR {
            if unsafe { libc::mkdirat(fds.dst, name.as_ptr(), 0o700) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let src_child = open_dir(fds.src, &name)?;
            let opened = fstat(src_child.as_raw_fd())?;
            if !same_inode(&st, &opened) || !is_dir(&opened) || has_setid(&opened) {
                return Err(invalid("directory changed"));
            }
            let dst_child = open_dir(fds.dst, &name)?;
            let dst_meta = fstat(dst_child.as_raw_fd())?;
            if !is_dir(&dst_meta) || dst_meta.st_uid != euid() || mode_bits(&dst_meta) != 0o700 {
                return Err(invalid("new directory"));
            }
            let mut child_rel = rel.to_vec();
            child_rel.push(name);
            copy_dir(
                CopyFds {
                    src: src_child.as_raw_fd(),
                    dst: dst_child.as_raw_fd(),
                },
                depth + 1,
                &child_rel,
                ctx,
            )?;
            fsync_fd(dst_child.as_raw_fd())?;
        } else if kind == libc::S_IFREG {
            copy_reg(fds, &name, &st, rel, ctx)?;
        } else if kind == libc::S_IFLNK {
            let target = read_link(fds.src, &name)?;
            ctx.deferred.push(DeferredLink {
                parent: rel.to_vec(),
                name,
                target,
            });
        } else {
            return Err(invalid("special file"));
        }
    }
    fsync_fd(fds.dst)
}

fn copy_reg(
    fds: CopyFds,
    name: &CString,
    looked: &libc::stat,
    rel: &[CString],
    ctx: &mut CopyCtx<'_>,
) -> io::Result<()> {
    let size = file_size(looked)?;
    let key = (looked.st_dev, looked.st_ino);
    if let Some(existing) = ctx.links.get(&key).cloned() {
        confirm_source_reg(fds.src, name, looked)?;
        link_alias(ctx.root, &existing, rel, name)?;
        return Ok(());
    }
    ctx.budget.charge_file(size)?;
    let src = openat2(fds.src, name, SRC_FILE_FLAGS, 0)?;
    let before = fstat(src.as_raw_fd())?;
    if !same_inode(looked, &before)
        || !is_reg(&before)
        || has_setid(&before)
        || file_size(&before)? != size
    {
        return Err(invalid("source file changed"));
    }
    let mode = if before.st_mode & 0o111 != 0 {
        0o700
    } else {
        0o600
    };
    let dst = openat2(fds.dst, name, DST_FILE_FLAGS, mode)?;
    let created = fstat(dst.as_raw_fd())?;
    if !is_reg(&created)
        || created.st_nlink != 1
        || mode_bits(&created) != mode
        || created.st_size != 0
    {
        return Err(invalid("new file"));
    }
    copy_exact(src.as_raw_fd(), dst.as_raw_fd(), size)?;
    let after = fstat(src.as_raw_fd())?;
    if !same_inode(&before, &after)
        || !is_reg(&after)
        || file_size(&after)? != size
        || has_setid(&after)
    {
        return Err(invalid("source file changed"));
    }
    let wrote = fstat(dst.as_raw_fd())?;
    if !is_reg(&wrote) || wrote.st_size as u64 != size || wrote.st_nlink < 1 {
        return Err(invalid("copied file"));
    }
    fsync_fd(dst.as_raw_fd())?;
    let mut path = rel.to_vec();
    path.push(name.clone());
    ctx.links.insert(key, path);
    Ok(())
}

fn confirm_source_reg(dir: RawFd, name: &CString, looked: &libc::stat) -> io::Result<()> {
    let fd = openat2(dir, name, SRC_FILE_FLAGS, 0)?;
    let st = fstat(fd.as_raw_fd())?;
    if !same_inode(looked, &st) || !is_reg(&st) || has_setid(&st) {
        return Err(invalid("hardlink source"));
    }
    Ok(())
}

fn link_alias(
    root: RawFd,
    existing: &[CString],
    new_parent_rel: &[CString],
    new_name: &CString,
) -> io::Result<()> {
    let Some((old_name, old_parent_rel)) = existing.split_last() else {
        return Err(invalid("hardlink path"));
    };
    let old_parent = open_rel(root, old_parent_rel)?;
    let old_st = fstat_at(old_parent.as_raw_fd(), old_name)?;
    if !is_reg(&old_st) {
        return Err(invalid("hardlink target"));
    }
    let new_parent = open_rel(root, new_parent_rel)?;
    if unsafe {
        libc::linkat(
            old_parent.as_raw_fd(),
            old_name.as_ptr(),
            new_parent.as_raw_fd(),
            new_name.as_ptr(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    fsync_fd(old_parent.as_raw_fd())?;
    fsync_fd(new_parent.as_raw_fd())?;
    Ok(())
}

fn open_rel(root: RawFd, comps: &[CString]) -> io::Result<OwnedFd> {
    let mut cur = open_dir(root, &dot())?;
    for c in comps {
        cur = open_dir(cur.as_raw_fd(), c)?;
    }
    Ok(cur)
}

fn copy_exact(src: RawFd, dst: RawFd, size: u64) -> io::Result<()> {
    let mut left = size;
    let mut buf = [0u8; 64 * 1024];
    while left > 0 {
        let want = std::cmp::min(left, buf.len() as u64) as usize;
        let n = read_chunk(src, &mut buf[..want])?;
        if n == 0 {
            return Err(invalid("source truncated"));
        }
        write_chunk(dst, &buf[..n])?;
        left -= n as u64;
    }
    Ok(())
}

fn read_chunk(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(n as usize);
    }
}

fn write_chunk(fd: RawFd, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(invalid("short write"));
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

fn read_link(dir: RawFd, name: &CString) -> io::Result<CString> {
    let mut buf = [0u8; MAX_LINK + 1];
    let n = unsafe { libc::readlinkat(dir, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let n = n as usize;
    if n > MAX_LINK {
        return Err(invalid("symlink length"));
    }
    CString::new(&buf[..n]).map_err(|_| invalid("symlink target"))
}

fn list_dir(dirfd: RawFd, budget: &mut Budget) -> io::Result<Vec<CString>> {
    let independent = open_dir(dirfd, &dot())?;
    let raw = independent.into_raw_fd();
    let dir = unsafe { libc::fdopendir(raw) };
    if dir.is_null() {
        unsafe { libc::close(raw) };
        return Err(io::Error::last_os_error());
    }
    let mut names = Vec::new();
    let listed = (|| loop {
        unsafe { *libc::__errno_location() = 0 };
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(0) {
                return Ok(());
            }
            return Err(err);
        }
        let bytes = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if bytes.len() > 255 {
            return Err(invalid("directory entry"));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| invalid("directory entry"))?;
        budget.charge_node()?;
        names.push(one_component(text)?);
    })();
    let closed = unsafe { libc::closedir(dir) };
    listed?;
    if closed != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(names)
}

fn remove_one(parent: RawFd, name: &CString, depth: u32, budget: &mut Budget) -> io::Result<()> {
    remove_at(parent, name, depth, budget, true)
}

fn remove_at(
    parent: RawFd,
    name: &CString,
    depth: u32,
    budget: &mut Budget,
    charge: bool,
) -> io::Result<()> {
    if depth == 0 || depth > CLEAN_DEPTH {
        return Err(invalid("cleanup depth"));
    }
    if charge {
        budget.charge_node()?;
    }
    let st = fstat_at(parent, name)?;
    let kind = st.st_mode & libc::S_IFMT;
    if kind == libc::S_IFDIR {
        require_private_dir(&st)?;
        let dir = openat2(parent, name, DIR_FLAGS | libc::O_NONBLOCK, 0)?;
        let opened = fstat(dir.as_raw_fd())?;
        if !same_inode(&st, &opened) || !is_dir(&opened) {
            return Err(invalid("cleanup directory changed"));
        }
        require_private_dir(&opened)?;
        let children = list_dir(dir.as_raw_fd(), budget)?;
        for child in children {
            remove_at(dir.as_raw_fd(), &child, depth + 1, budget, false)?;
        }
        fsync_fd(dir.as_raw_fd())?;
        if unsafe { libc::unlinkat(parent, name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(io::Error::last_os_error());
        }
        return Ok(());
    }
    if kind == libc::S_IFREG
        || kind == libc::S_IFLNK
        || kind == libc::S_IFIFO
        || kind == libc::S_IFSOCK
        || kind == libc::S_IFCHR
        || kind == libc::S_IFBLK
    {
        if unsafe { libc::unlinkat(parent, name.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        return Ok(());
    }
    Err(invalid("cleanup object"))
}

fn require_private_dir(st: &libc::stat) -> io::Result<()> {
    if !is_dir(st) || st.st_uid != euid() || st.st_mode & (libc::S_IWGRP | libc::S_IWOTH) != 0 {
        return Err(invalid("cleanup directory"));
    }
    Ok(())
}

fn open_dir(parent: RawFd, name: &CString) -> io::Result<OwnedFd> {
    openat2(parent, name, DIR_FLAGS, 0)
}

fn fstat_at(dir: RawFd, name: &CString) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(st)
    }
}

fn fsync_fd(fd: RawFd) -> io::Result<()> {
    if unsafe { libc::fsync(fd) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn file_size(st: &libc::stat) -> io::Result<u64> {
    if st.st_size < 0 {
        return Err(invalid("file size"));
    }
    Ok(st.st_size as u64)
}

fn is_dir(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFDIR
}

fn is_reg(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFREG
}

fn has_setid(st: &libc::stat) -> bool {
    st.st_mode & (libc::S_ISUID | libc::S_ISGID) != 0
}

fn same_inode(a: &libc::stat, b: &libc::stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}

fn mode_bits(st: &libc::stat) -> u32 {
    st.st_mode & 0o777
}

fn euid() -> libc::uid_t {
    unsafe { libc::geteuid() }
}

fn dot() -> CString {
    CString::new(".").unwrap()
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use super::*;

    #[test]
    fn budget_exact_bounds() {
        let mut b = Budget::copy();
        for _ in 0..4 {
            b.charge_file(MAX_FILE).unwrap();
        }
        assert!(b.charge_file(1).is_err());
        assert!(Budget::copy().charge_file(MAX_FILE + 1).is_err());
        let mut nodes = Budget::copy();
        for _ in 0..MAX_NODES {
            nodes.charge_node().unwrap();
        }
        assert!(nodes.charge_node().is_err());
    }

    #[test]
    fn nested_wide_tree_shares_node_budget() {
        let root = std::env::temp_dir().join(format!("seed-copy-nodes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nest/a")).unwrap();
        std::fs::create_dir_all(root.join("nest/b")).unwrap();
        std::fs::create_dir_all(root.join("wide")).unwrap();
        let top = std::fs::File::open(&root).unwrap();
        let mut budget = Budget {
            nodes: 0,
            bytes: 0,
            max_nodes: 3,
            max_bytes: MAX_TOTAL,
        };
        let names = list_dir(top.as_raw_fd(), &mut budget).unwrap();
        assert_eq!(names.len(), 2);
        assert_eq!(budget.nodes, 2);
        let nest = std::fs::File::open(root.join("nest")).unwrap();
        let err = list_dir(nest.as_raw_fd(), &mut budget).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(budget.nodes, 3);
        let _ = std::fs::remove_dir_all(&root);
    }
}
