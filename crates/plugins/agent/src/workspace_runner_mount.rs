use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::process::Command;

pub(super) fn push_fixed_args(
    cmd: &mut Command,
    work: RawFd,
    info: RawFd,
    block: RawFd,
    seccomp: RawFd,
) {
    // One uid: bwrap's own user namespace. No newuidmap and no extra caps.
    cmd.arg("--unshare-user")
        .arg("--uid")
        .arg("0")
        .arg("--gid")
        .arg("0")
        .arg("--unshare-pid")
        .arg("--unshare-ipc")
        .arg("--unshare-uts")
        .arg("--cap-drop")
        .arg("ALL")
        .arg("--new-session")
        .arg("--die-with-parent")
        .arg("--ro-bind")
        .arg("/usr")
        .arg("/usr")
        .arg("--ro-bind")
        .arg("/etc")
        .arg("/etc")
        .arg("--ro-bind")
        .arg("/var")
        .arg("/var");
    for path in ["/var/log", "/var/tmp", "/var/cache"] {
        if std::fs::metadata(path).is_ok_and(|meta| meta.is_dir()) {
            cmd.arg("--tmpfs").arg(path);
        }
    }
    if std::path::Path::new("/etc/apt/auth.conf.d").exists() {
        cmd.arg("--tmpfs").arg("/etc/apt/auth.conf.d");
    }
    if std::path::Path::new("/etc/apt/auth.conf").exists() {
        cmd.arg("--ro-bind")
            .arg("/dev/null")
            .arg("/etc/apt/auth.conf");
    }
    push_usr_links(cmd);
    push_resolv(cmd);
    cmd.arg("--proc")
        .arg("/proc")
        .arg("--dev")
        .arg("/dev")
        .arg("--tmpfs")
        .arg("/tmp")
        .arg("--tmpfs")
        .arg("/run")
        .arg("--bind-fd")
        .arg(work.to_string())
        .arg("/work")
        .arg("--chdir")
        .arg("/work")
        .arg("--clearenv")
        .arg("--setenv")
        .arg("PATH")
        .arg("/usr/sbin:/usr/bin:/bin")
        .arg("--setenv")
        .arg("HOME")
        .arg("/work")
        .arg("--setenv")
        .arg("LANG")
        .arg("C")
        .arg("--info-fd")
        .arg(info.to_string())
        .arg("--block-fd")
        .arg(block.to_string())
        .arg("--seccomp")
        .arg(seccomp.to_string())
        .arg("--");
}

fn push_usr_links(cmd: &mut Command) {
    for (name, fallback) in [
        ("bin", "/usr/bin"),
        ("lib", "/usr/lib"),
        ("lib64", "/usr/lib64"),
        ("sbin", "/usr/sbin"),
    ] {
        let path = format!("/{name}");
        let meta = std::fs::symlink_metadata(&path);
        let Ok(meta) = meta else { continue };
        if meta.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(&path) {
                let text = target.to_string_lossy();
                if text.starts_with("usr/") || text.starts_with("/usr/") {
                    cmd.arg("--symlink").arg(text.as_ref()).arg(&path);
                    continue;
                }
            }
        }
        if meta.is_dir() {
            cmd.arg("--ro-bind").arg(fallback).arg(&path);
        }
    }
}

fn push_resolv(cmd: &mut Command) {
    let path = std::path::Path::new("/etc/resolv.conf");
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !meta.file_type().is_symlink() {
        return;
    }
    let Ok(target) = std::fs::read_link(path) else {
        return;
    };
    let text = target.to_string_lossy();
    if !(text.starts_with("/run/") || text.starts_with("../run/") || text.starts_with("run/")) {
        return;
    }
    let abs = if text.starts_with('/') {
        text.into_owned()
    } else {
        format!("/{text}")
    };
    let Ok(name) = CString::new(abs.clone()) else {
        return;
    };
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return;
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let Ok(st) = file.metadata() else {
        return;
    };
    if st.is_file() && st.uid() == 0 {
        cmd.arg("--ro-bind").arg(abs).arg("/etc/resolv.conf");
    }
}

pub(crate) fn landlock_scope_ok(abi: i32) -> io::Result<()> {
    if abi < 6 {
        Err(io::Error::other("unavailable: landlock scope"))
    } else {
        Ok(())
    }
}

pub(super) fn scope_landlock() -> io::Result<()> {
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0,
            1u32,
        )
    };
    landlock_scope_ok(abi as i32)?;
    #[repr(C)]
    struct Attr {
        handled_access_fs: u64,
        handled_access_net: u64,
        scoped: u64,
    }
    let attr = Attr {
        handled_access_fs: 0,
        handled_access_net: 0,
        scoped: 1 | 2,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const Attr,
            std::mem::size_of::<Attr>() as libc::c_ulong,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, fd, 0) };
    unsafe { libc::close(fd as i32) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::landlock_scope_ok;

    #[test]
    fn landlock_below_abi_six_fails_closed() {
        let err = landlock_scope_ok(5).unwrap_err();
        assert!(err.to_string().contains("unavailable: landlock scope"));
        assert!(landlock_scope_ok(6).is_ok());
    }
}
