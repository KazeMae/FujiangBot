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

struct GpgHome(std::path::PathBuf);

impl Drop for GpgHome {
    fn drop(&mut self) {
        let _ = std::process::Command::new("/usr/bin/gpgconf")
            .arg("--homedir")
            .arg(&self.0)
            .args(["--kill", "all"])
            .status();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn gpg_export(home: &std::path::Path, out: &std::path::Path) {
    let status = std::process::Command::new("gpg")
        .env("GNUPGHOME", home)
        .args([
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-gen-key",
            "Fixture <f@example.invalid>",
            "default",
            "default",
            "never",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let key = std::process::Command::new("gpg")
        .env("GNUPGHOME", home)
        .args(["--batch", "--export"])
        .output()
        .unwrap();
    assert!(key.status.success());
    std::fs::write(out, key.stdout).unwrap();
}

fn file_digest(cmd: &str, path: &std::path::Path) -> String {
    let out = std::process::Command::new(cmd).arg(path).output().unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn fixture_sqv_rejects_wrong_key_tamper_and_keeps_fds() {
    let root = std::env::temp_dir().join(format!(
        "ws-aptfix-{}-{}",
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
    execute_with(
        owner,
        crate::workspace_runner::Payload::Bash("true".to_string()),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    let work = host.join("personal/9/work");
    let repo = work.join("repo");
    let pool = repo.join("pool");
    let dist = repo.join("dists/fix/main/binary-all");
    std::fs::create_dir_all(&pool).unwrap();
    std::fs::create_dir_all(&dist).unwrap();
    let deb_src = root.join("deb");
    std::fs::create_dir_all(deb_src.join("DEBIAN")).unwrap();
    std::fs::create_dir_all(deb_src.join("usr/share/mvp-sign")).unwrap();
    std::fs::write(
        deb_src.join("DEBIAN/control"),
        "Package: mvp-sign\nVersion: 0.0.1\nArchitecture: all\nMaintainer: f <f@example.invalid>\nDescription: fixture\n not a Debian package\n",
    )
    .unwrap();
    std::fs::write(deb_src.join("usr/share/mvp-sign/marker"), b"FIXTURE\n").unwrap();
    let deb = pool.join("mvp-sign_0.0.1_all.deb");
    assert!(std::process::Command::new("dpkg-deb")
        .args(["--root-owner-group", "--build"])
        .arg(&deb_src)
        .arg(&deb)
        .status()
        .unwrap()
        .success());
    let size = std::fs::metadata(&deb).unwrap().len();
    let sha = file_digest("sha256sum", &deb);
    let md5 = file_digest("md5sum", &deb);
    let packages = format!(
        "Package: mvp-sign\nVersion: 0.0.1\nArchitecture: all\nMaintainer: f <f@example.invalid>\nFilename: pool/mvp-sign_0.0.1_all.deb\nSize: {size}\nMD5sum: {md5}\nSHA256: {sha}\nDescription: fixture\n not a Debian package\n"
    );
    let packages_path = dist.join("Packages");
    std::fs::write(&packages_path, &packages).unwrap();
    let psha = file_digest("sha256sum", &packages_path);
    let pmd5 = file_digest("md5sum", &packages_path);
    let psize = std::fs::metadata(&packages_path).unwrap().len();
    let release = format!(
        "Origin: fixture\nLabel: fixture\nSuite: fix\nCodename: fix\nArchitectures: all\nComponents: main\nMD5Sum:\n {pmd5} {psize} main/binary-all/Packages\nSHA256:\n {psha} {psize} main/binary-all/Packages\n"
    );
    let release_path = repo.join("dists/fix/Release");
    std::fs::write(&release_path, &release).unwrap();
    let good = GpgHome(root.join("gnupg-good"));
    let bad = GpgHome(root.join("gnupg-bad"));
    std::fs::create_dir_all(&good.0).unwrap();
    std::fs::create_dir_all(&bad.0).unwrap();
    std::fs::set_permissions(&good.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&bad.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let keys = work.join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    gpg_export(&good.0, &keys.join("good.pgp"));
    gpg_export(&bad.0, &keys.join("bad.pgp"));
    assert!(std::process::Command::new("gpg")
        .env("GNUPGHOME", &good.0)
        .args([
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--clearsign",
            "--output",
        ])
        .arg(repo.join("dists/fix/InRelease"))
        .arg(&release_path)
        .status()
        .unwrap()
        .success());
    let sqv = std::process::Command::new("sqv")
        .args(["--keyring"])
        .arg(keys.join("good.pgp"))
        .args(["--cleartext", "--output"])
        .arg(root.join("sqv-out"))
        .arg(repo.join("dists/fix/InRelease"))
        .status()
        .unwrap();
    assert!(sqv.success(), "fixture sqv");
    let sent = sentinel.display().to_string();
    let script = r#"
import os, subprocess
os.makedirs("/work/apt-parts", exist_ok=True)
open("/work/apt-empty","w").write("")
open("/work/apt-parts/fixture.sources","w").write("Types: deb\nURIs: file:/work/repo\nSuites: fix\nComponents: main\nSigned-By: /work/keys/good.pgp\n")
opts=["-o","Dir::Etc::sourcelist=/work/apt-empty","-o","Dir::Etc::sourceparts=/work/apt-parts","-o","APT::Update::Error-Mode=any","-o","Acquire::Retries=0"]
bad=[]
sent="SENTINEL_PATH"
for base in ("/proc/self/fd","/proc/1/fd"):
    for n in os.listdir(base):
        try:
            t=os.readlink(base+"/"+n)
        except FileNotFoundError:
            continue
        if sent in t or "/etc/apt/auth.conf" in t or "/etc/apt/sources.list" in t:
            bad.append(t)
print("FD_BAD" if bad else "FD_CLEAN")
u=subprocess.run(["apt-get",*opts,"update"],capture_output=True,text=True)
print("FIXTURE_NOT_DEBIAN")
print(u.stdout)
print(u.stderr)
print("UPDATE", u.returncode)
if u.returncode:
    raise SystemExit(u.returncode)
i=subprocess.run(["apt-get",*opts,"install","-y","mvp-sign"],capture_output=True,text=True)
print(i.stdout)
print(i.stderr)
print("INSTALL", i.returncode)
print(open("/usr/share/mvp-sign/marker").read().strip())
raise SystemExit(i.returncode)
"#
    .replace("SENTINEL_PATH", &sent);
    let apt = execute_with(
        owner,
        crate::workspace_runner::Payload::Python(script),
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
    assert!(apt_out.contains("FD_CLEAN"), "{apt_out}");
    assert!(apt_out.contains("newly installed"), "{apt_out}");
    assert!(apt_out.contains("FIXTURE"), "{apt_out}");
    assert!(apt_out.contains("FIXTURE_NOT_DEBIAN"), "{apt_out}");
    assert!(!apt_out.contains("deb.debian.org"), "{apt_out}");
    let wrong = r#"
import subprocess
open("/work/apt-parts/fixture.sources","w").write("Types: deb\nURIs: file:/work/repo\nSuites: fix\nComponents: main\nSigned-By: /work/keys/bad.pgp\n")
opts=["-o","Dir::Etc::sourcelist=/work/apt-empty","-o","Dir::Etc::sourceparts=/work/apt-parts","-o","APT::Update::Error-Mode=any","-o","Acquire::Retries=0"]
u=subprocess.run(["apt-get",*opts,"update"],capture_output=True,text=True)
print(u.stdout)
print(u.stderr)
print("WRONG", u.returncode)
raise SystemExit(0 if u.returncode else 1)
"#;
    let wrong_run = execute_with(
        owner,
        crate::workspace_runner::Payload::Python(wrong.to_string()),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    let wrong_out = format!(
        "{}{}",
        String::from_utf8_lossy(&wrong_run.stdout),
        String::from_utf8_lossy(&wrong_run.stderr)
    );
    assert_eq!(wrong_run.exit_code, 0, "{wrong_out}");
    assert!(wrong_out.contains("WRONG"), "{wrong_out}");
    let bytes = std::fs::read(&packages_path).unwrap();
    let mut flipped = bytes.clone();
    flipped[0] ^= 0x01;
    std::fs::write(&packages_path, flipped).unwrap();
    let tamper = r#"
import subprocess
open("/work/apt-parts/fixture.sources","w").write("Types: deb\nURIs: file:/work/repo\nSuites: fix\nComponents: main\nSigned-By: /work/keys/good.pgp\n")
opts=["-o","Dir::Etc::sourcelist=/work/apt-empty","-o","Dir::Etc::sourceparts=/work/apt-parts","-o","APT::Update::Error-Mode=any","-o","Acquire::Retries=0"]
u=subprocess.run(["apt-get",*opts,"update"],capture_output=True,text=True)
print(u.stdout)
print(u.stderr)
print("TAMPER", u.returncode)
raise SystemExit(0 if u.returncode else 1)
"#;
    let tamper_run = execute_with(
        owner,
        crate::workspace_runner::Payload::Python(tamper.to_string()),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    let tamper_out = format!(
        "{}{}",
        String::from_utf8_lossy(&tamper_run.stdout),
        String::from_utf8_lossy(&tamper_run.stderr)
    );
    assert_eq!(tamper_run.exit_code, 0, "{tamper_out}");
    assert!(tamper_out.contains("TAMPER"), "{tamper_out}");
    std::fs::write(&packages_path, bytes).unwrap();
    drop(good);
    drop(bad);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
#[ignore = "external network prerequisite known DNS failure: deb.debian.org Temporary failure resolving during owner apt-get update; not a proved policy denial. Do not run until an administrator confirms ordinary network is restored. Then: cargo test -p fujiang-plugin-agent --lib --offline workspace_backend::tests::remote_debian_signed_install_persists -- --ignored --exact"]
async fn remote_debian_signed_install_persists() {
    let root = std::env::temp_dir().join(format!(
        "ws-aptnet-{}-{}",
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
    let other = resolved_owner(Source::Friend { id: 8 }, 8).await;
    let script = r#"
import subprocess
opts=["-o","APT::Update::Error-Mode=any","-o","Acquire::Retries=0"]
u=subprocess.run(["apt-get",*opts,"update"],capture_output=True,text=True)
print(u.stdout)
print(u.stderr)
print("UPDATE", u.returncode)
if u.returncode:
    raise SystemExit(u.returncode)
i=subprocess.run(["apt-get",*opts,"install","-y","sensible-utils"],capture_output=True,text=True)
print(i.stdout)
print(i.stderr)
print("INSTALL", i.returncode)
raise SystemExit(i.returncode)
"#;
    let apt = execute_with(
        owner,
        crate::workspace_runner::Payload::Python(script.to_string()),
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
            "print(open('/usr/bin/sensible-pager').read(20))".to_string(),
        ),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    assert_eq!(
        kept.exit_code,
        0,
        "{}",
        String::from_utf8_lossy(&kept.stderr)
    );
    let other_run = execute_with(
        other,
        crate::workspace_runner::Payload::Python(
            "import os; print('ABSENT' if not os.path.exists('/usr/bin/sensible-pager') else 'PRESENT')"
                .to_string(),
        ),
        &auth,
        crate::workspace_runner::run,
    )
    .unwrap();
    let other_out = String::from_utf8_lossy(&other_run.stdout);
    assert_eq!(
        other_run.exit_code,
        0,
        "{}",
        String::from_utf8_lossy(&other_run.stderr)
    );
    assert!(other_out.contains("ABSENT"), "{other_out}");
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"HOST-SENTINEL");
    let _ = std::fs::remove_dir_all(root);
}
