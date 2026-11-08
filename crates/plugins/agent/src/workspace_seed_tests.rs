use std::fs::{self, File};
use std::io::ErrorKind;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{Owner, PublicRuntime, WorkspaceStore};

const DIGEST: [u8; 32] = [0x11; 32];
const TOOL: &[u8] = b"tool-bytes";
const SPARSE: u64 = 256 * 1024 * 1024 + 1;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = N.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("ws-seed-{n}-{id}"));
    fs::create_dir(&root).unwrap();
    mode(&root, 0o700);
    Scratch(root)
}

fn mode(path: &Path, bits: u32) {
    let mut perms = fs::symlink_metadata(path).unwrap().permissions();
    perms.set_mode(bits);
    fs::set_permissions(path, perms).unwrap();
}

fn dir(path: &Path) {
    fs::create_dir(path).unwrap();
    mode(path, 0o700);
}

fn cstr(text: &str) -> std::ffi::CString {
    std::ffi::CString::new(text).unwrap()
}

fn open_rel(dirfd: RawFd, rel: &str, flags: i32) -> OwnedFd {
    let mut cur = unsafe { libc::dup(dirfd) };
    assert!(cur >= 0);
    let parts: Vec<&str> = rel.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        let flags = if i + 1 == parts.len() {
            flags
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
        };
        let next = unsafe { libc::openat(cur, cstr(part).as_ptr(), flags) };
        unsafe { libc::close(cur) };
        assert!(next >= 0, "openat {rel}");
        cur = next;
    }
    unsafe { OwnedFd::from_raw_fd(cur) }
}

fn parent_of(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(i) => (&rel[..i], &rel[i + 1..]),
        None => ("", rel),
    }
}

fn held(dirfd: RawFd, rel: &str) -> OwnedFd {
    let (parent, _) = parent_of(rel);
    if parent.is_empty() {
        unsafe { OwnedFd::from_raw_fd(libc::dup(dirfd)) }
    } else {
        open_rel(
            dirfd,
            parent,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    }
}

fn stat_nofollow(dirfd: RawFd, rel: &str) -> libc::stat {
    let parent = held(dirfd, rel);
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            cstr(parent_of(rel).1).as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    assert_eq!(rc, 0);
    st
}

fn kind(dirfd: RawFd, rel: &str) -> u32 {
    stat_nofollow(dirfd, rel).st_mode & libc::S_IFMT
}

fn ino(dirfd: RawFd, rel: &str) -> u64 {
    stat_nofollow(dirfd, rel).st_ino
}

fn link_target(dirfd: RawFd, rel: &str) -> String {
    let parent = held(dirfd, rel);
    let mut buf = vec![0u8; 512];
    let n = unsafe {
        libc::readlinkat(
            parent.as_raw_fd(),
            cstr(parent_of(rel).1).as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    assert!(n > 0);
    buf.truncate(n as usize);
    String::from_utf8(buf).unwrap()
}

fn read_reg(dirfd: RawFd, rel: &str) -> Vec<u8> {
    let fd = open_rel(
        dirfd,
        rel,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    );
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        assert!(n >= 0);
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    out
}

fn write_reg(dirfd: RawFd, rel: &str, data: &[u8]) {
    let fd = open_rel(
        dirfd,
        rel,
        libc::O_WRONLY | libc::O_TRUNC | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    );
    let mut rest = data;
    while !rest.is_empty() {
        let n = unsafe { libc::write(fd.as_raw_fd(), rest.as_ptr().cast(), rest.len()) };
        assert!(n > 0);
        rest = &rest[n as usize..];
    }
}

fn create_reg(dirfd: RawFd, name: &str, data: &[u8]) {
    let fd = unsafe {
        libc::openat(
            dirfd,
            cstr(name).as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    assert!(fd >= 0);
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    assert_eq!(
        unsafe { libc::write(fd.as_raw_fd(), data.as_ptr().cast(), data.len()) },
        data.len() as isize
    );
}

fn entries(dirfd: RawFd, rel: &str) -> Vec<String> {
    let fd = open_rel(
        dirfd,
        rel,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    );
    let dirp = unsafe { libc::fdopendir(libc::dup(fd.as_raw_fd())) };
    assert!(!dirp.is_null());
    let mut names = Vec::new();
    loop {
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) }
            .to_str()
            .unwrap();
        if name != "." && name != ".." {
            names.push(name.to_string());
        }
    }
    unsafe { libc::closedir(dirp) };
    names.sort();
    names
}

fn seed_base(base: &Path) {
    dir(base);
    dir(&base.join("usr"));
    dir(&base.join("usr/bin"));
    dir(&base.join("etc"));
    dir(&base.join("alternatives"));
    let tool = base.join("usr/bin/tool");
    fs::write(&tool, TOOL).unwrap();
    mode(&tool, 0o644);
    fs::hard_link(&tool, base.join("usr/bin/tool-alias")).unwrap();
    symlink("usr/bin", base.join("bin")).unwrap();
    symlink("/proc/mounts", base.join("etc/mtab")).unwrap();
    symlink("/usr/bin/tool", base.join("alternatives/tool")).unwrap();
}

fn fixture(base: &Path, digest: [u8; 32]) -> PublicRuntime {
    PublicRuntime::fixture(base, digest).unwrap()
}

fn host_dir(root: &Path) -> PathBuf {
    let host = root.join("host");
    dir(&host);
    host
}

fn owner_path(host: &Path, owner: Owner) -> PathBuf {
    let (ns, id) = match owner {
        Owner::Personal(id) => ("personal", id),
        Owner::Group(id) => ("group", id),
    };
    host.join(ns).join(id.to_string())
}

fn assert_tree(fd: RawFd) {
    assert_eq!(read_reg(fd, "usr/bin/tool"), TOOL);
    assert_eq!(read_reg(fd, "usr/bin/tool-alias"), TOOL);
    assert_eq!(ino(fd, "usr/bin/tool"), ino(fd, "usr/bin/tool-alias"));
    assert_eq!(link_target(fd, "bin"), "usr/bin");
    assert_eq!(link_target(fd, "etc/mtab"), "/proc/mounts");
    assert_eq!(link_target(fd, "alternatives/tool"), "/usr/bin/tool");
    assert_eq!(kind(fd, "bin"), libc::S_IFLNK);
    assert_eq!(entries(fd, "usr/bin"), ["tool", "tool-alias"]);
}

fn gone(path: &Path) {
    match fs::symlink_metadata(path) {
        Err(err) => assert_eq!(err.kind(), ErrorKind::NotFound),
        Ok(_) => panic!("still present {}", path.display()),
    }
}

fn sentinel(root: &Path) -> PathBuf {
    let path = root.join("sentinel");
    fs::write(&path, b"sentinel-secret").unwrap();
    mode(&path, 0o600);
    path
}

#[test]
fn sequential_same_base_distinct_owner_inodes_persist() {
    let tmp = scratch();
    let base_path = tmp.0.join("base");
    seed_base(&base_path);
    let base = fixture(&base_path, DIGEST);
    let host = host_dir(&tmp.0);
    let personal = Owner::personal(1).unwrap();
    let other = Owner::personal(2).unwrap();
    let group = Owner::group(1).unwrap();
    let a = WorkspaceStore::open(&host, personal, &base).unwrap();
    let b = WorkspaceStore::open(&host, other, &base).unwrap();
    let g = WorkspaceStore::open(&host, group, &base).unwrap();
    assert!(a.as_fd().as_raw_fd() >= 0 && a.rootfs().as_raw_fd() >= 0);
    let generation = owner_path(&host, personal).join("generation");
    assert_eq!(
        fs::metadata(&generation).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    assert_eq!(
        fs::metadata(generation.join("rootfs"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o755
    );
    assert_eq!(
        fs::metadata(owner_path(&host, personal))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    for store in [&a, &b, &g] {
        assert_tree(store.rootfs().as_raw_fd());
    }
    let base_ino = fs::metadata(base_path.join("usr/bin/tool")).unwrap().ino();
    let a_ino = ino(a.rootfs().as_raw_fd(), "usr/bin/tool");
    let b_ino = ino(b.rootfs().as_raw_fd(), "usr/bin/tool");
    let g_ino = ino(g.rootfs().as_raw_fd(), "usr/bin/tool");
    assert_ne!(a_ino, base_ino);
    assert_ne!(b_ino, base_ino);
    assert_ne!(g_ino, base_ino);
    assert_ne!(a_ino, b_ino);
    assert_ne!(a_ino, g_ino);
    assert_eq!(a_ino, ino(a.rootfs().as_raw_fd(), "usr/bin/tool-alias"));
    write_reg(a.rootfs().as_raw_fd(), "usr/bin/tool", b"mutated-a");
    create_reg(a.as_fd().as_raw_fd(), "marker", b"work-mark");
    drop(a);
    let a = WorkspaceStore::open(&host, personal, &base).unwrap();
    assert_eq!(
        read_reg(a.rootfs().as_raw_fd(), "usr/bin/tool"),
        b"mutated-a"
    );
    assert_eq!(read_reg(a.as_fd().as_raw_fd(), "marker"), b"work-mark");
    assert_eq!(read_reg(b.rootfs().as_raw_fd(), "usr/bin/tool"), TOOL);
    assert_eq!(fs::read(base_path.join("usr/bin/tool")).unwrap(), TOOL);
    let personal_tool = owner_path(&host, personal).join("generation/rootfs/usr/bin/tool");
    let group_tool = owner_path(&host, group).join("generation/rootfs/usr/bin/tool");
    assert_ne!(
        fs::metadata(&personal_tool).unwrap().ino(),
        fs::metadata(&group_tool).unwrap().ino()
    );
}

#[test]
fn owner_lock_releases_while_unrelated_child_blocked_before_exec() {
    let tmp = scratch();
    let base_path = tmp.0.join("base");
    seed_base(&base_path);
    let base = fixture(&base_path, [0x61; 32]);
    let host = host_dir(&tmp.0);
    let owner = Owner::personal(61).unwrap();
    let store = WorkspaceStore::open(&host, owner, &base).unwrap();
    let flags = unsafe { libc::fcntl(store._lock.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);

    let mut ready = [0; 2];
    let mut release = [0; 2];
    assert_eq!(
        unsafe { libc::pipe2(ready.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    assert_eq!(
        unsafe { libc::pipe2(release.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let ready_r = unsafe { OwnedFd::from_raw_fd(ready[0]) };
    let _ready_w = unsafe { OwnedFd::from_raw_fd(ready[1]) };
    let ready_w = _ready_w.as_raw_fd();
    let _release_r = unsafe { OwnedFd::from_raw_fd(release[0]) };
    let release_r = _release_r.as_raw_fd();
    let release_w = unsafe { OwnedFd::from_raw_fd(release[1]) };

    let spawned = thread::spawn(move || {
        let mut cmd = Command::new("/bin/true");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(move || {
                let mut one = [0u8; 1];
                loop {
                    let n = libc::write(ready_w, one.as_ptr() as *const libc::c_void, 1);
                    if n == 1 {
                        break;
                    }
                    if n < 0 && *libc::__errno_location() == libc::EINTR {
                        continue;
                    }
                    libc::_exit(71);
                }
                loop {
                    let n = libc::read(release_r, one.as_mut_ptr() as *mut libc::c_void, 1);
                    if n == 1 {
                        break;
                    }
                    if n < 0 && *libc::__errno_location() == libc::EINTR {
                        continue;
                    }
                    libc::_exit(71);
                }
                Ok(())
            });
        }
        cmd.spawn()
    });
    struct Reap {
        release: RawFd,
        thread: Option<thread::JoinHandle<std::io::Result<std::process::Child>>>,
    }
    impl Drop for Reap {
        fn drop(&mut self) {
            let byte = [0u8; 1];
            unsafe { libc::write(self.release, byte.as_ptr() as *const libc::c_void, 1) };
            if let Some(handle) = self.thread.take() {
                if let Ok(Ok(mut child)) = handle.join() {
                    let _ = child.wait();
                }
            }
        }
    }
    impl Reap {
        fn finish(mut self) -> std::io::Result<std::process::ExitStatus> {
            let byte = [0u8; 1];
            unsafe { libc::write(self.release, byte.as_ptr() as *const libc::c_void, 1) };
            let handle = self.thread.take().expect("spawn thread");
            let mut child = handle.join().expect("spawn thread panicked")?;
            child.wait()
        }
    }
    let reap = Reap {
        release: release_w.as_raw_fd(),
        thread: Some(spawned),
    };
    let ready_fd = ready_r.as_raw_fd();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut saw = false;
    while Instant::now() < deadline {
        let mut pfd = libc::pollfd {
            fd: ready_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, 50) };
        if rc > 0 {
            let mut one = [0u8; 1];
            let n = unsafe { libc::read(ready_fd, one.as_mut_ptr() as *mut libc::c_void, 1) };
            assert_eq!(n, 1);
            saw = true;
            break;
        }
    }
    assert!(saw, "child did not reach pre_exec");
    drop(store);
    let reopened = WorkspaceStore::open(&host, owner, &base);
    assert!(
        !reap.thread.as_ref().expect("thread").is_finished(),
        "child left pre_exec before the reopen"
    );
    let status = reap.finish().expect("reap");
    assert!(
        reopened.is_ok(),
        "owner lock still held after critical section: {reopened:?}"
    );
    assert!(status.success(), "{status}");
    drop(reopened);
}

#[test]
fn concurrent_arc_base_independent_dirs_and_owner_lock() {
    let tmp = scratch();
    let base_path = tmp.0.join("base");
    seed_base(&base_path);
    let base = Arc::new(fixture(&base_path, DIGEST));
    let host = host_dir(&tmp.0);
    let host_b = host.clone();
    let base_b = Arc::clone(&base);
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let store = WorkspaceStore::open(&host_b, Owner::group(2).unwrap(), &base_b).unwrap();
        let fd = store.rootfs().as_raw_fd();
        tx.send((
            entries(fd, "usr/bin"),
            read_reg(fd, "usr/bin/tool"),
            ino(fd, "usr/bin/tool"),
        ))
        .unwrap();
    });
    let store = WorkspaceStore::open(&host, Owner::personal(2).unwrap(), &base).unwrap();
    let fd = store.rootfs().as_raw_fd();
    assert_eq!(entries(fd, "usr/bin"), ["tool", "tool-alias"]);
    assert_eq!(read_reg(fd, "usr/bin/tool"), TOOL);
    assert_eq!(read_reg(fd, "usr/bin/tool-alias"), TOOL);
    let (other_names, other_bytes, other_ino) = rx.recv_timeout(Duration::from_secs(3)).unwrap();
    worker.join().unwrap();
    assert_eq!(other_names, ["tool", "tool-alias"]);
    assert_eq!(other_bytes, TOOL);
    assert_ne!(other_ino, ino(fd, "usr/bin/tool"));
    let host_c = host.clone();
    let base_c = Arc::clone(&base);
    let (tx, rx) = mpsc::channel();
    let contested = thread::spawn(move || {
        tx.send(WorkspaceStore::open(&host_c, Owner::personal(2).unwrap(), &base_c).is_err())
            .unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_secs(3)).unwrap());
    contested.join().unwrap();
    drop(store);
    WorkspaceStore::open(&host, Owner::personal(2).unwrap(), &base).unwrap();
}

#[test]
fn identity_digest_tamper_nlink_fifo_symlink_keep_generation() {
    let tmp = scratch();
    let base_path = tmp.0.join("base");
    seed_base(&base_path);
    let host = host_dir(&tmp.0);
    let seed_owner = |id, digest: [u8; 32]| {
        let base = fixture(&base_path, digest);
        let owner = Owner::personal(id).unwrap();
        let store = WorkspaceStore::open(&host, owner, &base).unwrap();
        create_reg(store.as_fd().as_raw_fd(), "keep", b"keep");
        drop(store);
        owner
    };
    let unchanged = |owner: Owner| {
        let root = owner_path(&host, owner);
        assert_eq!(fs::read(root.join("work/keep")).unwrap(), b"keep");
        assert_eq!(
            fs::read(root.join("generation/rootfs/usr/bin/tool")).unwrap(),
            TOOL
        );
        gone(&root.join(".stage"));
    };
    let owner = seed_owner(1, DIGEST);
    assert!(WorkspaceStore::open(&host, owner, &fixture(&base_path, [0x22; 32])).is_err());
    unchanged(owner);
    let owner = seed_owner(2, DIGEST);
    let ident = owner_path(&host, owner).join("generation/identity");
    let mut bytes = fs::read(&ident).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&ident, &bytes).unwrap();
    assert!(WorkspaceStore::open(&host, owner, &fixture(&base_path, DIGEST)).is_err());
    unchanged(owner);
    let owner = seed_owner(3, DIGEST);
    let ident = owner_path(&host, owner).join("generation/identity");
    fs::hard_link(&ident, ident.with_file_name("identity-link")).unwrap();
    assert!(WorkspaceStore::open(&host, owner, &fixture(&base_path, DIGEST)).is_err());
    unchanged(owner);
    let owner = seed_owner(4, DIGEST);
    let ident = owner_path(&host, owner).join("generation/identity");
    let secret = sentinel(&tmp.0);
    fs::remove_file(&ident).unwrap();
    symlink(&secret, &ident).unwrap();
    assert!(WorkspaceStore::open(&host, owner, &fixture(&base_path, DIGEST)).is_err());
    assert_eq!(fs::read(&secret).unwrap(), b"sentinel-secret");
    unchanged(owner);
    let owner = seed_owner(5, DIGEST);
    let ident = owner_path(&host, owner).join("generation/identity");
    fs::remove_file(&ident).unwrap();
    assert_eq!(
        unsafe { libc::mkfifo(cstr(ident.to_str().unwrap()).as_ptr(), 0o600) },
        0
    );
    mode(&ident, 0o600);
    let host_f = host.clone();
    let base = Arc::new(fixture(&base_path, DIGEST));
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let _ = tx.send(WorkspaceStore::open(&host_f, owner, &base).is_err());
    });
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(true) => handle.join().unwrap(),
        Ok(false) => panic!("fifo identity accepted"),
        Err(_) => {
            std::mem::forget(handle);
            panic!("fifo identity blocked past 3s");
        }
    }
    unchanged(owner);
}

fn expect_clean_failure(host: &Path, base: &Path, owner: Owner) {
    assert!(WorkspaceStore::open(host, owner, &fixture(base, DIGEST)).is_err());
    gone(&owner_path(host, owner).join(".stage"));
    gone(&owner_path(host, owner).join("generation"));
}

#[test]
fn source_fifo_setid_sparse_and_depth_clean_stage() {
    let tmp = scratch();
    let host = host_dir(&tmp.0);
    let fifo_base = tmp.0.join("fifo-base");
    seed_base(&fifo_base);
    let pipe = fifo_base.join("usr/bin/pipe");
    assert_eq!(
        unsafe { libc::mkfifo(cstr(pipe.to_str().unwrap()).as_ptr(), 0o600) },
        0
    );
    expect_clean_failure(&host, &fifo_base, Owner::personal(1).unwrap());
    let setid_base = tmp.0.join("setid-base");
    seed_base(&setid_base);
    let flagged = setid_base.join("usr/bin/flagged");
    fs::write(&flagged, b"x").unwrap();
    mode(&flagged, 0o4755);
    expect_clean_failure(&host, &setid_base, Owner::personal(2).unwrap());
    let sparse_base = tmp.0.join("sparse-base");
    seed_base(&sparse_base);
    File::create(sparse_base.join("usr/bin/sparse"))
        .unwrap()
        .set_len(SPARSE)
        .unwrap();
    expect_clean_failure(&host, &sparse_base, Owner::personal(3).unwrap());
    let deep = tmp.0.join("deep-base");
    seed_base(&deep);
    let mut cursor = deep.clone();
    for _ in 0..65 {
        cursor.push("d");
        dir(&cursor);
    }
    fs::write(cursor.join("f"), b"z").unwrap();
    expect_clean_failure(&host, &deep, Owner::personal(4).unwrap());
}

#[test]
fn raw_symlink_sentinel_and_base_host_overlap() {
    let tmp = scratch();
    let secret = sentinel(&tmp.0);
    let base_path = tmp.0.join("base");
    seed_base(&base_path);
    symlink(&secret, base_path.join("usr/bin/leak")).unwrap();
    let host = host_dir(&tmp.0);
    let base = fixture(&base_path, DIGEST);
    assert!(Owner::personal(0).is_err() && Owner::group(0).is_err());
    assert!(WorkspaceStore::open(Path::new("/"), Owner::personal(1).unwrap(), &base).is_err());
    let store = WorkspaceStore::open(&host, Owner::personal(1).unwrap(), &base).unwrap();
    assert_eq!(
        link_target(store.rootfs().as_raw_fd(), "usr/bin/leak"),
        secret.to_str().unwrap()
    );
    assert_eq!(
        kind(store.rootfs().as_raw_fd(), "usr/bin/leak"),
        libc::S_IFLNK
    );
    assert_eq!(fs::read(&secret).unwrap(), b"sentinel-secret");
    drop(store);
    let via = tmp.0.join("via-host");
    symlink(&host, &via).unwrap();
    assert!(WorkspaceStore::open(&via, Owner::personal(1).unwrap(), &base).is_err());
    assert_eq!(fs::read(&secret).unwrap(), b"sentinel-secret");
    let owner = owner_path(&host, Owner::personal(8).unwrap());
    dir(&owner);
    symlink(&secret, owner.join("generation")).unwrap();
    assert!(WorkspaceStore::open(&host, Owner::personal(8).unwrap(), &base).is_err());
    assert_eq!(fs::read(&secret).unwrap(), b"sentinel-secret");
    let group = Owner::group(8).unwrap();
    drop(WorkspaceStore::open(&host, group, &base).unwrap());
    let gen = owner_path(&host, group);
    fs::remove_dir_all(gen.join("generation/rootfs")).unwrap();
    symlink(&secret, gen.join("generation/rootfs")).unwrap();
    assert!(WorkspaceStore::open(&host, group, &base).is_err());
    assert_eq!(fs::read(&secret).unwrap(), b"sentinel-secret");
    let overlap = tmp.0.join("overlap");
    seed_base(&overlap);
    assert!(WorkspaceStore::open(
        &overlap,
        Owner::personal(3).unwrap(),
        &fixture(&overlap, DIGEST)
    )
    .is_err());
    assert_eq!(fs::read(overlap.join("usr/bin/tool")).unwrap(), TOOL);
    gone(&overlap.join("personal"));
    let nested_base = tmp.0.join("nested-base");
    seed_base(&nested_base);
    let nested_host = nested_base.join("inside");
    dir(&nested_host);
    assert!(WorkspaceStore::open(
        &nested_host,
        Owner::personal(4).unwrap(),
        &fixture(&nested_base, DIGEST)
    )
    .is_err());
    assert_eq!(fs::read(nested_base.join("usr/bin/tool")).unwrap(), TOOL);
    gone(&nested_base.join("personal"));
    gone(&nested_host.join("personal"));
}

#[test]
fn simulated_crash_stale_stage_cleans_without_sentinel_writes() {
    let tmp = scratch();
    let base_path = tmp.0.join("base");
    seed_base(&base_path);
    let host = host_dir(&tmp.0);
    let base = fixture(&base_path, DIGEST);
    let owner = Owner::personal(9).unwrap();
    drop(WorkspaceStore::open(&host, owner, &base).unwrap());
    fs::write(base_path.join("usr/bin/tool"), b"reseed-would-change").unwrap();
    let secret_dir = tmp.0.join("precious");
    dir(&secret_dir);
    fs::write(secret_dir.join("file"), b"untouched").unwrap();
    let stage = owner_path(&host, owner).join(".stage");
    dir(&stage);
    symlink(&secret_dir, stage.join("rootfs")).unwrap();
    symlink(secret_dir.join("file"), stage.join("hostile")).unwrap();
    let again = WorkspaceStore::open(&host, owner, &base).unwrap();
    assert_eq!(read_reg(again.rootfs().as_raw_fd(), "usr/bin/tool"), TOOL);
    drop(again);
    gone(&stage);
    assert_eq!(fs::read(secret_dir.join("file")).unwrap(), b"untouched");
}

#[test]
fn mountpoints_are_real_dirs_and_proc_symlink_rejects() {
    let tmp = scratch();
    let base_path = tmp.0.join("base");
    seed_base(&base_path);
    let host = host_dir(&tmp.0);
    let store = WorkspaceStore::open(
        &host,
        Owner::personal(1).unwrap(),
        &fixture(&base_path, DIGEST),
    )
    .unwrap();
    for name in ["proc", "dev", "work", "tmp", "run", "sys"] {
        assert_eq!(kind(store.rootfs().as_raw_fd(), name), libc::S_IFDIR);
    }
    drop(store);
    let linked = tmp.0.join("proc-link-base");
    seed_base(&linked);
    symlink("/proc", linked.join("proc")).unwrap();
    let owner = Owner::personal(2).unwrap();
    assert!(WorkspaceStore::open(&host, owner, &fixture(&linked, DIGEST)).is_err());
    gone(&owner_path(&host, owner).join("generation"));
    gone(&owner_path(&host, owner).join(".stage"));
}

fn perm_bits(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn copy_preserves_ordinary_modes_and_readonly_dir_cleans() {
    let tmp = scratch();
    let src = tmp.0.join("src");
    dir(&src);
    for (name, bits) in [
        ("d755", 0o755),
        ("d700", 0o700),
        ("d775", 0o775),
        ("d1777", 0o1777),
    ] {
        let path = src.join(name);
        dir(&path);
        mode(&path, bits);
    }
    for (name, bits) in [("f644", 0o644), ("f664", 0o664), ("f700", 0o700)] {
        let path = src.join(name);
        fs::write(&path, b"payload").unwrap();
        mode(&path, bits);
    }
    let dst = tmp.0.join("dst");
    dir(&dst);
    let src_fd = File::open(&src).unwrap();
    let dst_fd = File::open(&dst).unwrap();
    super::workspace_seed::copy_tree(src_fd.as_raw_fd(), dst_fd.as_raw_fd()).unwrap();
    for (name, bits) in [
        ("d755", 0o755),
        ("d700", 0o700),
        ("d775", 0o775),
        ("d1777", 0o1777),
        ("f644", 0o644),
        ("f664", 0o664),
        ("f700", 0o700),
    ] {
        assert_eq!(perm_bits(&dst.join(name)), bits, "{name}");
        assert_eq!(fs::read(dst.join(name)).unwrap_or_default(), {
            if name.starts_with('f') {
                b"payload".to_vec()
            } else {
                Vec::new()
            }
        });
    }
    let parent = tmp.0.join("clean");
    dir(&parent);
    let locked = parent.join("locked");
    dir(&locked);
    fs::write(locked.join("secret"), b"keep-until-clean").unwrap();
    mode(&locked, 0o555);
    let parent_fd = File::open(&parent).unwrap();
    super::workspace_seed::remove_tree(parent_fd.as_raw_fd(), &cstr("locked")).unwrap();
    gone(&locked);
}
