use std::ffi::CString;
use std::io::{self, ErrorKind};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Component, Path, PathBuf};

const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;
const RESOLVE: u64 = RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS | RESOLVE_BENEATH;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Personal(u64),
    Group(u64),
}

impl Owner {
    pub fn personal(qq: u64) -> io::Result<Self> {
        (qq != 0)
            .then_some(Self::Personal(qq))
            .ok_or_else(|| invalid("qq must be positive"))
    }

    pub fn group(id: u64) -> io::Result<Self> {
        (id != 0)
            .then_some(Self::Group(id))
            .ok_or_else(|| invalid("group id must be positive"))
    }
}

#[derive(Debug)]
pub struct WorkspaceStore {
    work: OwnedFd,
    rootfs: OwnedFd,
    _lock: OwnedFd,
}

impl AsFd for WorkspaceStore {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.work.as_fd()
    }
}

impl WorkspaceStore {
    pub fn open(host: &Path, owner: Owner, base: &VerifiedBase) -> io::Result<Self> {
        let (ns, id_s, id) = match owner {
            Owner::Personal(id) => ("personal", id.to_string(), id),
            Owner::Group(id) => ("group", id.to_string(), id),
        };
        if id == 0 {
            return Err(invalid("id must be positive"));
        }
        let host_fd = open_host(host)?;
        workspace_seed::ensure_host_outside_base(host_fd.as_raw_fd(), base)?;
        let ns_fd = subdir(host_fd.as_raw_fd(), ns)?;
        let owner_fd = subdir(ns_fd.as_raw_fd(), &id_s)?;
        let _lock = lock_owner(owner_fd.as_raw_fd())?;
        let work = subdir(owner_fd.as_raw_fd(), "work")?;
        let rootfs = workspace_seed::ensure_rootfs(owner_fd.as_raw_fd(), owner, base)?;
        Ok(Self {
            work,
            rootfs,
            _lock,
        })
    }

    pub fn rootfs(&self) -> BorrowedFd<'_> {
        self.rootfs.as_fd()
    }
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, msg)
}

fn open_host(host: &Path) -> io::Result<OwnedFd> {
    if !host.is_absolute() {
        return Err(invalid("host path must be absolute"));
    }
    let mut rel = String::new();
    for c in host.components() {
        match c {
            Component::RootDir => {}
            Component::Normal(s) => {
                let s = s
                    .to_str()
                    .ok_or_else(|| invalid("host path must be utf-8"))?;
                if !rel.is_empty() {
                    rel.push('/');
                }
                rel.push_str(s);
            }
            _ => return Err(invalid("host path rejects . .. and prefixes")),
        }
    }
    if rel.is_empty() {
        return Err(invalid("host path rejects filesystem root"));
    }
    let root = raw_owned(unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })?;
    let fd = openat2(
        root.as_raw_fd(),
        &CString::new(rel).map_err(|_| invalid("host path nul"))?,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        0,
    )?;
    assert_owned_dir(fd.as_raw_fd())?;
    Ok(fd)
}

fn subdir(parent: RawFd, name: &str) -> io::Result<OwnedFd> {
    let name = one_component(name)?;
    if unsafe { libc::mkdirat(parent, name.as_ptr(), 0o700) } != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EEXIST) {
            return Err(err);
        }
    }
    let fd = openat2(
        parent,
        &name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )?;
    assert_owned_dir(fd.as_raw_fd())?;
    Ok(fd)
}

fn assert_owned_dir(fd: RawFd) -> io::Result<()> {
    let st = fstat(fd)?;
    if (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
        return Err(invalid("expected directory"));
    }
    if st.st_uid != unsafe { libc::geteuid() } {
        return Err(invalid("directory not owned by current uid"));
    }
    if st.st_mode & (libc::S_IWGRP | libc::S_IWOTH) != 0 {
        return Err(invalid("directory is group or world writable"));
    }
    Ok(())
}

fn one_component(name: &str) -> io::Result<CString> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.as_bytes().iter().any(|b| *b == b'/' || *b == 0)
    {
        return Err(invalid("child must be one normal path component"));
    }
    CString::new(name).map_err(|_| invalid("child nul"))
}

fn openat2(dirfd: RawFd, path: &CString, flags: i32, mode: u32) -> io::Result<OwnedFd> {
    let mut how = OpenHow {
        flags: flags as u64,
        mode: mode as u64,
        resolve: RESOLVE,
    };
    let rc = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &mut how as *mut OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if rc < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOSYS) {
            return Err(io::Error::new(
                ErrorKind::Unsupported,
                "openat2 unsupported",
            ));
        }
        return Err(err);
    }
    Ok(unsafe { OwnedFd::from_raw_fd(rc as RawFd) })
}

fn raw_owned(fd: RawFd) -> io::Result<OwnedFd> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

fn lock_owner(dir: RawFd) -> io::Result<OwnedFd> {
    let name = one_component(".lock")?;
    let mut pre: libc::stat = unsafe { std::mem::zeroed() };
    let pre_rc = unsafe { libc::fstatat(dir, name.as_ptr(), &mut pre, libc::AT_SYMLINK_NOFOLLOW) };
    let flags = libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    let fd = if pre_rc != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ENOENT) {
            return Err(err);
        }
        raw_owned(unsafe {
            libc::openat(
                dir,
                name.as_ptr(),
                flags | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )
        })?
    } else {
        reject_lock_stat(&pre)?;
        raw_owned(unsafe { libc::openat(dir, name.as_ptr(), flags, 0) })?
    };
    let st = fstat(fd.as_raw_fd())?;
    reject_lock_stat(&st)?;
    if unsafe { libc::flock(fd.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

fn reject_lock_stat(st: &libc::stat) -> io::Result<()> {
    if (st.st_mode & libc::S_IFMT) != libc::S_IFREG
        || st.st_nlink != 1
        || st.st_mode & 0o7777 != 0o600
    {
        return Err(invalid("lock must be a regular file with nlink 1"));
    }
    if st.st_uid != unsafe { libc::geteuid() } {
        return Err(invalid("lock not owned by current uid"));
    }
    Ok(())
}

#[path = "workspace_tree.rs"]
mod tree;

fn tree_digest(root: RawFd) -> io::Result<[u8; 32]> {
    tree::digest(root)
}

#[path = "workspace_seed.rs"]
mod workspace_seed;

#[cfg(test)]
#[path = "workspace_seed_tests.rs"]
mod workspace_seed_tests;

#[path = "workspace_acceptance.rs"]
mod acceptance;

pub(crate) fn acquire_accepted(trust: &Path) -> io::Result<(PathBuf, VerifiedBase)> {
    acceptance::acquire(trust)
}

pub(crate) use workspace_seed::VerifiedBase;
