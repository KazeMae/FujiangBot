//! Non-production admin probe. No `acceptance.json`, no `approved`, no production env.
//! Owner ids below are fixture bindings, not online QQ.

use crate::workspace::{GroupChat, PersonalQq, WorkspaceOwner};
use crate::workspace_backend::{self, RootfsAuthority};
use crate::workspace_runner::{self, Payload, RunResult};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const TOOLS: [&str; 5] = [
    "/usr/bin/gpgv",
    "/usr/bin/newuidmap",
    "/usr/bin/newgidmap",
    "/usr/bin/bwrap",
    "/usr/bin/python3",
];
const META: [&str; 3] = ["SHA256SUMS", "SHA256SUMS.gpg", "ubuntu-archive-keyring.gpg"];
const CASES: [&str; 9] = [
    "shell",
    "persist",
    "isolate",
    "apt",
    "root-iso",
    "python",
    "proc-fd",
    "maps-caps",
    "sgid",
];
const OWNER_A: i64 = 910_001;
const OWNER_B: i64 = 910_002;
const STATUS_PATH: &str = "/var/lib/dpkg/status";
static NAMES: AtomicU64 = AtomicU64::new(1);

fn unavailable(what: &str) -> io::Error {
    io::Error::other(format!("unavailable: {what}"))
}

struct WorkGuard {
    path: PathBuf,
    armed: bool,
}

impl WorkGuard {
    fn release(mut self) -> io::Result<()> {
        self.armed = false;
        match fs::remove_dir_all(&self.path) {
            Ok(()) if !self.path.exists() => Ok(()),
            Ok(()) => Err(unavailable(&format!("residual {}", self.path.display()))),
            Err(err) => Err(io::Error::other(format!(
                "cleanup unaccepted residual {}: {err}",
                self.path.display()
            ))),
        }
    }
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match fs::remove_dir_all(&self.path) {
            Ok(()) if !self.path.exists() => {}
            _ => eprintln!("cleanup unaccepted residual {}", self.path.display()),
        }
    }
}

fn fresh_path(label: &str) -> PathBuf {
    let n = NAMES.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("fujiang-{label}-{}-{n}", std::process::id()))
}

fn private_parent(label: &str) -> PathBuf {
    let path = fresh_path(label);
    fs::create_dir(&path).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o700);
    fs::set_permissions(&path, perms).unwrap();
    path
}

fn normal_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

fn anchored(path: &Path) -> io::Result<PathBuf> {
    if !normal_path(path) {
        return Err(unavailable("path"));
    }
    if path.exists() {
        return path.canonicalize().map_err(|_| unavailable("path"));
    }
    let parent = path.parent().ok_or_else(|| unavailable("path"))?;
    let name = path.file_name().ok_or_else(|| unavailable("path"))?;
    Ok(parent
        .canonicalize()
        .map_err(|_| unavailable("path"))?
        .join(name))
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a == b || a.starts_with(b) || b.starts_with(a)
}

fn touches_production(path: &Path) -> bool {
    let Ok(raw) = std::env::var("FUJIANG_WORKSPACE_ACCEPTANCE") else {
        return false;
    };
    if raw.is_empty() {
        return false;
    }
    let Ok(here) = anchored(path) else {
        return true;
    };
    match anchored(Path::new(&raw)) {
        Ok(prod) => overlaps(&here, &prod),
        Err(_) => true,
    }
}

fn claim_new(path: &Path) -> io::Result<WorkGuard> {
    if !normal_path(path) || path.exists() || touches_production(path) {
        return Err(unavailable("work"));
    }
    let parent = path.parent().ok_or_else(|| unavailable("work"))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| unavailable("work"))?;
    let name = super::one_component(name)?;
    let parent_fd = super::open_mode(parent, 0o700)?;
    if unsafe { libc::mkdirat(parent_fd.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        return Err(unavailable("work"));
    }
    Ok(WorkGuard {
        path: path.to_path_buf(),
        armed: true,
    })
}

fn open_private_source(path: &Path) -> io::Result<()> {
    if !normal_path(path) {
        return Err(unavailable("source"));
    }
    super::open_mode(path, 0o700)?;
    Ok(())
}

fn archive_present(source: &Path) -> bool {
    fs::read_dir(source)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|ent| ent.ok())
        .any(|ent| {
            ent.file_type().map(|kind| kind.is_file()).unwrap_or(false)
                && ent.file_name().as_bytes().ends_with(b".tar.gz")
        })
}

fn missing_parts(source: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    if open_private_source(source).is_err() {
        missing.push("source".into());
    }
    for tool in TOOLS {
        if fs::metadata(tool).map(|m| m.is_file()).unwrap_or(false) {
            continue;
        }
        missing.push(tool.into());
    }
    if source.is_dir() {
        for name in META {
            if !source.join(name).is_file() {
                missing.push(name.into());
            }
        }
        if !archive_present(source) {
            missing.push("archive".into());
        }
    }
    missing
}

fn separated(source: &Path, work: &Path) -> io::Result<()> {
    let source = anchored(source)?;
    let work = anchored(work)?;
    if overlaps(&source, &work) || touches_production(&source) || touches_production(&work) {
        return Err(unavailable("overlap"));
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
struct PinnedIdentity {
    archive: String,
    sha256: String,
    bytes: u64,
    tree: [u8; 32],
    primary: String,
    policy: String,
    extracted: bool,
    execution_accepted: bool,
}

impl PinnedIdentity {
    fn from_receipt(receipt: &super::Receipt) -> Self {
        Self {
            archive: receipt.archive.clone(),
            sha256: receipt.sha256.clone(),
            bytes: receipt.bytes,
            tree: receipt.tree,
            primary: receipt.primary.clone(),
            policy: receipt.tree_policy.clone(),
            extracted: receipt.extracted,
            execution_accepted: receipt.execution_accepted,
        }
    }

    fn text(&self) -> String {
        let tree = self
            .tree
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!(
            "archive={} sha256={} bytes={} tree={tree} primary={} policy={} extracted={} execution_accepted={}",
            self.archive,
            self.sha256,
            self.bytes,
            self.primary,
            self.policy,
            self.extracted,
            self.execution_accepted
        )
    }
}

fn pin_first(slot: &mut Option<PinnedIdentity>, next: PinnedIdentity) -> Result<(), String> {
    match slot {
        None => {
            *slot = Some(next);
            Ok(())
        }
        Some(first) if first == &next => Ok(()),
        Some(_) => Err("identity mismatch".into()),
    }
}

struct AdminAuth<'a> {
    source: &'a Path,
    work: &'a Path,
    identity: std::sync::Mutex<Option<PinnedIdentity>>,
}

impl RootfsAuthority for AdminAuth<'_> {
    fn acquire(&self) -> anyhow::Result<(PathBuf, crate::workspace_store::VerifiedBase)> {
        separated(self.source, self.work)?;
        let missing = missing_parts(self.source);
        if !missing.is_empty() {
            anyhow::bail!("unavailable: {}", missing.join(", "));
        }
        let (base, pinned) = seal_real(self.source, self.work)?;
        let mut slot = self.identity.lock().unwrap_or_else(|err| err.into_inner());
        if let Err(err) = pin_first(&mut slot, pinned) {
            drop(slot);
            drop(base);
            anyhow::bail!("{err}");
        }
        Ok((self.work.to_path_buf(), base))
    }
}

fn seal_real(
    source: &Path,
    parent: &Path,
) -> io::Result<(crate::workspace_store::VerifiedBase, PinnedIdentity)> {
    let parent_fd = super::open_mode(parent, 0o700)?;
    let name = format!("probe-{}-{}", std::process::id(), super::nanos()?);
    let held = super::leases::create_owned(parent_fd.as_raw_fd(), &name)?;
    let outcome = (|| {
        let target = parent.join(&name).join("snapshot");
        let stdout = super::run_verifier(source, &target, held.lease_fd())?;
        let snap = super::open_dir(held.dir_fd(), &super::one_component("snapshot")?)?;
        let filed = super::read_exact_file(snap.as_raw_fd(), "receipt.json", 8192)?;
        let from_file = super::parse_receipt(&filed)?;
        let from_out = super::parse_receipt(&stdout)?;
        if from_file != from_out || from_out.execution_accepted || !from_out.extracted {
            return Err(io::Error::new(ErrorKind::InvalidInput, "receipt"));
        }
        let unpacked = super::open_dir(snap.as_raw_fd(), &super::one_component("rootfs")?)?;
        let pinned = PinnedIdentity::from_receipt(&from_out);
        super::seal(parent_fd.as_raw_fd(), unpacked.as_raw_fd(), from_out.tree)
            .map(|base| (base, pinned))
    })();
    drop(held);
    outcome
}

fn owner_personal(id: i64) -> Result<WorkspaceOwner, String> {
    PersonalQq::new(id)
        .map(WorkspaceOwner::Personal)
        .ok_or_else(|| "owner".into())
}

fn owner_group(id: i64) -> Result<WorkspaceOwner, String> {
    GroupChat::new(id)
        .map(WorkspaceOwner::Group)
        .ok_or_else(|| "owner".into())
}

fn exec(
    auth: &impl RootfsAuthority,
    owner: WorkspaceOwner,
    payload: Payload,
) -> Result<RunResult, String> {
    workspace_backend::execute_with(owner, payload, auth, workspace_runner::run)
        .map_err(|err| err.to_string())
}

fn show(result: &RunResult) -> String {
    format!(
        "code {} timeout {} stdout {} stderr {}",
        result.exit_code,
        result.timed_out,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    )
}

fn need_ok(result: &RunResult, needle: &str) -> Result<(), String> {
    let out = String::from_utf8_lossy(&result.stdout);
    if result.timed_out || result.exit_code != 0 || !out.contains(needle) {
        Err(format!("failed {needle}: {}", show(result)))
    } else {
        Ok(())
    }
}

fn case_line(name: &str, body: &str, identity: &str) -> String {
    if identity.is_empty() {
        format!("{name}: {body}\n")
    } else {
        format!("{name}: {body} {identity}\n")
    }
}

fn drive(
    mut step: impl FnMut(&str) -> Result<(), String>,
    identity: impl Fn() -> String,
) -> String {
    let mut report = String::new();
    let mut stop = false;
    for name in CASES {
        if stop {
            report.push_str(&case_line(name, "unrun", &identity()));
            continue;
        }
        let body = match step(name) {
            Ok(()) => "ok".to_string(),
            Err(err) => {
                stop = true;
                err
            }
        };
        report.push_str(&case_line(name, &body, &identity()));
    }
    report
}

fn unrun_report() -> String {
    CASES
        .iter()
        .map(|name| format!("{name}: unrun\n"))
        .collect()
}

fn report_bound(report: &str, identity: &str) -> bool {
    if identity.is_empty()
        || !identity.contains(" sha256=")
        || !identity.contains(" bytes=")
        || !identity.contains(" tree=")
        || !identity.contains(" primary=")
        || !identity.contains(" policy=")
        || !identity.contains(" extracted=true")
        || !identity.contains(" execution_accepted=false")
    {
        return false;
    }
    let mut lines = report.lines().filter(|line| !line.is_empty());
    for name in CASES {
        let Some(line) = lines.next() else {
            return false;
        };
        if line != format!("{name}: ok {identity}") {
            return false;
        }
    }
    lines.next().is_none()
}

fn host_status_digest() -> io::Result<Vec<u8>> {
    let bytes = fs::read(STATUS_PATH)?;
    Ok(Sha256::digest(bytes).to_vec())
}

fn hold_nonce(path: &Path) -> io::Result<(OwnedFd, String)> {
    let nonce = format!(
        "nonce-{}-{}",
        std::process::id(),
        NAMES.fetch_add(1, Ordering::Relaxed)
    );
    create_new(path, nonce.as_bytes())?;
    let c =
        std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| unavailable("nonce"))?;
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((unsafe { OwnedFd::from_raw_fd(fd) }, nonce))
}

fn hold_dir(path: &Path) -> io::Result<OwnedFd> {
    let c =
        std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| unavailable("dirfd"))?;
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn needs_restore(apt_attempted: bool, report: &str) -> bool {
    apt_attempted
        || report
            .lines()
            .any(|line| line.starts_with("sgid:") && !line.starts_with("sgid: unrun"))
}

fn case_shell(auth: &impl RootfsAuthority) -> Result<(), String> {
    let got = exec(
        auth,
        owner_personal(OWNER_A)?,
        Payload::Bash("set -eu; printf fujiang-sh".into()),
    )?;
    need_ok(&got, "fujiang-sh")
}

fn case_python(auth: &impl RootfsAuthority) -> Result<(), String> {
    let got = exec(
        auth,
        owner_personal(OWNER_A)?,
        Payload::Python("print('fujiang-py')".into()),
    )?;
    let err = String::from_utf8_lossy(&got.stderr);
    if err.contains("No such file") {
        return Err(format!("MISSING python unaccepted: {err}"));
    }
    need_ok(&got, "fujiang-py")
}

fn case_persist(auth: &impl RootfsAuthority) -> Result<(), String> {
    let wrote = exec(
        auth,
        owner_personal(OWNER_A)?,
        Payload::Bash("set -eu; printf kept >/work/kept".into()),
    )?;
    need_ok(&wrote, "")?;
    let read = exec(
        auth,
        owner_personal(OWNER_A)?,
        Payload::Bash("set -eu; cat /work/kept".into()),
    )?;
    need_ok(&read, "kept")
}

fn absence_script() -> &'static str {
    "set -eu; if [ -e /work/kept ]; then echo PRESENT; exit 1; fi; echo ABSENT"
}

fn case_isolate(auth: &impl RootfsAuthority) -> Result<(), String> {
    for owner in [owner_group(OWNER_A)?, owner_personal(OWNER_B)?] {
        let got = exec(auth, owner, Payload::Bash(absence_script().into()))?;
        need_ok(&got, "ABSENT")?;
    }
    Ok(())
}

fn case_proc_fd(
    auth: &impl RootfsAuthority,
    nonce: &str,
    host: &Path,
    other: &Path,
) -> Result<(), String> {
    let script = format!(
        r#"
import os, stat
nonce={nonce:?}
paths=[{host:?},{other:?}]
def blocked(path):
    try:
        fd=os.open(path, os.O_RDONLY|os.O_NOFOLLOW)
    except OSError:
        return True
    os.close(fd)
    return False
for path in paths:
    for root in ("", "/proc/self/root", "/proc/1/root"):
        if not blocked(root+path):
            raise SystemExit("ESCAPE "+root+path)
for name in os.listdir("/proc/self/fd"):
    fd=int(name)
    if fd<3:
        continue
    try:
        st=os.fstat(fd)
    except OSError:
        continue
    if stat.S_ISDIR(st.st_mode):
        raise SystemExit("DIR FD")
    if not stat.S_ISREG(st.st_mode):
        continue
    try:
        data=os.pread(fd, 128, 0)
    except OSError:
        continue
    if nonce.encode() in data:
        raise SystemExit("FD LEAK")
print("closed")
"#
    );
    let got = exec(auth, owner_personal(OWNER_A)?, Payload::Python(script))?;
    let err = String::from_utf8_lossy(&got.stderr);
    if err.contains("No such file") {
        return Err(format!("MISSING python unaccepted: {err}"));
    }
    need_ok(&got, "closed")
}

fn case_maps(auth: &impl RootfsAuthority) -> Result<(), String> {
    let euid = unsafe { libc::geteuid() };
    let egid = unsafe { libc::getegid() };
    let script = format!(
        r#"
def rows(path):
    out=[]
    for line in open(path):
        parts=line.split()
        if len(parts)!=3:
            raise SystemExit(path)
        out.append(tuple(int(x) for x in parts))
    return out
uid=rows("/proc/self/uid_map")
gid=rows("/proc/self/gid_map")
assert uid==[(0,{euid},1),(1,100000,65536)], uid
assert gid==[(0,{egid},1),(1,100000,65536)], gid
assert open("/proc/self/setgroups").read()=="allow\n"
import os
assert os.geteuid()==0 and os.getegid()==0
text=open("/proc/self/status").read()
assert "NoNewPrivs:\t1" in text
cap=int([ln.split()[1] for ln in text.splitlines() if ln.startswith("CapEff:")][0],16)
assert cap==0xdb, hex(cap)
print("maps-ok")
"#
    );
    let got = exec(auth, owner_personal(OWNER_A)?, Payload::Python(script))?;
    let err = String::from_utf8_lossy(&got.stderr);
    if err.contains("No such file") {
        return Err(format!("MISSING python unaccepted: {err}"));
    }
    need_ok(&got, "maps-ok")
}

fn case_sgid(auth: &impl RootfsAuthority) -> Result<(), String> {
    let script = r#"
import os
os.setgroups([])
assert os.getegid()!=2 and 2 not in os.getgroups()
fd=os.open("/tmp/sgid-probe", os.O_CREAT|os.O_RDWR|os.O_NOFOLLOW, 0o644)
os.close(fd)
os.chown("/tmp/sgid-probe", 1, 2)
os.chmod("/tmp/sgid-probe", 0o2755)
st=os.lstat("/tmp/sgid-probe")
assert st.st_uid==1 and st.st_gid==2 and (st.st_mode & 0o7777)==0o2755, (st.st_uid, st.st_gid, oct(st.st_mode))
pid=os.fork()
if pid==0:
    os.setgid(1)
    os.setuid(1)
    if os.geteuid()!=1 or os.getegid()!=1:
        os._exit(4)
    os._exit(0)
wpid, status=os.waitpid(pid, 0)
assert wpid==pid and os.WIFEXITED(status) and os.WEXITSTATUS(status)==0
print("sgid-ok")
"#;
    let got = exec(
        auth,
        owner_personal(OWNER_A)?,
        Payload::Python(script.into()),
    )?;
    let err = String::from_utf8_lossy(&got.stderr);
    if err.contains("No such file") {
        return Err(format!("MISSING python unaccepted: {err}"));
    }
    need_ok(&got, "sgid-ok")
}

fn create_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn case_apt(
    auth: &impl RootfsAuthority,
    before: &[u8],
    sentinel: &Path,
    attempted: &mut bool,
) -> Result<(), String> {
    *attempted = true;
    let script = apt_guest_script();
    let got = exec(auth, owner_personal(OWNER_A)?, Payload::Bash(script.into()))?;
    let out = String::from_utf8_lossy(&got.stdout);
    let err = String::from_utf8_lossy(&got.stderr);
    if is_missing_apt(&out) {
        return Err(format!("MISSING apt unaccepted: {err}"));
    }
    if out.lines().any(|line| line == "PRECONDITION") {
        return Err("apt hello already present: unmet precondition".into());
    }
    if !apt_records_python(&out) {
        return Err(format!("apt hid initial python state: {}", show(&got)));
    }
    if got.exit_code != 0 || got.timed_out || !out.contains("APT_OK") {
        return Err(format!("apt failed unaccepted: {}", show(&got)));
    }
    let after = host_status_digest().map_err(|err| format!("host status unaccepted: {err}"))?;
    if after != before || fs::read(sentinel).ok().as_deref() != Some(b"sentinel") {
        return Err("host changed".into());
    }
    Ok(())
}

fn is_missing_apt(out: &str) -> bool {
    out.lines().any(|line| line == "MISSING")
}

fn apt_guest_script() -> &'static str {
    r#"set -eu
if ! command -v apt-get >/dev/null; then echo MISSING; exit 2; fi
if dpkg-query -W -f '${Status}' hello 2>/dev/null | grep -qx 'install ok installed'; then echo PRECONDITION; exit 3; fi
if command -v python3 >/dev/null; then
  echo PYTHON_PRESENT
  pkgs="hello"
else
  echo MISSING_PYTHON
  if dpkg-query -W -f '${Status}' python3-minimal 2>/dev/null | grep -qx 'install ok installed'; then echo PRECONDITION; exit 3; fi
  pkgs="hello python3-minimal"
fi
mkdir -p /etc/apt/apt.conf.d
printf 'Acquire::Retries "0";\n' >/etc/apt/apt.conf.d/00no-retry
apt-get -o Acquire::Retries=0 update
apt-get -o Acquire::Retries=0 install -y --no-install-recommends $pkgs
test "$(dpkg-query -W -f '${Status}' hello)" = "install ok installed"
if [ "$pkgs" = "hello python3-minimal" ]; then test "$(dpkg-query -W -f '${Status}' python3-minimal)" = "install ok installed"; fi
hello
command -v python3
echo APT_OK
"#
}

fn apt_records_python(out: &str) -> bool {
    let absent = out.lines().any(|line| line == "MISSING_PYTHON");
    let present = out.lines().any(|line| line == "PYTHON_PRESENT");
    absent != present
}

fn root_mark_script() -> &'static str {
    "set -eu; printf a >/etc/fujiang-root-mark; test -f /etc/fujiang-root-mark; echo ROOTMARK"
}

fn root_absent_script() -> &'static str {
    r#"set -eu
command -v dpkg-query >/dev/null
if [ -e /etc/fujiang-root-mark ]; then echo PRESENT; exit 1; fi
dpkg-query -W -f '${Package} ${Status}\n' >/tmp/dpkg-snap
while IFS= read -r line; do
  if [ "$line" = "hello install ok installed" ]; then echo PRESENT; exit 1; fi
done </tmp/dpkg-snap
echo ABSENT
"#
}

fn case_root_iso(auth: &impl RootfsAuthority) -> Result<(), String> {
    let marked = exec(
        auth,
        owner_personal(OWNER_A)?,
        Payload::Bash(root_mark_script().into()),
    )?;
    need_ok(&marked, "ROOTMARK")?;
    for owner in [owner_group(OWNER_A)?, owner_personal(OWNER_B)?] {
        let got = exec(auth, owner, Payload::Bash(root_absent_script().into()))?;
        need_ok(&got, "ABSENT")?;
    }
    Ok(())
}

fn bash_restore_script() -> &'static str {
    r#"set -eu
command -v chown >/dev/null
command -v find >/dev/null
command -v chmod >/dev/null
for top in /tmp /var /work; do
  [ -d "$top" ] || continue
  chown -hRP 0:0 "$top"
  find -P "$top" -type d -exec chmod 0755 {} +
done
echo restored
"#
}

fn guest_restore(auth: &impl RootfsAuthority, report: &str) -> Result<(), String> {
    if !report.lines().any(|line| line.starts_with("python: ok")) {
        let got = exec(
            auth,
            owner_personal(OWNER_A)?,
            Payload::Bash(bash_restore_script().into()),
        )?;
        let out = String::from_utf8_lossy(&got.stdout);
        let err = String::from_utf8_lossy(&got.stderr);
        if got.exit_code != 0 || !out.contains("restored") {
            return Err(format!("MISSING cleanup unaccepted: {err} {out}"));
        }
        return Ok(());
    }
    python_restore(auth)
}

fn python_restore(auth: &impl RootfsAuthority) -> Result<(), String> {
    let script = r#"
import os
skip={"/proc","/sys","/dev","/run"}
def restore(top):
    def boom(err):
        raise err
    for dirpath, dirnames, filenames in os.walk(top, followlinks=False, onerror=boom):
        kept=[]
        for name in dirnames:
            path=os.path.join(dirpath, name)
            if path in skip or os.path.islink(path) or os.path.ismount(path):
                continue
            kept.append(name)
        dirnames[:] = kept
        for name in kept+filenames:
            path=os.path.join(dirpath, name)
            if os.path.islink(path):
                continue
            try:
                os.chown(path, 0, 0, follow_symlinks=False)
            except OSError:
                raise SystemExit("restore failed "+path)
            if name in kept:
                os.chmod(path, 0o755)
print("restored")
for top in ("/tmp", "/var", "/work"):
    if os.path.isdir(top):
        restore(top)
"#;
    let got = exec(
        auth,
        owner_personal(OWNER_A)?,
        Payload::Python(script.into()),
    )?;
    let err = String::from_utf8_lossy(&got.stderr);
    if err.contains("No such file") {
        return Err(format!("MISSING python cleanup unaccepted: {err}"));
    }
    need_ok(&got, "restored")
}

#[test]
#[ignore = "admin real rootfs: FUJIANG_ROOTFS_SOURCE and FUJIANG_ROOTFS_WORK"]
fn admin_real_rootfs_acceptance() {
    let source =
        PathBuf::from(std::env::var_os("FUJIANG_ROOTFS_SOURCE").expect("FUJIANG_ROOTFS_SOURCE"));
    let work = PathBuf::from(std::env::var_os("FUJIANG_ROOTFS_WORK").expect("FUJIANG_ROOTFS_WORK"));
    let mut missing = missing_parts(&source);
    if let Err(err) = separated(&source, &work) {
        missing.push(err.to_string());
    }
    if let Err(err) = host_status_digest() {
        missing.push(format!("dpkg-status: {err}"));
    }
    if !missing.is_empty() {
        let report = unrun_report();
        panic!("unavailable: {}\n{report}", missing.join(", "));
    }
    let guard = claim_new(&work).expect("work must be a new leaf under a private 0700 parent");
    let auth = AdminAuth {
        source: &source,
        work: &guard.path,
        identity: std::sync::Mutex::new(None),
    };
    let nonce_path = guard.path.join("nonce");
    let other_path = guard.path.join("owner-b-sentinel");
    let sentinel = guard.path.join("sentinel");
    let (_fd, nonce) = hold_nonce(&nonce_path).unwrap();
    let _outer = hold_dir(&guard.path).unwrap();
    let mut apt_attempted = false;
    create_new(&other_path, b"owner-b-sentinel").unwrap();
    create_new(&sentinel, b"sentinel").unwrap();
    let before = host_status_digest().unwrap();
    let report = drive(
        |name| match name {
            "shell" => case_shell(&auth),
            "persist" => case_persist(&auth),
            "isolate" => case_isolate(&auth),
            "apt" => case_apt(&auth, &before, &sentinel, &mut apt_attempted),
            "root-iso" => case_root_iso(&auth),
            "python" => case_python(&auth),
            "proc-fd" => case_proc_fd(&auth, &nonce, &nonce_path, &other_path),
            "maps-caps" => case_maps(&auth),
            "sgid" => case_sgid(&auth),
            other => Err(format!("unknown {other}")),
        },
        || {
            auth.identity
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .as_ref()
                .map(PinnedIdentity::text)
                .unwrap_or_default()
        },
    );
    eprintln!("{report}");
    let restore = if needs_restore(apt_attempted, &report) {
        Some(guest_restore(&auth, &report))
    } else {
        None
    };
    let residual = guard.path.clone();
    let pinned = auth
        .identity
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .as_ref()
        .map(PinnedIdentity::text)
        .unwrap_or_default();
    drop(auth);
    let released = guard.release();
    if let Some(Err(err)) = restore {
        panic!(
            "cleanup unaccepted residual {}: {err}\n{report}",
            residual.display()
        );
    }
    if let Err(err) = released {
        panic!(
            "cleanup unaccepted residual {}: {err}\n{report}",
            residual.display()
        );
    }
    assert!(report_bound(&report, &pinned), "{report}");
}

#[cfg(test)]
#[path = "workspace_rootfs_probe_tests.rs"]
mod probe_tests;
