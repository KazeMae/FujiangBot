use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use fujiang_core::Source;

async fn resolved_owner(source: Source, actor: i64) -> crate::workspace::WorkspaceOwner {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    crate::workspace::ensure_schema(&pool).await.unwrap();
    crate::workspace::resolve(&pool, crate::workspace::HostTurn { source, actor })
        .await
        .unwrap()
}

#[tokio::test]
async fn production_authority_never_reaches_the_runner() {
    assert!(!production_available());
    assert!(ProductionSupply.acquire().is_err());
    let owner = resolved_owner(Source::Friend { id: 1 }, 1).await;
    let mut called = false;
    let err = execute_with(
        owner,
        crate::workspace_runner::Payload::Sh("true".to_string()),
        &ProductionSupply,
        |_, _, _| {
            called = true;
            Ok(crate::workspace_runner::RunResult {
                exit_code: 0,
                timed_out: false,
                stdout: Vec::new(),
                stderr: Vec::new(),
                truncated: false,
            })
        },
    )
    .unwrap_err();
    assert!(!called);
    assert!(err.to_string().contains("workspace mvp disabled"));
}

struct PublicAuth {
    host: PathBuf,
}

impl RootfsAuthority for PublicAuth {
    fn acquire(&self) -> anyhow::Result<(PathBuf, PublicRuntime)> {
        Ok((self.host.clone(), PublicRuntime::current()?))
    }
}

fn scratch(label: &str) -> (PathBuf, PublicAuth) {
    let root = std::env::temp_dir().join(format!(
        "ws-ovl-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let host = root.join("host");
    std::fs::create_dir_all(&host).unwrap();
    let mode = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(&root, mode.clone()).unwrap();
    std::fs::set_permissions(&host, mode).unwrap();
    (root, PublicAuth { host })
}

fn chmod_tree(path: &Path) {
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            continue;
        };
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
}

fn cleanup(root: &Path) {
    chmod_tree(root);
    let _ = std::fs::remove_dir_all(root);
}

fn run_py(
    auth: &PublicAuth,
    owner: crate::workspace::WorkspaceOwner,
    script: &str,
) -> crate::workspace_runner::RunResult {
    execute_with(
        owner,
        crate::workspace_runner::Payload::Python(script.to_string()),
        auth,
        crate::workspace_runner::run,
    )
    .unwrap()
}

fn cloexec(fd: std::os::fd::RawFd, on: bool) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0, "{}", std::io::Error::last_os_error());
    let flags = if on {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags) },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
}

fn text(run: &crate::workspace_runner::RunResult) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    )
}

fn sha256(path: &Path) -> String {
    let out = Command::new("sha256sum").arg(path).output().unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn dir_names(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .unwrap()
        .map(|ent| ent.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn bash_python_and_work_persist() {
    let (root, auth) = scratch("keep");
    let owner = resolved_owner(Source::Friend { id: 9 }, 9).await;
    let bash = execute_with(
        owner,
        crate::workspace_runner::Payload::Bash("echo BASH_OK > /work/mark".to_string()),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    assert_eq!(bash.exit_code, 0, "{}", text(&bash));
    let py = run_py(&auth, owner, "print(open('/work/mark').read().strip())");
    assert_eq!(py.exit_code, 0, "{}", text(&py));
    assert!(text(&py).contains("BASH_OK"));
    cleanup(&root);
}

#[tokio::test]
async fn system_dirs_read_only_and_apt_fails_without_touching_host() {
    let (root, auth) = scratch("ro");
    let owner = resolved_owner(Source::Friend { id: 9 }, 9).await;
    let status = Path::new("/var/lib/dpkg/status");
    let usr_bin = Path::new("/usr/bin");
    let before_hash = sha256(status);
    let before_mtime = std::fs::metadata(status).unwrap().modified().unwrap();
    let before_dir_mtime = std::fs::metadata(usr_bin).unwrap().modified().unwrap();
    let before_names = dir_names(usr_bin);
    let run = run_py(
        &auth,
        owner,
        r#"
import os, subprocess
def create(path):
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
        os.close(fd)
        print("CREATE", 0)
    except OSError as err:
        print("CREATE", err.errno)
def append(path):
    try:
        fd = os.open(path, os.O_WRONLY | os.O_APPEND)
        os.write(fd, b"X")
        os.close(fd)
        print("APPEND", 0)
    except OSError as err:
        print("APPEND", err.errno)
create("/usr/bin/fujiang-ro-probe")
create("/etc/fujiang-ro-probe")
create("/var/lib/dpkg/fujiang-ro-probe")
append("/var/lib/dpkg/status")
open("/work/not-a-deb","wb").write(b"not a deb\n")
def rc(argv):
    print("RC", subprocess.call(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
rc(["apt-get","install","-y","--no-download","/work/not-a-deb"])
rc(["dpkg","-i","/work/not-a-deb"])
rc(["dpkg","--configure","-a"])
"#,
    );
    let guest = text(&run);
    let after_hash = sha256(status);
    let after_mtime = std::fs::metadata(status).unwrap().modified().unwrap();
    let after_dir_mtime = std::fs::metadata(usr_bin).unwrap().modified().unwrap();
    let after_names = dir_names(usr_bin);
    cleanup(&root);
    assert_eq!(run.exit_code, 0, "exit {}", run.exit_code);
    let writes: Vec<&str> = guest
        .lines()
        .filter(|line| line.starts_with("CREATE") || line.starts_with("APPEND"))
        .collect();
    assert_eq!(writes.len(), 4, "write attempts {}", writes.len());
    for line in writes {
        let errno: i32 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        assert!(
            errno == libc::EROFS || errno == libc::EACCES,
            "write errno {errno}"
        );
    }
    let codes: Vec<i32> = guest
        .lines()
        .filter(|line| line.starts_with("RC "))
        .map(|line| line.split_whitespace().nth(1).unwrap().parse().unwrap())
        .collect();
    assert_eq!(codes.len(), 3);
    assert!(
        codes.iter().all(|code| *code != 0),
        "command exits {codes:?}"
    );
    assert_eq!(before_hash, after_hash);
    assert_eq!(before_mtime, after_mtime);
    assert_eq!(before_dir_mtime, after_dir_mtime);
    assert_eq!(before_names, after_names);
    assert!(!Path::new("/usr/bin/fujiang-ro-probe").exists());
    assert!(!Path::new("/etc/fujiang-ro-probe").exists());
    assert!(!Path::new("/var/lib/dpkg/fujiang-ro-probe").exists());
}

#[tokio::test]
async fn guest_cannot_use_host_fds_env_or_abstract_sockets() {
    let (root, auth) = scratch("neg");
    let sentinel = root.join("sentinel");
    std::fs::write(&sentinel, b"HOST-SENTINEL").unwrap();
    let owner = resolved_owner(Source::Friend { id: 9 }, 9).await;
    let other = resolved_owner(Source::Friend { id: 8 }, 8).await;
    let _seed = execute_with(
        other,
        crate::workspace_runner::Payload::Bash("echo OTHER > /work/secret".to_string()),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    let file = File::open(&sentinel).unwrap();
    let mut out = File::options().write(true).open(&sentinel).unwrap();
    out.write_all(b"HOST-SENTINEL").unwrap();
    cloexec(file.as_raw_fd(), false);
    cloexec(out.as_raw_fd(), false);
    std::env::set_var("FUJIANG_HOST_ENV_MARKER", "host-marker-value");
    let name = format!("fujiang-ovl-{}", std::process::id());
    let sock = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(sock >= 0);
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as _;
    let bytes = name.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        addr.sun_path[i + 1] = *b as libc::c_char;
    }
    let len = (std::mem::size_of::<libc::sa_family_t>() + 1 + bytes.len()) as libc::socklen_t;
    assert_eq!(
        unsafe { libc::bind(sock, (&addr as *const libc::sockaddr_un).cast(), len) },
        0
    );
    assert_eq!(unsafe { libc::listen(sock, 1) }, 0);
    let sent = sentinel.display().to_string();
    let other_file = auth.host.join("personal/8/work/secret");
    let script = r#"
import os, socket, stat
bad=False
dirs=0
def outside(target):
    return target.startswith("/") and target != "/work" and not target.startswith("/work/")
for base in ("/proc/self/fd","/proc/1/fd"):
    try:
        names=os.listdir(base)
    except OSError:
        continue
    for n in names:
        if not n.isdigit() or int(n) < 3:
            continue
        path=base+"/"+n
        try:
            if stat.S_ISDIR(os.stat(path).st_mode):
                dirs += 1
        except OSError:
            pass
        try:
            target=os.readlink(path)
        except OSError:
            target=""
        try:
            rd=os.open(path, os.O_RDONLY)
        except OSError:
            rd=None
        if rd is not None:
            try:
                data=os.read(rd, 64)
            except OSError:
                data=b""
            os.close(rd)
            if b"HOST-SENTINEL" in data:
                bad=True
        try:
            wr=os.open(path, os.O_WRONLY | os.O_APPEND)
        except OSError:
            wr=None
        if wr is not None:
            try:
                os.write(wr, b"X")
                if outside(target):
                    bad=True
            except OSError:
                pass
            os.close(wr)
print("FD_BAD" if bad else "FD_CLEAN")
print("DIRFD", dirs)
marker=b"host-marker-value"
hits=0
for pid in os.listdir("/proc"):
    if not pid.isdigit():
        continue
    try:
        env=open("/proc/"+pid+"/environ","rb").read()
    except OSError:
        continue
    if marker in env:
        hits += 1
print("ENV_HITS", hits)
def fail(path):
    try:
        os.symlink(path, "/work/link")
        open("/work/link").read()
        print("LINK_OK")
    except OSError as err:
        print("LINK", err.errno)
    finally:
        try:
            os.unlink("/work/link")
        except OSError:
            pass
fail("SENTINEL_PATH")
fail("OTHER_PATH")
try:
    open("/usr/bin/ovl-host-write","w").write("x")
    print("WRITE", 0)
except OSError as err:
    print("WRITE", err.errno)
s=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    s.connect("\0SOCKET_NAME")
    print("SOCK_OK")
except OSError as err:
    print("SOCK", err.errno)
print("DONE")
"#
    .replace("SENTINEL_PATH", &sent)
    .replace("OTHER_PATH", &other_file.display().to_string())
    .replace("SOCKET_NAME", &name);
    let run = run_py(&auth, owner, &script);
    let guest = text(&run);
    cloexec(file.as_raw_fd(), true);
    cloexec(out.as_raw_fd(), true);
    drop(file);
    drop(out);
    unsafe { libc::close(sock) };
    let sent_bytes = std::fs::read(&sentinel).unwrap();
    cleanup(&root);
    assert_eq!(run.exit_code, 0, "exit {} {guest}", run.exit_code);
    assert!(guest.contains("FD_CLEAN"), "{guest}");
    let dirfd = guest
        .lines()
        .find_map(|line| line.strip_prefix("DIRFD "))
        .unwrap_or("missing");
    assert_eq!(dirfd, "0", "directory fds {dirfd}");
    let env_hits = guest
        .lines()
        .find_map(|line| line.strip_prefix("ENV_HITS "))
        .unwrap_or("missing");
    assert_eq!(
        env_hits, "0",
        "host env marker visible in {env_hits} proc environ files"
    );
    assert!(guest.contains("LINK"), "{guest}");
    assert!(!guest.contains("LINK_OK"), "{guest}");
    let write_errno: i32 = guest
        .lines()
        .find(|line| line.starts_with("WRITE "))
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        write_errno == libc::EROFS || write_errno == libc::EACCES,
        "write errno {write_errno}"
    );
    assert!(guest.contains(&format!("SOCK {}", libc::EPERM)), "{guest}");
    assert_eq!(sent_bytes, b"HOST-SENTINEL");
    assert!(!Path::new("/usr/bin/ovl-host-write").exists());
}
