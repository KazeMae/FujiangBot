//! Private copy of a fixed public system runtime. Not an approval or an archive.
use std::ffi::CString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::workspace_seed::PublicRuntime;

const BINS: &[&str] = &[
    "/usr/bin/bash",
    "/usr/bin/dpkg",
    "/usr/bin/dpkg-deb",
    "/usr/bin/dpkg-split",
    "/usr/bin/apt-get",
    "/usr/bin/rm",
    "/usr/bin/tar",
    "/usr/bin/diff",
    "/usr/sbin/ldconfig",
    "/usr/sbin/start-stop-daemon",
];

const TABLES: &[&str] = &[
    "/usr/share/dpkg/cputable",
    "/usr/share/dpkg/tupletable",
    "/usr/share/dpkg/triplettable",
    "/usr/share/dpkg/ostable",
    "/usr/share/dpkg/abitable",
];

pub(crate) fn stage_public() -> io::Result<PublicRuntime> {
    let parent = std::env::temp_dir();
    let name = format!(
        "fujiang-runtime-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos()
    );
    let stage = parent.join(&name);
    fs::create_dir(&stage).map_err(|err| io::Error::new(err.kind(), "runtime stage"))?;
    let parent_fd = match open_dir(&parent) {
        Ok(fd) => fd,
        Err(err) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(err);
        }
    };
    let cname = match CString::new(name.clone()) {
        Ok(name) => name,
        Err(_) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "name"));
        }
    };
    let mut hold = StageHold {
        parent: Some(parent_fd),
        name: cname,
    };
    let created = (|| {
        let mut perms = fs::metadata(&stage)?.permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&stage, perms)?;
        let (python, stdlib) = discover_python()?;
        let mut bins: Vec<PathBuf> = BINS.iter().map(PathBuf::from).collect();
        bins.push(python.clone());
        for method in [
            "/usr/lib/apt/methods/file",
            "/usr/lib/apt/methods/http",
            "/usr/lib/apt/methods/https",
        ] {
            if Path::new(method).exists() {
                bins.push(PathBuf::from(method));
            }
        }
        for bin in &bins {
            let real = resolve_public(bin)?;
            copy_public_file(&real, &stage, guest_rel(bin.to_str().unwrap_or(""))?, 0o755)?;
            place_libs(&real, &stage)?;
        }
        let bash = resolve_public(Path::new("/usr/bin/bash"))?;
        copy_public_file(&bash, &stage, Path::new("bin/bash"), 0o755)?;
        copy_public_file(&bash, &stage, Path::new("bin/sh"), 0o755)?;
        copy_public_file(&python, &stage, Path::new("usr/bin/python3"), 0o755)?;
        if let Some(ld) = interp_of(&python)? {
            let real = resolve_public(&ld)?;
            copy_public_file(&real, &stage, guest_rel(ld.to_str().unwrap_or(""))?, 0o755)?;
        }
        for table in TABLES {
            if Path::new(table).exists() {
                copy_public_file(Path::new(table), &stage, guest_rel(table)?, 0o644)?;
            }
        }
        copy_stdlib(&stage, &stdlib)?;
        if let Some(ca) = public_ca() {
            let _ = copy_public_file(
                &ca,
                &stage,
                Path::new("etc/ssl/certs/ca-certificates.crt"),
                0o644,
            );
        }
        fs::create_dir_all(stage.join("etc"))?;
        let mut etc = fs::metadata(stage.join("etc"))?.permissions();
        etc.set_mode(0o755);
        fs::set_permissions(stage.join("etc"), etc)?;
        let root = open_dir(&stage)?;
        let identity = super::tree_digest(root.as_raw_fd())?;
        let parent_fd = hold
            .parent
            .take()
            .ok_or_else(|| io::Error::other("stage parent"))?;
        let lease = open_dir(&stage)?;
        PublicRuntime::from_sealed(root, identity, parent_fd, hold.name.clone(), lease)
    })();
    if created.is_err() && hold.parent.is_none() {
        if let Ok(parent_fd) = open_dir(&parent) {
            let _ = super::workspace_seed::discard_stage(parent_fd.as_raw_fd(), &hold.name);
        }
    }
    created
}

struct StageHold {
    parent: Option<OwnedFd>,
    name: CString,
}

impl Drop for StageHold {
    fn drop(&mut self) {
        if let Some(parent) = self.parent.take() {
            let _ = super::workspace_seed::discard_stage(parent.as_raw_fd(), &self.name);
        }
    }
}

#[cfg(test)]
pub(crate) fn abandon_fresh_stage(name: &str) -> io::Result<()> {
    let parent = std::env::temp_dir();
    let stage = parent.join(name);
    fs::create_dir(&stage)?;
    let parent_fd = open_dir(&parent)?;
    let cname = CString::new(name).map_err(|_| io::Error::other("name"))?;
    let _hold = StageHold {
        parent: Some(parent_fd),
        name: cname,
    };
    Err(io::Error::other("forced stage failure"))
}

fn guest_rel(abs: &str) -> io::Result<&Path> {
    Ok(Path::new(abs.trim_start_matches('/')))
}

fn ldd_output(bin: &Path) -> io::Result<String> {
    let ldd = validated_exec(Path::new("/usr/bin/ldd"))?;
    if ldd != Path::new("/usr/bin/ldd") && !ldd.starts_with("/usr/bin/") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "ldd"));
    }
    let mut cmd = Command::new(&ldd);
    cmd.arg(bin);
    let out = output_timeout(&mut cmd)?;
    if !out.status.success() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "ldd"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn output_timeout(cmd: &mut Command) -> io::Result<std::process::Output> {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            let pr = libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
            if pr != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .env_clear()
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|err| io::Error::new(err.kind(), "spawn"))?;
    let mut pipe = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("stdout"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            match pipe.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    let room = 1_048_576usize.saturating_sub(buf.len());
                    if room > 0 {
                        buf.extend_from_slice(&tmp[..n.min(room)]);
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(buf);
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
            let _ = rx.recv_timeout(Duration::from_secs(1));
            return Err(io::Error::new(io::ErrorKind::TimedOut, "discovery timeout"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

fn validated_exec(start: &Path) -> io::Result<PathBuf> {
    let real = resolve_public(start)?;
    let csrc = CString::new(real.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "exec"))?;
    let fd = unsafe {
        libc::open(
            csrc.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let st = file.metadata()?;
    let mode = MetadataExt::mode(&st);
    if !st.is_file()
        || MetadataExt::uid(&st) != 0
        || mode & 0o022 != 0
        || mode & 0o004 == 0
        || mode & 0o111 == 0
        || mode & 0o6000 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("refused exec {}", real.display()),
        ));
    }
    Ok(real)
}

fn discover_python() -> io::Result<(PathBuf, PathBuf)> {
    let python = validated_exec(Path::new("/usr/bin/python3"))?;
    let mut cmd = Command::new(&python);
    cmd.arg("-I")
        .arg("-c")
        .arg("import sys,sysconfig\nprint(sys.executable)\nprint(sysconfig.get_path('stdlib'))\n");
    let out = output_timeout(&mut cmd)?;
    if !out.status.success() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "python"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines().filter(|l| !l.is_empty());
    let bin = PathBuf::from(lines.next().unwrap_or(""));
    let stdlib = PathBuf::from(lines.next().unwrap_or(""));
    let bin = resolve_public(&bin)?;
    let stdlib = resolve_public(&stdlib)?;
    if !bin.starts_with("/usr/bin/") || !stdlib.starts_with("/usr/lib/") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "python path"));
    }
    Ok((bin, stdlib))
}

fn interp_of(bin: &Path) -> io::Result<Option<PathBuf>> {
    let text = ldd_output(bin)?;
    for line in text.lines() {
        let line = line.trim();
        if line.contains("ld-linux") && line.starts_with('/') {
            let path = line.split_whitespace().next().unwrap_or("");
            if path.starts_with('/') {
                return Ok(Some(PathBuf::from(path)));
            }
        }
    }
    Ok(None)
}

fn place_libs(bin: &Path, stage: &Path) -> io::Result<()> {
    let text = ldd_output(bin)?;
    for line in text.lines() {
        let Some((_, right)) = line.split_once("=>") else {
            continue;
        };
        let path = right.split_whitespace().next().unwrap_or("");
        if !path.starts_with('/') {
            continue;
        }
        let lib = PathBuf::from(path);
        let real = resolve_public(&lib)?;
        let textp = real.to_string_lossy();
        if !(textp.starts_with("/usr/lib/")
            || textp.starts_with("/lib/")
            || textp.starts_with("/lib64/"))
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "lib root"));
        }
        let file = lib
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "lib name"))?;
        copy_public_file(&real, stage, &Path::new("lib/runtime").join(file), 0o644)?;
    }
    Ok(())
}

fn public_ca() -> Option<PathBuf> {
    let path = Path::new("/etc/ssl/certs/ca-certificates.crt");
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let mode = MetadataExt::mode(&meta);
    if MetadataExt::uid(&meta) != 0 || mode & 0o022 != 0 || mode & 0o004 == 0 {
        return None;
    }
    Some(path.to_path_buf())
}

fn lexical(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::RootDir => out.push("/"),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            std::path::Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    Some(out)
}

fn resolve_public(start: &Path) -> io::Result<PathBuf> {
    let mut cur = start.to_path_buf();
    for _ in 0..8 {
        let meta = fs::symlink_metadata(&cur)?;
        if meta.file_type().is_symlink() {
            let link = fs::read_link(&cur)?;
            let next = if link.is_absolute() {
                link
            } else {
                cur.parent().unwrap_or(Path::new("/")).join(link)
            };
            let Some(next) = lexical(&next) else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "public link"));
            };
            let text = next.to_string_lossy();
            if !(text.starts_with("/usr/")
                || text.starts_with("/lib/")
                || text.starts_with("/lib64/"))
            {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "public link"));
            }
            cur = next;
            continue;
        }
        return Ok(cur);
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "public link"))
}

fn copy_public_file(src: &Path, stage: &Path, rel: &Path, mode: u32) -> io::Result<()> {
    let dest = stage.join(rel);
    if dest.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let csrc = CString::new(src.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "src"))?;
    let fd = unsafe {
        libc::open(
            csrc.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    let st = file.metadata()?;
    if !st.is_file()
        || MetadataExt::uid(&st) != 0
        || MetadataExt::mode(&st) & 0o022 != 0
        || MetadataExt::mode(&st) & 0o004 == 0
        || MetadataExt::mode(&st) & 0o6000 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "refused public file {} uid={} gid={} mode={:o} regular={}",
                src.display(),
                MetadataExt::uid(&st),
                MetadataExt::gid(&st),
                MetadataExt::mode(&st),
                st.is_file()
            ),
        ));
    }
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&dest)?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
    }
    Ok(())
}

fn copy_stdlib(stage: &Path, src: &Path) -> io::Result<()> {
    let rel = src.strip_prefix("/").unwrap_or(src);
    let dst = stage.join(rel);
    fs::create_dir_all(&dst)?;
    copy_dir(src, &dst, stage)
}

fn copy_dir(src: &Path, dst: &Path, stage: &Path) -> io::Result<()> {
    for ent in fs::read_dir(src)? {
        let ent = ent?;
        let name = ent.file_name();
        let from = ent.path();
        let to = dst.join(&name);
        let kind = ent.file_type()?;
        if kind.is_symlink() {
            place_link(&from, &to, stage)?;
        } else if kind.is_dir() {
            fs::create_dir(&to)?;
            copy_dir(&from, &to, stage)?;
        } else if kind.is_file() {
            copy_public_file(&from, stage, to.strip_prefix(stage).unwrap(), 0o644)?;
            if name.as_bytes().ends_with(b".so") {
                place_libs(&from, stage)?;
            }
        }
    }
    Ok(())
}

fn place_link(src: &Path, dst: &Path, stage: &Path) -> io::Result<()> {
    let text = fs::read_link(src)?;
    if text.is_absolute() {
        // Do not plant a symlink that leaves the private root.
        let _ = (src, stage);
        return Ok(());
    }
    if text.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    }) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "stdlib link"));
    }
    std::os::unix::fs::symlink(&text, dst)?;
    Ok(())
}

fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path"))?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_stage_directory_is_removed() {
        let name = format!(
            "fujiang-runtime-abandon-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(&name);
        let err = super::abandon_fresh_stage(&name).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
        assert!(!path.exists(), "{}", path.display());
    }

    #[test]
    fn discovery_timeout_drains_and_reaps() {
        use std::process::Command;
        use std::time::{Duration, Instant};
        let mut cmd = Command::new("/usr/bin/python3");
        cmd.arg("-I").arg("-c").arg(
            "import sys,time; sys.stdout.write('Z'*200000); sys.stdout.flush(); time.sleep(30)",
        );
        let start = Instant::now();
        let err = super::output_timeout(&mut cmd).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(8));
    }
}
