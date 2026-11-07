use std::ffi::CString;
use std::io::{self, ErrorKind, Read};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::workspace_seed::{self, VerifiedBase};
use super::{fstat, invalid, one_component, open_host, openat2, raw_owned, tree_digest};

const PRIMARY: &str = "843938DF228D22F7B3742BC0D94AA3F0EFE21092";
const POLICY: &str = "workspace-rootfs-runtime-v1";
const EMBEDDED_VERIFIER: &str = include_str!("../../../../scripts/verify-workspace-bundle.py");

struct Approval {
    source: PathBuf,
    host: PathBuf,
    archive_sha256: String,
    tree: [u8; 32],
}

#[derive(PartialEq, Eq)]
struct Receipt {
    archive: String,
    sha256: String,
    bytes: u64,
    primary: String,
    extracted: bool,
    execution_accepted: bool,
    tree: [u8; 32],
    tree_policy: String,
}

pub(super) fn acquire(trust_path: &Path) -> io::Result<(PathBuf, VerifiedBase)> {
    let name = format!("w{}-{}", std::process::id(), nanos()?);
    acquire_named(trust_path, &name)
}

fn acquire_named(trust_path: &Path, wrapper: &str) -> io::Result<(PathBuf, VerifiedBase)> {
    let trust = open_mode(trust_path, 0o700)?;
    let approval = read_approval(trust.as_raw_fd())?;
    let _host = open_mode(Path::new(&approval.host), 0o700)?;
    let wrapper_name = one_component(wrapper)?;
    if unsafe { libc::mkdirat(trust.as_raw_fd(), wrapper_name.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let outcome = (|| {
        let wrapper_fd = open_dir(trust.as_raw_fd(), &wrapper_name)?;
        let snap_name = one_component("snapshot")?;
        let stdout = run_verifier(&approval.source, &trust_path.join(wrapper).join("snapshot"))?;
        let snap_fd = open_dir(wrapper_fd.as_raw_fd(), &snap_name)?;
        let filed = read_exact_file(snap_fd.as_raw_fd(), "receipt.json", 8192)?;
        let from_file = parse_receipt(&filed)?;
        let from_out = parse_receipt(&stdout)?;
        if from_file != from_out {
            return Err(invalid("receipt"));
        }
        bind(&approval, &from_out)?;
        let unpacked = open_dir(snap_fd.as_raw_fd(), &one_component("rootfs")?)?;
        seal(trust.as_raw_fd(), unpacked.as_raw_fd(), approval.tree)
    })();
    let _ = workspace_seed::remove_tree(trust.as_raw_fd(), &wrapper_name);
    outcome.map(|base| (approval.host, base))
}

fn bind(approval: &Approval, receipt: &Receipt) -> io::Result<()> {
    if receipt.execution_accepted || !receipt.extracted {
        return Err(invalid("receipt"));
    }
    if receipt.primary != PRIMARY
        || receipt.tree_policy != POLICY
        || receipt.sha256 != approval.archive_sha256
        || receipt.tree != approval.tree
    {
        return Err(invalid("stale"));
    }
    if receipt.bytes == 0 || receipt.bytes > 512 * 1024 * 1024 {
        return Err(invalid("receipt"));
    }
    if receipt.archive.is_empty()
        || receipt.archive.len() > 240
        || receipt.archive.bytes().any(|b| b == b'/' || b < 0x20)
    {
        return Err(invalid("receipt"));
    }
    Ok(())
}

fn seal(trust: RawFd, source: RawFd, approved: [u8; 32]) -> io::Result<VerifiedBase> {
    let name = one_component(&format!("live-{}-{}", std::process::id(), nanos()?))?;
    if unsafe { libc::mkdirat(trust, name.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let sealed = (|| {
        let parent = open_dir(trust, &name)?;
        if unsafe { libc::mkdirat(parent.as_raw_fd(), one_component("rootfs")?.as_ptr(), 0o700) }
            != 0
        {
            return Err(io::Error::last_os_error());
        }
        let dest = open_dir(parent.as_raw_fd(), &one_component("rootfs")?)?;
        workspace_seed::copy_tree(source, dest.as_raw_fd())?;
        let got = tree_digest(dest.as_raw_fd())?;
        if got != approved {
            return Err(invalid("tree"));
        }
        let again = tree_digest(dest.as_raw_fd())?;
        if again != got {
            return Err(invalid("tree"));
        }
        VerifiedBase::from_sealed(dest, approved, dup_fd(trust)?, name.clone())
    })();
    if sealed.is_err() {
        let _ = workspace_seed::remove_tree(trust, &name);
    }
    sealed
}

fn read_approval(trust: RawFd) -> io::Result<Approval> {
    parse_approval(&read_exact_file(trust, "acceptance.json", 8192)?)
}

fn parse_approval(bytes: &[u8]) -> io::Result<Approval> {
    let map = json_object(bytes, "approval")?;
    const KEYS: [&str; 9] = [
        "version",
        "kind",
        "source",
        "host",
        "archive_sha256",
        "primary",
        "policy",
        "tree_sha256",
        "approved",
    ];
    if map.len() != KEYS.len() || KEYS.iter().any(|key| !map.contains_key(*key)) {
        return Err(invalid("approval"));
    }
    if map["version"] != 1
        || map["kind"] != "runtime_safety_approval"
        || map["approved"] != true
        || map["policy"] != POLICY
        || map["primary"] != PRIMARY
    {
        return Err(invalid("approval"));
    }
    let archive = map["archive_sha256"]
        .as_str()
        .ok_or_else(|| invalid("approval"))?;
    let tree_hex = map["tree_sha256"]
        .as_str()
        .ok_or_else(|| invalid("approval"))?;
    Ok(Approval {
        source: abs_path(map["source"].as_str().ok_or_else(|| invalid("approval"))?)?,
        host: abs_path(map["host"].as_str().ok_or_else(|| invalid("approval"))?)?,
        archive_sha256: parse_hex_text(archive)?,
        tree: parse_hex32(tree_hex)?,
    })
}

fn parse_receipt(bytes: &[u8]) -> io::Result<Receipt> {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let map = json_object(bytes, "receipt")?;
    const KEYS: [&str; 8] = [
        "archive",
        "sha256",
        "bytes",
        "primary",
        "extracted",
        "execution_accepted",
        "tree_sha256",
        "tree_policy",
    ];
    if map.len() != KEYS.len() || KEYS.iter().any(|key| !map.contains_key(*key)) {
        return Err(invalid("receipt"));
    }
    let sha = map["sha256"].as_str().ok_or_else(|| invalid("receipt"))?;
    let tree = map["tree_sha256"]
        .as_str()
        .ok_or_else(|| invalid("receipt"))?;
    Ok(Receipt {
        archive: map["archive"]
            .as_str()
            .ok_or_else(|| invalid("receipt"))?
            .to_string(),
        sha256: parse_hex_text(sha)?,
        bytes: map["bytes"].as_u64().ok_or_else(|| invalid("receipt"))?,
        primary: map["primary"]
            .as_str()
            .ok_or_else(|| invalid("receipt"))?
            .to_string(),
        extracted: map["extracted"]
            .as_bool()
            .ok_or_else(|| invalid("receipt"))?,
        execution_accepted: map["execution_accepted"]
            .as_bool()
            .ok_or_else(|| invalid("receipt"))?,
        tree: parse_hex32(tree)?,
        tree_policy: map["tree_policy"]
            .as_str()
            .ok_or_else(|| invalid("receipt"))?
            .to_string(),
    })
}

fn json_object(
    bytes: &[u8],
    label: &'static str,
) -> io::Result<serde_json::Map<String, serde_json::Value>> {
    if bytes.is_empty() || bytes.contains(&0) {
        return Err(invalid(label));
    }
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        _ => Err(invalid(label)),
    }
}

fn run_verifier(source: &Path, target: &Path) -> io::Result<Vec<u8>> {
    let mut cmd = Command::new("/usr/bin/python3");
    cmd.arg("-I")
        .arg("-c")
        .arg(EMBEDDED_VERIFIER)
        .arg("--unpack")
        .arg(source)
        .arg(target)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/");
    collect_command(cmd, Instant::now() + Duration::from_secs(40), 8192, 65536)
}

fn collect_command(
    mut cmd: Command,
    deadline: Instant,
    out_cap: usize,
    err_cap: usize,
) -> io::Result<Vec<u8>> {
    cmd.process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return Err(invalid("verifier")),
    };
    let output = drive(&mut child, deadline, out_cap, err_cap);
    // Leader is still unreaped, so its pid is the process group id.
    if output.is_err() {
        kill_group(child.id());
    }
    let reaped = reap_direct(&mut child);
    match (output, reaped) {
        (Ok(buf), Ok(())) => Ok(buf),
        (Err(err), _) => Err(err),
        (_, Err(err)) => Err(err),
    }
}

fn drive(
    child: &mut Child,
    deadline: Instant,
    out_cap: usize,
    err_cap: usize,
) -> io::Result<Vec<u8>> {
    let pid = child.id();
    let mut stdout = match child.stdout.take() {
        Some(pipe) => pipe,
        None => return Err(invalid("verifier")),
    };
    let mut stderr = match child.stderr.take() {
        Some(pipe) => pipe,
        None => return Err(invalid("verifier")),
    };
    set_nonblock(stdout.as_raw_fd())?;
    set_nonblock(stderr.as_raw_fd())?;
    let mut out = Vec::new();
    let mut err_n = 0usize;
    let mut out_done = false;
    let mut err_done = false;
    loop {
        if Instant::now() >= deadline {
            return Err(invalid("verifier"));
        }
        if !out_done || !err_done {
            let mut fds = [
                libc::pollfd {
                    fd: stdout.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: stderr.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, 50) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.kind() != ErrorKind::Interrupted {
                    return Err(err);
                }
            } else {
                if !out_done && fds[0].revents != 0 {
                    let room = out_cap.saturating_sub(out.len());
                    match pump(&mut stdout, Some(&mut out), room) {
                        Ok(true) => {}
                        Ok(false) => out_done = true,
                        Err(err) => return Err(err),
                    }
                }
                if !err_done && fds[1].revents != 0 {
                    let room = err_cap.saturating_sub(err_n);
                    let mut discard = Vec::new();
                    match pump(&mut stderr, Some(&mut discard), room) {
                        Ok(true) => err_n += discard.len(),
                        Ok(false) => {
                            err_n += discard.len();
                            err_done = true;
                        }
                        Err(err) => return Err(err),
                    }
                }
            }
        }
        match peek_exit(pid)? {
            Some(0) if out_done && err_done => return Ok(out),
            Some(_) if out_done && err_done => return Err(invalid("verifier")),
            _ => thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn peek_exit(pid: u32) -> io::Result<Option<i32>> {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc != 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        if info.si_code == libc::CLD_EXITED {
            return Ok(Some(unsafe { info.si_status() }));
        }
        return Ok(Some(-1));
    }
}

fn kill_group(pid: u32) {
    if pid == 0 {
        return;
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

fn reap_direct(child: &mut Child) -> io::Result<()> {
    let end = Instant::now() + Duration::from_millis(500);
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(invalid("verifier"));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn pump(file: &mut impl Read, mut dest: Option<&mut Vec<u8>>, room: usize) -> io::Result<bool> {
    let mut buf = [0u8; 512];
    let mut stored = 0usize;
    loop {
        match file.read(&mut buf) {
            Ok(0) => return Ok(false),
            Ok(n) => {
                if stored + n > room {
                    return Err(invalid("verifier"));
                }
                stored += n;
                if let Some(out) = dest.as_mut() {
                    out.extend_from_slice(&buf[..n]);
                }
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(true),
            Err(err) => return Err(err),
        }
    }
}

fn read_exact_file(dir: RawFd, name: &str, cap: usize) -> io::Result<Vec<u8>> {
    let fd = openat2(
        dir,
        &one_component(name)?,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )?;
    let st = fstat(fd.as_raw_fd())?;
    if st.st_mode & libc::S_IFMT != libc::S_IFREG
        || st.st_nlink != 1
        || st.st_uid != euid()
        || st.st_mode & 0o7777 != 0o600
        || st.st_size <= 0
        || st.st_size as usize > cap
    {
        return Err(invalid("approval"));
    }
    let mut buf = vec![0u8; st.st_size as usize];
    let mut off = 0;
    while off < buf.len() {
        let n = unsafe {
            libc::read(
                fd.as_raw_fd(),
                buf[off..].as_mut_ptr().cast(),
                buf.len() - off,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(invalid("approval"));
        }
        off += n as usize;
    }
    let mut extra = [0u8; 1];
    let n = unsafe { libc::read(fd.as_raw_fd(), extra.as_mut_ptr().cast(), 1) };
    if n != 0 {
        return Err(invalid("approval"));
    }
    let after = fstat(fd.as_raw_fd())?;
    if after.st_ino != st.st_ino
        || after.st_size != st.st_size
        || after.st_nlink != 1
        || after.st_mode & 0o7777 != 0o600
    {
        return Err(invalid("approval"));
    }
    Ok(buf)
}

fn open_mode(path: &Path, mode: u32) -> io::Result<OwnedFd> {
    let fd = open_host(path)?;
    let st = fstat(fd.as_raw_fd())?;
    if st.st_mode & 0o7777 != mode {
        return Err(invalid("mode"));
    }
    Ok(fd)
}

fn open_dir(parent: RawFd, name: &CString) -> io::Result<OwnedFd> {
    let fd = openat2(
        parent,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )?;
    let st = fstat(fd.as_raw_fd())?;
    if st.st_mode & libc::S_IFMT != libc::S_IFDIR
        || st.st_uid != euid()
        || st.st_mode & 0o7777 != 0o700
    {
        return Err(invalid("mode"));
    }
    Ok(fd)
}

fn dup_fd(fd: RawFd) -> io::Result<OwnedFd> {
    raw_owned(unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) })
}

fn set_nonblock(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn euid() -> libc::uid_t {
    unsafe { libc::geteuid() }
}

fn nanos() -> io::Result<u128> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .map_err(|_| invalid("clock"))
}

fn abs_path(text: &str) -> io::Result<PathBuf> {
    if text.len() > 4096 || text.bytes().any(|b| b == 0 || b == b'\n') {
        return Err(invalid("path"));
    }
    let path = Path::new(text);
    if !path.is_absolute() {
        return Err(invalid("path"));
    }
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) if part.to_str().is_some() => {}
            _ => return Err(invalid("path")),
        }
    }
    Ok(path.to_path_buf())
}

fn parse_hex_text(text: &str) -> io::Result<String> {
    let _ = parse_hex32(text)?;
    Ok(text.to_string())
}

fn parse_hex32(text: &str) -> io::Result<[u8; 32]> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(invalid("digest"));
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).unwrap_or("zz"), 16)
            .map_err(|_| invalid("digest"))?;
    }
    Ok(out)
}

#[cfg(test)]
#[path = "workspace_acceptance_tests.rs"]
mod tests;
