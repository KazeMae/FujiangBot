//! Private copy of a fixed public system runtime. Not an approval or an archive.
use std::collections::VecDeque;
use std::ffi::{CString, OsStr};
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
        .arg("-S")
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
    copy_dir(src, &dst, stage, 0)
}

fn copy_dir(src: &Path, dst: &Path, stage: &Path, depth: u32) -> io::Result<()> {
    if depth > 32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "stdlib link limit",
        ));
    }
    for ent in fs::read_dir(src)? {
        let ent = ent?;
        let name = ent.file_name();
        let from = ent.path();
        let to = dst.join(&name);
        let kind = ent.file_type()?;
        if kind.is_symlink() {
            if optional_sitecustomize(&from) {
                continue;
            }
            place_link(&from, &to, stage)?;
        } else if kind.is_dir() {
            fs::create_dir(&to)?;
            copy_dir(&from, &to, stage, depth + 1)?;
        } else if kind.is_file() {
            copy_public_file(&from, stage, to.strip_prefix(stage).unwrap(), 0o644)?;
            if name.as_bytes().ends_with(b".so") {
                place_libs(&from, stage)?;
            }
        }
    }
    Ok(())
}

const PUBLIC_ROOTS: &[&str] = &["/usr/lib", "/usr/lib64", "/lib", "/lib64"];

fn optional_sitecustomize(path: &Path) -> bool {
    let Ok(text) = fs::read_link(path) else {
        return false;
    };
    sitecustomize_hook(path, &text)
}

fn sitecustomize_hook(src: &Path, link: &Path) -> bool {
    let loss = src.to_string_lossy();
    let Some(rest) = loss
        .strip_prefix("/usr/lib/python3.")
        .or_else(|| loss.strip_prefix("/usr/lib64/python3."))
    else {
        return false;
    };
    let Some((minor, tail)) = rest.split_once('/') else {
        return false;
    };
    if tail != "sitecustomize.py" || minor.is_empty() || !minor.chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    let joined = if link.is_absolute() {
        link.to_path_buf()
    } else {
        let Some(parent) = src.parent() else {
            return false;
        };
        parent.join(link)
    };
    let Some(norm) = lexical(&joined) else {
        return false;
    };
    norm.as_os_str() == std::ffi::OsStr::new(&format!("/etc/python3.{minor}/sitecustomize.py"))
}

fn place_link(src: &Path, dst: &Path, _stage: &Path) -> io::Result<()> {
    materialize_public_link(src, dst, PUBLIC_ROOTS, true)
}

fn materialize_public_link(
    src: &Path,
    dst: &Path,
    roots: &[&str],
    enforce: bool,
) -> io::Result<()> {
    let parent = src
        .parent()
        .ok_or_else(|| link_err("stdlib link race", src, ""))?;
    let name = src
        .file_name()
        .ok_or_else(|| link_err("stdlib link race", src, ""))?;
    let dir = open_anchored(parent, enforce)?;
    let cname = CString::new(name.as_bytes()).map_err(|_| link_err("stdlib link race", src, ""))?;
    let text = symlink_text(dir.as_raw_fd(), &cname, src)?;
    let show = text.to_string_lossy().into_owned();
    let root_fd = open_dir(Path::new("/"))?;
    let mut logical = if text.is_absolute() {
        PathBuf::from("/")
    } else {
        parent.to_path_buf()
    };
    let mut cur = if text.is_absolute() {
        dup_owned(root_fd.as_raw_fd())?
    } else {
        dup_owned(dir.as_raw_fd())?
    };
    let mut pending: VecDeque<std::ffi::OsString> = text
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => Some(n.to_os_string()),
            std::path::Component::ParentDir => Some(OsStr::new("..").to_os_string()),
            std::path::Component::CurDir => None,
            std::path::Component::RootDir => None,
            _ => Some(OsStr::new("").to_os_string()),
        })
        .collect();
    let mut hops = 1u32;
    while let Some(comp) = pending.pop_front() {
        if comp.is_empty() {
            return Err(link_err("stdlib link escape", src, &show));
        }
        if comp == ".." {
            if !logical.pop() || !can_traverse(&logical, roots) {
                return Err(link_err("stdlib link escape", src, &show));
            }
            cur = open_at(cur.as_raw_fd(), c"..", true)?;
            if enforce {
                trusted_dir(&fstat_fd(cur.as_raw_fd())?, src, &show)?;
            }
            continue;
        }
        logical.push(&comp);
        if !can_traverse(&logical, roots) {
            return Err(link_err("stdlib link escape", src, &show));
        }
        let cn = CString::new(comp.as_bytes())
            .map_err(|_| link_err("stdlib link escape", src, &show))?;
        let mut st = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                cur.as_raw_fd(),
                cn.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        if st.st_mode & libc::S_IFMT == libc::S_IFLNK {
            hops += 1;
            if hops > 8 {
                return Err(link_err("stdlib link limit", src, &show));
            }
            let next = readlink_at(cur.as_raw_fd(), &cn)?;
            logical.pop();
            if next.is_absolute() {
                logical = PathBuf::from("/");
                cur = dup_owned(root_fd.as_raw_fd())?;
            }
            for c in next.components().rev() {
                match c {
                    std::path::Component::Normal(n) => pending.push_front(n.to_os_string()),
                    std::path::Component::ParentDir => {
                        pending.push_front(OsStr::new("..").to_os_string())
                    }
                    std::path::Component::CurDir | std::path::Component::RootDir => {}
                    _ => return Err(link_err("stdlib link escape", src, &show)),
                }
            }
            continue;
        }
        if pending.is_empty() {
            if !is_inside(&logical, roots) {
                return Err(link_err("stdlib link escape", src, &show));
            }
            return finish_target(Finish {
                dir: cur.as_raw_fd(),
                name: &cn,
                st: &st,
                dst,
                enforce,
                src,
                show: &show,
            });
        }
        if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(link_err(
                &format!("stdlib link type mode={:o}", st.st_mode),
                src,
                &show,
            ));
        }
        if enforce {
            trusted_dir(&st, src, &show)?;
        }
        cur = open_at(cur.as_raw_fd(), &cn, true)?;
        if enforce {
            trusted_dir(&fstat_fd(cur.as_raw_fd())?, src, &show)?;
        }
    }
    Err(link_err("stdlib link escape", src, &show))
}

struct Finish<'a> {
    dir: libc::c_int,
    name: &'a CString,
    st: &'a libc::stat,
    dst: &'a Path,
    enforce: bool,
    src: &'a Path,
    show: &'a str,
}

fn finish_target(job: Finish<'_>) -> io::Result<()> {
    let Finish {
        dir,
        name,
        st,
        dst,
        enforce,
        src,
        show,
    } = job;
    let kind = st.st_mode & libc::S_IFMT;
    if kind == libc::S_IFDIR {
        return Err(link_err("stdlib link type directory", src, show));
    }
    if kind != libc::S_IFREG {
        return Err(link_err(
            &format!(
                "stdlib link type uid={} gid={} mode={:o} regular=false",
                st.st_uid, st.st_gid, st.st_mode
            ),
            src,
            show,
        ));
    }
    if !enforce && st.st_mode & 0o6000 != 0 {
        return Err(link_err("stdlib link type setid", src, show));
    }
    #[cfg(test)]
    race_swap_now();
    let fd = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    let again = file.metadata()?;
    if !again.is_file()
        || MetadataExt::dev(&again) != st.st_dev
        || MetadataExt::ino(&again) != st.st_ino
    {
        return Err(link_err("stdlib link race", src, show));
    }
    let mode = MetadataExt::mode(&again);
    if enforce
        && (MetadataExt::uid(&again) != 0
            || mode & 0o022 != 0
            || mode & 0o004 == 0
            || mode & 0o6000 != 0)
    {
        return Err(link_err(
            &format!(
                "stdlib link trust uid={} gid={} mode={:o}",
                MetadataExt::uid(&again),
                MetadataExt::gid(&again),
                mode
            ),
            src,
            show,
        ));
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dst)?;
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

fn symlink_text(dir: libc::c_int, name: &CString, src: &Path) -> io::Result<PathBuf> {
    let mut st = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFLNK {
        return Err(link_err(
            &format!("stdlib link race mode={:o}", st.st_mode),
            src,
            "",
        ));
    }
    readlink_at(dir, name)
}

fn readlink_at(dir: libc::c_int, name: &CString) -> io::Result<PathBuf> {
    let mut buf = [0u8; 4096];
    let n = unsafe {
        libc::readlinkat(
            dir,
            name.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if n as usize >= buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "stdlib link limit",
        ));
    }
    Ok(PathBuf::from(OsStr::from_bytes(&buf[..n as usize])))
}

fn is_inside(path: &Path, roots: &[&str]) -> bool {
    roots.iter().any(|root| {
        let root = Path::new(root);
        path == root || path.starts_with(root)
    })
}

fn can_traverse(path: &Path, roots: &[&str]) -> bool {
    if path == Path::new("/") || is_inside(path, roots) {
        return true;
    }
    roots.iter().any(|root| Path::new(root).starts_with(path))
}

fn trusted_dir(st: &libc::stat, src: &Path, show: &str) -> io::Result<()> {
    if st.st_mode & libc::S_IFMT != libc::S_IFDIR || st.st_uid != 0 || st.st_mode & 0o022 != 0 {
        return Err(link_err(
            &format!(
                "stdlib link trust uid={} gid={} mode={:o}",
                st.st_uid, st.st_gid, st.st_mode
            ),
            src,
            show,
        ));
    }
    Ok(())
}

fn open_anchored(path: &Path, enforce: bool) -> io::Result<OwnedFd> {
    let mut cur = open_dir(Path::new("/"))?;
    if enforce {
        trusted_dir(&fstat_fd(cur.as_raw_fd())?, path, "")?;
    }
    for comp in path.components() {
        let std::path::Component::Normal(name) = comp else {
            if matches!(comp, std::path::Component::RootDir) {
                continue;
            }
            return Err(link_err("stdlib link escape", path, ""));
        };
        let cn =
            CString::new(name.as_bytes()).map_err(|_| link_err("stdlib link escape", path, ""))?;
        let mut st = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                cur.as_raw_fd(),
                cn.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        if enforce {
            trusted_dir(&st, path, "")?;
        } else if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(link_err("stdlib link type", path, ""));
        }
        cur = open_at(cur.as_raw_fd(), &cn, true)?;
        let live = fstat_fd(cur.as_raw_fd())?;
        if live.st_dev != st.st_dev || live.st_ino != st.st_ino {
            return Err(link_err("stdlib link race", path, ""));
        }
        if enforce {
            trusted_dir(&live, path, "")?;
        }
    }
    Ok(cur)
}

#[cfg(test)]
thread_local! {
    static RACE_SWAP: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn set_race_swap(path: Option<PathBuf>) {
    RACE_SWAP.with(|slot| *slot.borrow_mut() = path);
}

#[cfg(test)]
fn race_swap_now() {
    let path = RACE_SWAP.with(|slot| slot.borrow_mut().take());
    let Some(path) = path else {
        return;
    };
    let _ = fs::remove_file(&path);
    if let Ok(c) = CString::new(path.as_os_str().as_bytes()) {
        unsafe {
            libc::mkfifo(c.as_ptr(), 0o644);
        }
    }
}

fn link_err(kind: &str, src: &Path, text: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{kind} src={} text={text}", src.display()),
    )
}

fn open_at(dir: libc::c_int, name: &std::ffi::CStr, directory: bool) -> io::Result<OwnedFd> {
    let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn dup_owned(fd: libc::c_int) -> io::Result<OwnedFd> {
    let d = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if d < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(d) })
    }
}

fn fstat_fd(fd: libc::c_int) -> io::Result<libc::stat> {
    let mut st = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(st)
    }
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
#[path = "workspace_runtime_tests.rs"]
mod tests;
