use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;

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
    let text = err.to_string();
    assert!(text.contains("workspace mvp disabled"), "{text}");
}

struct FixtureAuth {
    host: std::path::PathBuf,
    image: std::path::PathBuf,
}

impl RootfsAuthority for FixtureAuth {
    fn acquire(&self) -> anyhow::Result<(PathBuf, PublicRuntime)> {
        Ok((
            self.host.clone(),
            PublicRuntime::fixture(&self.image, [3u8; 32])?,
        ))
    }
}

#[tokio::test]
async fn existing_runner_failclosed_on_incomplete_root() {
    let root = std::env::temp_dir().join(format!(
        "ws-fail-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let host = root.join("host");
    let host_path = host.clone();
    let image = root.join("image");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::create_dir_all(&image).unwrap();
    let mode = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(&root, mode.clone()).unwrap();
    std::fs::set_permissions(&host, mode.clone()).unwrap();
    std::fs::set_permissions(&image, mode).unwrap();
    let sentinel = root.join("host-sentinel");
    std::fs::write(&sentinel, b"untouched").unwrap();
    let auth = FixtureAuth { host, image };
    let owner = resolved_owner(Source::Group { id: 4 }, 4).await;
    let mut saw_bwrap = false;
    for payload in [
        crate::workspace_runner::Payload::Bash("true".to_string()),
        crate::workspace_runner::Payload::Python("print(1)".to_string()),
    ] {
        let mut entered = false;
        let outcome = execute_with(owner, payload, &auth, |root, work, payload| {
            entered = true;
            crate::workspace_runner::run(root, work, payload)
        });
        assert!(entered, "runner was not entered: {outcome:?}");
        match &outcome {
            Err(err) => {
                let text = err.to_string();
                eprintln!("native runner diagnostic: {text}");
                assert!(!text.to_lowercase().contains("ubuntu"), "{text}");
            }
            Ok(result) => {
                eprintln!(
                    "native runner diagnostic: exit={} stdout={} stderr={}",
                    result.exit_code,
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
                assert_ne!(result.exit_code, 0);
                if result.stderr.windows(5).any(|w| w == b"bwrap")
                    || result.stdout.windows(5).any(|w| w == b"bwrap")
                {
                    saw_bwrap = true;
                }
                let blob = format!(
                    "{}{}",
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
                assert!(!blob.to_lowercase().contains("ubuntu"), "{blob}");
            }
        }
        let mut reopened = false;
        let reopen = execute_with(
            owner,
            crate::workspace_runner::Payload::Sh("true".to_string()),
            &auth,
            |_, _, _| {
                reopened = true;
                Err(anyhow::anyhow!("reopen probe"))
            },
        );
        assert!(reopened, "owner lock still held: {reopen:?}");
        assert!(reopen.is_err());
    }
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"untouched");
    if saw_bwrap {
        let tmp = host_path.join("group/4/generation/rootfs/tmp");
        let mode = std::fs::metadata(&tmp).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o1777, "persistent /tmp mode {mode:#o}");
    }
    let _ = std::fs::remove_dir_all(root);
}

struct PublicAuth {
    host: std::path::PathBuf,
}

impl RootfsAuthority for PublicAuth {
    fn acquire(&self) -> anyhow::Result<(PathBuf, PublicRuntime)> {
        Ok((self.host.clone(), crate::workspace_store::stage_public()?))
    }
}

#[tokio::test]
async fn public_runtime_shell_python_persist() {
    let root = std::env::temp_dir().join(format!(
        "ws-mvp-{}-{}",
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
    let sentinel = root.join("sentinel");
    std::fs::write(&sentinel, b"HOST-SENTINEL").unwrap();
    let auth = PublicAuth { host: host.clone() };
    let owner = resolved_owner(Source::Friend { id: 9 }, 9).await;
    let script = format!(
        "import os,hashlib,sqlite3,lzma,ctypes,ssl\nopen('/work/marker','w').write('A-MARKER\\n')\nprint('SHELL_VIA_PY')\nprint(open('/proc/self/status').read().split('CapEff:')[1].split()[0])\nh=hashlib.sha256(b'abc').hexdigest()\nc=sqlite3.connect(':memory:')\nc.execute('create table t(x)')\nz=lzma.compress(b'abc')\nprint('MODS', h[:8], sqlite3.sqlite_version, len(z), ctypes.sizeof(ctypes.c_int), ssl.OPENSSL_VERSION.split()[0])\nprint('SSLFILE', os.environ.get('SSL_CERT_FILE',''))\nbad=[]\nfor base in ('/proc/self/fd','/proc/1/fd'):\n names=list(os.listdir(base))\n for n in names:\n  p=base+'/'+n\n  try:\n   t=os.readlink(p)\n  except FileNotFoundError:\n   continue\n  if {sent:?} in t: bad.append(t)\nprint('FD_BAD' if bad else 'FD_CLEAN')\nprint(bad)\n",
        sent = sentinel.display().to_string()
    );
    let bash = execute_with(
        owner,
        crate::workspace_runner::Payload::Bash("echo SHELL_OK".to_string()),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    assert_eq!(
        bash.exit_code,
        0,
        "{}",
        String::from_utf8_lossy(&bash.stderr)
    );
    assert!(bash.stdout.windows(8).any(|w| w == b"SHELL_OK"));
    let py = execute_with(
        owner,
        crate::workspace_runner::Payload::Python(script),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    let out = String::from_utf8_lossy(&py.stdout);
    let err = String::from_utf8_lossy(&py.stderr);
    assert_eq!(py.exit_code, 0, "{out}{err}");
    assert!(out.contains("MODS"), "{out}{err}");
    assert!(
        out.contains("SSLFILE /etc/ssl/certs/ca-certificates.crt"),
        "{out}{err}"
    );
    assert!(out.contains("SHELL_VIA_PY"), "{out}");
    assert!(out.contains("FD_CLEAN"), "{out}");
    assert!(out.contains("0000000000000000"), "{out}");
    let again = execute_with(
        owner,
        crate::workspace_runner::Payload::Python(
            "print(open('/work/marker').read().strip())".to_string(),
        ),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    assert_eq!(
        again.exit_code,
        0,
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert!(again.stdout.windows(8).any(|w| w == b"A-MARKER"));
    let work = host.join("personal/9/work");
    let deb_src = root.join("deb");
    std::fs::create_dir_all(deb_src.join("DEBIAN")).unwrap();
    std::fs::create_dir_all(deb_src.join("usr/share/mvp-apt")).unwrap();
    std::fs::write(
        deb_src.join("DEBIAN/control"),
        "Package: mvp-apt\nVersion: 0.0.1\nArchitecture: all\nMaintainer: mvp <mvp@example.invalid>\nDescription: local\n offline fixture\n",
    )
    .unwrap();
    std::fs::write(deb_src.join("usr/share/mvp-apt/marker"), "apt-marker\n").unwrap();
    let deb = work.join("mvp-apt.deb");
    let built = std::process::Command::new("dpkg-deb")
        .args(["--root-owner-group", "--build"])
        .arg(&deb_src)
        .arg(&deb)
        .status()
        .unwrap();
    assert!(built.success());
    let apt_py = r#"
import os, subprocess
for d in ("/var/lib/dpkg/info","/var/lib/dpkg/updates","/etc/apt/apt.conf.d","/etc/apt/preferences.d","/var/cache/apt/archives/partial","/var/lib/apt/lists/partial","/var/log/apt"):
    os.makedirs(d, exist_ok=True)
open("/var/lib/dpkg/status","a").close()
open("/var/lib/dpkg/available","a").close()
open("/etc/apt/sources.list","w").write("")
open("/etc/apt/apt.conf","w").write('APT::Sandbox::User "root";\n')
import shutil
shutil.copy("/work/mvp-apt.deb", "/var/cache/apt/archives/mvp-apt_0.0.1_all.deb")
r=subprocess.run(["apt-get","install","-y","--no-download","/var/cache/apt/archives/mvp-apt_0.0.1_all.deb"],capture_output=True,text=True)
print(r.stdout)
print(r.stderr)
print("APT_EXIT", r.returncode)
raise SystemExit(r.returncode)
"#;
    let apt = execute_with(
        owner,
        crate::workspace_runner::Payload::Python(apt_py.to_string()),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    let apt_out = format!(
        "{}{}",
        String::from_utf8_lossy(&apt.stdout),
        String::from_utf8_lossy(&apt.stderr)
    );
    assert_eq!(apt.exit_code, 0, "{apt_out}");
    assert!(apt_out.contains("newly installed"), "{apt_out}");
    let kept = execute_with(
        owner,
        crate::workspace_runner::Payload::Python(
            "print(open('/usr/share/mvp-apt/marker').read().strip())".to_string(),
        ),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    let kept_out = String::from_utf8_lossy(&kept.stdout);
    assert_eq!(
        kept.exit_code,
        0,
        "{}",
        String::from_utf8_lossy(&kept.stderr)
    );
    assert!(kept_out.contains("apt-marker"), "{kept_out}");
    let _ = std::fs::remove_dir_all(root);
}

fn clear_cloexec(fd: i32) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0);
    assert!(unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } == 0);
}

#[tokio::test]
async fn inherited_fds_cannot_touch_host_or_other_owner() {
    let root = std::env::temp_dir().join(format!(
        "ws-fd-{}-{}",
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
    let sentinel = root.join("sentinel");
    std::fs::write(&sentinel, b"HOST-SENTINEL-BYTES").unwrap();
    let other = root.join("other-owner");
    std::fs::write(&other, b"OTHER-OWNER-BYTES").unwrap();
    let sent_fd = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&sentinel)
        .unwrap();
    let other_fd = std::fs::File::open(&other).unwrap();
    let root_fd = std::fs::File::open(&root).unwrap();
    let host_fd = std::fs::File::open(&host).unwrap();
    let usr_fd = std::fs::File::open("/usr/bin").unwrap();
    for fd in [
        sent_fd.as_raw_fd(),
        other_fd.as_raw_fd(),
        root_fd.as_raw_fd(),
        host_fd.as_raw_fd(),
        usr_fd.as_raw_fd(),
    ] {
        clear_cloexec(fd);
    }
    let auth = PublicAuth { host: host.clone() };
    let owner = resolved_owner(Source::Friend { id: 11 }, 11).await;
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    let source_fd = std::fs::File::open(&source).unwrap();
    clear_cloexec(source_fd.as_raw_fd());
    let sent_path = sentinel.display().to_string();
    let other_path = other.display().to_string();
    let probe = format!(
        r#"
import os
secret=b"HOST-SENTINEL-BYTES"
other=b"OTHER-OWNER-BYTES"
leaks=[]
open("/work/marker","wb").write(b"OWN-MARKER")
os.link("/work/marker", "/work/marker-hard")
os.symlink("marker", "/work/marker-sym")
if open("/work/marker-hard","rb").read()!=b"OWN-MARKER":
    raise SystemExit("control hardlink")
if open("/work/marker-sym","rb").read()!=b"OWN-MARKER":
    raise SystemExit("control symlink")
def touch(path):
    fd=None
    for flag in (os.O_RDWR, os.O_RDONLY):
        try:
            fd=os.open(path, flag)
            break
        except OSError:
            fd=None
    if fd is None:
        return
    try:
        data=os.read(fd, 80)
        if secret in data or other in data:
            leaks.append("read "+path)
        if flag_writable(fd):
            leaks.append("write "+path)
    finally:
        os.close(fd)
def flag_writable(fd):
    try:
        os.write(fd, b"PWNED")
        return True
    except OSError:
        return False
def scan(base):
    try:
        names=list(os.listdir(base))
    except OSError:
        return
    rels=("sentinel","other-owner","../sentinel","../other-owner","../../sentinel","../../other-owner","../../../sentinel","../../../other-owner")
    for n in names:
        if n in ("0","1","2"):
            continue
        proc=base+"/"+n
        touch(proc)
        for rel in rels:
            try:
                data=open(proc+"/"+rel,"rb").read(80)
            except OSError:
                continue
            if secret in data or other in data:
                leaks.append("climb "+proc+" "+rel)
for base in ("/proc/self/fd","/proc/1/fd"):
    scan(base)
for path in ({sent_path:?}, {other_path:?}):
    try:
        data=open(path,"rb").read(80)
        if secret in data or other in data:
            leaks.append("path")
    except OSError:
        pass
    try:
        os.symlink(path, "/work/host-escape")
        data=open("/work/host-escape","rb").read(80)
        if secret in data or other in data:
            leaks.append("symlink")
    except OSError:
        pass
    try:
        os.link(path, "/work/host-hard")
        data=open("/work/host-hard","rb").read(80)
        if secret in data or other in data:
            leaks.append("hardlink")
    except OSError:
        pass
print("CONTROL_OK")
print("SEALED" if not leaks else "LEAK "+",".join(leaks))
"#
    );
    let run = execute_with(
        owner,
        crate::workspace_runner::Payload::Python(probe),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    let out = String::from_utf8_lossy(&run.stdout);
    let err = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.exit_code, 0, "{out}{err}");
    assert!(out.contains("CONTROL_OK"), "{out}{err}");
    assert!(out.contains("SEALED"), "{out}{err}");
    assert!(!out.contains("LEAK"), "{out}");
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"HOST-SENTINEL-BYTES");
    assert_eq!(std::fs::read(&other).unwrap(), b"OTHER-OWNER-BYTES");
    drop((sent_fd, other_fd, root_fd, host_fd, usr_fd, source_fd));
    let _ = std::fs::remove_dir_all(root);
}
