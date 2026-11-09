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

fn build_deb(dir: &Path) -> PathBuf {
    let src = dir.join("deb");
    std::fs::create_dir_all(src.join("DEBIAN")).unwrap();
    std::fs::create_dir_all(src.join("usr/bin")).unwrap();
    std::fs::write(
        src.join("DEBIAN/control"),
        "Package: ovlhello\nVersion: 0.0.1\nArchitecture: all\nMaintainer: f <f@example.invalid>\nDescription: overlay fixture\n",
    )
    .unwrap();
    std::fs::write(src.join("usr/bin/ovlhello"), b"#!/bin/sh\necho OVLHELLO\n").unwrap();
    std::fs::set_permissions(
        src.join("usr/bin/ovlhello"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let deb = dir.join("ovlhello.deb");
    assert!(Command::new("dpkg-deb")
        .args(["--root-owner-group", "--build"])
        .arg(&src)
        .arg(&deb)
        .status()
        .unwrap()
        .success());
    deb
}

#[tokio::test]
async fn bash_python_and_local_deb_persist_without_touching_host() {
    let (root, auth) = scratch("keep");
    let owner = resolved_owner(Source::Friend { id: 9 }, 9).await;
    let other = resolved_owner(Source::Friend { id: 8 }, 8).await;
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
    let deb = build_deb(&root);
    std::fs::copy(&deb, auth.host.join("personal/9/work/ovlhello.deb")).unwrap();
    let install = run_py(
        &auth,
        owner,
        r#"
import subprocess
r=subprocess.run(["apt-get","-o","APT::Sandbox::User=root","install","-y","/work/ovlhello.deb"],capture_output=True,text=True)
print(r.stdout)
print(r.stderr)
raise SystemExit(r.returncode)
"#,
    );
    let inst = text(&install);
    assert_eq!(install.exit_code, 0, "{inst}");
    assert!(inst.contains("newly installed"), "{inst}");
    let again = run_py(
        &auth,
        owner,
        "import subprocess; raise SystemExit(subprocess.call(['/usr/bin/ovlhello']))",
    );
    assert_eq!(again.exit_code, 0, "{}", text(&again));
    assert!(text(&again).contains("OVLHELLO"));
    assert!(!Path::new("/usr/bin/ovlhello").exists());
    let host_dpkg = Command::new("dpkg")
        .args(["-s", "ovlhello"])
        .status()
        .unwrap();
    assert!(!host_dpkg.success());
    let hidden = run_py(
        &auth,
        other,
        "import os; print('ABSENT' if not os.path.exists('/usr/bin/ovlhello') else 'PRESENT')",
    );
    assert_eq!(hidden.exit_code, 0, "{}", text(&hidden));
    assert!(text(&hidden).contains("ABSENT"), "{}", text(&hidden));
    cleanup(&root);
}

#[tokio::test]
async fn prepare_skips_guest_symlink_that_points_outside_the_owner() {
    let (root, auth) = scratch("escape");
    let hostdir = root.join("hostdir");
    std::fs::create_dir(&hostdir).unwrap();
    let sentinel = root.join("sentinel");
    std::fs::write(&sentinel, b"HOST-SENTINEL").unwrap();
    let owner = resolved_owner(Source::Friend { id: 9 }, 9).await;
    let target = hostdir.display().to_string();
    let plant = run_py(
        &auth,
        owner,
        &r#"
import os, shutil
def plant(path, dest):
    if os.path.islink(path):
        os.unlink(path)
    elif os.path.isdir(path):
        shutil.rmtree(path)
    elif os.path.lexists(path):
        os.unlink(path)
    os.symlink(dest, path)
plant("/etc/apt/apt.conf.d", "HOSTDIR")
plant("/var/lib/dpkg/triggers", "HOSTDIR")
for path in (
    "/var/lib/dpkg/lock",
    "/var/lib/dpkg/lock-frontend",
    "/var/lib/apt/lists/lock",
    "/var/cache/debconf/passwords.dat",
):
    try:
        os.remove(path)
    except FileNotFoundError:
        pass
print("PLANTED")
"#
        .replace("HOSTDIR", &target),
    );
    assert_eq!(plant.exit_code, 0, "{}", text(&plant));
    assert!(text(&plant).contains("PLANTED"), "{}", text(&plant));
    let again = run_py(&auth, owner, "print('SECOND_OK')");
    let again_text = text(&again);
    let host_entries: Vec<_> = std::fs::read_dir(&hostdir)
        .unwrap()
        .map(|ent| ent.unwrap().file_name())
        .collect();
    let sent_bytes = std::fs::read(&sentinel).unwrap();
    cleanup(&root);
    assert_eq!(again.exit_code, 0, "{again_text}");
    assert!(again_text.contains("SECOND_OK"), "{again_text}");
    assert!(host_entries.is_empty(), "hostdir gained {host_entries:?}");
    assert_eq!(sent_bytes, b"HOST-SENTINEL");
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
import os, socket
bad=False
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
marker=b"host-marker-value"
hit=[]
for pid in os.listdir("/proc"):
    if not pid.isdigit():
        continue
    try:
        env=open("/proc/"+pid+"/environ","rb").read()
    except OSError:
        continue
    if marker in env:
        hit.append(pid)
print("ENV_BAD" if hit else "ENV_CLEAN")
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
open("/usr/bin/ovl-host-write","w").write("x")
print("GUEST_WROTE")
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
    assert!(guest.contains("ENV_CLEAN"), "{guest}");
    assert!(guest.contains("LINK"), "{guest}");
    assert!(!guest.contains("LINK_OK"), "{guest}");
    assert!(guest.contains("GUEST_WROTE"), "{guest}");
    assert!(guest.contains(&format!("SOCK {}", libc::EPERM)), "{guest}");
    assert_eq!(sent_bytes, b"HOST-SENTINEL");
    assert!(!Path::new("/usr/bin/ovl-host-write").exists());
}
