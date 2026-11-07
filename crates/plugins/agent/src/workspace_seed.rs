use std::ffi::CString;
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
#[cfg(test)]
use std::path::Path;

#[cfg(test)]
use super::open_host;
use super::{assert_owned_dir, fstat, invalid, one_component, openat2, raw_owned, Owner};

#[path = "workspace_seed_copy.rs"]
mod copy;

const MOUNTS: [&str; 6] = ["proc", "dev", "work", "tmp", "run", "sys"];

struct LiveCleanup {
    parent: OwnedFd,
    name: CString,
}

pub(crate) struct VerifiedBase {
    root: OwnedFd,
    identity: [u8; 32],
    live: Option<LiveCleanup>,
    /// `fixture` skips the consumer digest. Production seals never set this.
    check_tree: bool,
}

impl Drop for VerifiedBase {
    fn drop(&mut self) {
        if let Some(live) = self.live.take() {
            let _ = copy::remove_partial(live.parent.as_raw_fd(), &live.name);
        }
    }
}

impl VerifiedBase {
    #[cfg(test)]
    pub(crate) fn fixture(root: &Path, identity: [u8; 32]) -> io::Result<Self> {
        Ok(Self {
            root: open_host(root)?,
            identity,
            live: None,
            check_tree: false,
        })
    }

    /// Test-only sealed base. Still checked at seed; not a receipt constructor.
    #[cfg(test)]
    pub(crate) fn checking(root: &Path, identity: [u8; 32]) -> io::Result<Self> {
        Ok(Self {
            root: open_host(root)?,
            identity,
            live: None,
            check_tree: true,
        })
    }

    /// Sealed directory FD plus the approved tree digest. Not a path constructor.
    pub(crate) fn from_sealed(
        root: OwnedFd,
        identity: [u8; 32],
        parent: OwnedFd,
        name: CString,
    ) -> io::Result<Self> {
        let st = fstat(root.as_raw_fd())?;
        if st.st_mode & libc::S_IFMT != libc::S_IFDIR
            || st.st_uid != unsafe { libc::geteuid() }
            || st.st_mode & 0o777 != 0o700
        {
            return Err(invalid("sealed root"));
        }
        Ok(Self {
            root,
            identity,
            live: Some(LiveCleanup { parent, name }),
            check_tree: true,
        })
    }
}

pub(crate) fn copy_tree(source: RawFd, dest: RawFd) -> io::Result<()> {
    copy::populate(source, dest)
}

pub(crate) fn remove_tree(parent: RawFd, name: &CString) -> io::Result<()> {
    copy::remove_partial(parent, name)
}

pub(super) fn ensure_host_outside_base(host: RawFd, base: &VerifiedBase) -> io::Result<()> {
    let base_st = fstat(base.root.as_raw_fd())?;
    let mut cur = dup_fd(host)?;
    for _ in 0..1024 {
        let st = fstat(cur.as_raw_fd())?;
        if st.st_dev == base_st.st_dev && st.st_ino == base_st.st_ino {
            return Err(invalid("host equals or is inside base"));
        }
        let parent = open_parent(cur.as_raw_fd())?;
        let up = fstat(parent.as_raw_fd())?;
        if up.st_dev == st.st_dev && up.st_ino == st.st_ino {
            return Ok(());
        }
        cur = parent;
    }
    Err(invalid("host ancestry bound"))
}

pub(super) fn ensure_rootfs(
    owner_fd: RawFd,
    owner: Owner,
    base: &VerifiedBase,
) -> io::Result<OwnedFd> {
    private_dir(owner_fd)?;
    private_dir(open_dir(owner_fd, &one_component("work")?)?.as_raw_fd())?;
    let stage = one_component(".stage")?;
    if exists(owner_fd, &stage)? {
        copy::remove_partial(owner_fd, &stage)?;
    }
    if exists(owner_fd, &one_component("rootfs")?)? {
        return Err(invalid("legacy rootfs"));
    }
    let published = one_component("generation")?;
    if exists(owner_fd, &published)? {
        return open_published(owner_fd, &published, owner, base);
    }
    seed(owner_fd, &stage, &published, owner, base)
}

fn open_published(
    owner_fd: RawFd,
    name: &CString,
    owner: Owner,
    base: &VerifiedBase,
) -> io::Result<OwnedFd> {
    let generation = open_dir(owner_fd, name)?;
    private_dir(generation.as_raw_fd())?;
    validate_identity(generation.as_raw_fd(), owner, &base.identity)?;
    let rootfs = open_dir(generation.as_raw_fd(), &one_component("rootfs")?)?;
    assert_owned_dir(rootfs.as_raw_fd())?;
    reject_mount_links(rootfs.as_raw_fd())?;
    Ok(rootfs)
}

fn seed(
    owner_fd: RawFd,
    stage: &CString,
    published: &CString,
    owner: Owner,
    base: &VerifiedBase,
) -> io::Result<OwnedFd> {
    if unsafe { libc::mkdirat(owner_fd, stage.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut renamed = false;
    let seeded = (|| {
        let stage_fd = open_dir(owner_fd, stage)?;
        private_dir(stage_fd.as_raw_fd())?;
        if unsafe {
            libc::mkdirat(
                stage_fd.as_raw_fd(),
                one_component("rootfs")?.as_ptr(),
                0o755,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let rootfs = open_dir(stage_fd.as_raw_fd(), &one_component("rootfs")?)?;
        assert_owned_dir(rootfs.as_raw_fd())?;
        copy::populate(base.root.as_raw_fd(), rootfs.as_raw_fd())?;
        // Directory FD does not freeze bytes. Hash the clone before mount dirs exist.
        if base.check_tree {
            let got = super::tree_digest(rootfs.as_raw_fd())?;
            if got != base.identity {
                return Err(invalid("tree"));
            }
        }
        ensure_mounts(rootfs.as_raw_fd())?;
        write_identity(stage_fd.as_raw_fd(), owner, &base.identity)?;
        fsync_fd(rootfs.as_raw_fd())?;
        fsync_fd(stage_fd.as_raw_fd())?;
        drop(stage_fd);
        if unsafe {
            libc::renameat2(
                owner_fd,
                stage.as_ptr(),
                owner_fd,
                published.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        renamed = true;
        fsync_fd(owner_fd)?;
        Ok(rootfs)
    })();
    match seeded {
        Ok(fd) => Ok(fd),
        Err(err) => {
            let victim = if renamed { published } else { stage };
            if let Err(clean) = copy::remove_partial(owner_fd, victim) {
                return Err(io::Error::new(
                    clean.kind(),
                    format!("seed failed ({err}); cleanup failed ({clean})"),
                ));
            }
            Err(err)
        }
    }
}

fn ensure_mounts(rootfs: RawFd) -> io::Result<()> {
    for name in MOUNTS {
        let name = one_component(name)?;
        match fstat_at(rootfs, &name) {
            Err(err) if err.raw_os_error() == Some(libc::ENOENT) => {
                if unsafe { libc::mkdirat(rootfs, name.as_ptr(), 0o755) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(st) if st.st_mode & libc::S_IFMT == libc::S_IFDIR => {}
            Ok(st) if st.st_mode & libc::S_IFMT == libc::S_IFLNK => {
                return Err(invalid("mountpoint link"));
            }
            Ok(_) => return Err(invalid("mountpoint type")),
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn reject_mount_links(rootfs: RawFd) -> io::Result<()> {
    for name in MOUNTS {
        let st = fstat_at(rootfs, &one_component(name)?)?;
        if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(invalid("mountpoint"));
        }
    }
    Ok(())
}

fn write_identity(dir: RawFd, owner: Owner, identity: &[u8; 32]) -> io::Result<()> {
    let bytes = meta_bytes(owner, identity);
    let name = one_component("identity")?;
    let fd = openat2(
        dir,
        &name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o600,
    )?;
    write_full(fd.as_raw_fd(), &bytes)?;
    let st = fstat(fd.as_raw_fd())?;
    if st.st_nlink != 1 || st.st_mode & 0o7777 != 0o600 || st.st_size != bytes.len() as i64 {
        return Err(invalid("metadata"));
    }
    fsync_fd(fd.as_raw_fd())
}

fn validate_identity(dir: RawFd, owner: Owner, identity: &[u8; 32]) -> io::Result<()> {
    let expected = meta_bytes(owner, identity);
    let fd = openat2(
        dir,
        &one_component("identity")?,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )?;
    let st = fstat(fd.as_raw_fd())?;
    if !metadata_file(&st, expected.len()) {
        return Err(invalid("metadata"));
    }
    let mut buf = vec![0u8; expected.len()];
    read_full(fd.as_raw_fd(), &mut buf)?;
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
            return Err(invalid("metadata"));
        }
        break;
    }
    let after = fstat(fd.as_raw_fd())?;
    if !metadata_file(&after, expected.len()) || !same_file_meta(&st, &after) {
        return Err(invalid("metadata"));
    }
    if buf != expected {
        return Err(invalid("metadata"));
    }
    Ok(())
}

fn metadata_file(st: &libc::stat, len: usize) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFREG
        && st.st_nlink == 1
        && st.st_uid == unsafe { libc::geteuid() }
        && st.st_mode & 0o7777 == 0o600
        && st.st_size == len as i64
}

fn same_file_meta(before: &libc::stat, after: &libc::stat) -> bool {
    before.st_mode & (libc::S_IFMT | 0o7777) == after.st_mode & (libc::S_IFMT | 0o7777)
        && before.st_nlink == after.st_nlink
        && before.st_uid == after.st_uid
        && before.st_size == after.st_size
}

fn meta_bytes(owner: Owner, identity: &[u8; 32]) -> Vec<u8> {
    let (kind, id) = match owner {
        Owner::Personal(id) => ("personal", id),
        Owner::Group(id) => ("group", id),
    };
    let mut hex = String::with_capacity(64);
    for byte in identity {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("v1\n{kind}\n{id}\n{hex}\nunaccepted\n").into_bytes()
}

fn private_dir(fd: RawFd) -> io::Result<()> {
    assert_owned_dir(fd)?;
    let st = fstat(fd)?;
    if st.st_mode & 0o7777 != 0o700 {
        return Err(invalid("directory mode"));
    }
    Ok(())
}

fn exists(dir: RawFd, name: &CString) -> io::Result<bool> {
    match fstat_at(dir, name) {
        Ok(_) => Ok(true),
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => Ok(false),
        Err(err) => Err(err),
    }
}

fn open_dir(parent: RawFd, name: &CString) -> io::Result<OwnedFd> {
    openat2(
        parent,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )
}

fn open_parent(dir: RawFd) -> io::Result<OwnedFd> {
    let name = CString::new("..").map_err(|_| invalid(".."))?;
    raw_owned(unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    })
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

fn fsync_fd(fd: RawFd) -> io::Result<()> {
    if unsafe { libc::fsync(fd) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn write_full(fd: RawFd, mut buf: &[u8]) -> io::Result<()> {
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
            return Err(io::Error::new(ErrorKind::WriteZero, "write returned 0"));
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

fn read_full(fd: RawFd, buf: &mut [u8]) -> io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        let n = unsafe { libc::read(fd, buf[off..].as_mut_ptr().cast(), buf.len() - off) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(invalid("metadata short"));
        }
        off += n as usize;
    }
    Ok(())
}
