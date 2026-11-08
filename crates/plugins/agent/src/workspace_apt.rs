//! Private apt seed from public os-release. Does not read host apt configuration.
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

const DEBIAN_KEY_GUEST: &str = "/usr/share/keyrings/debian-archive-keyring.gpg";
const UBUNTU_KEY_GUEST: &str = "/usr/share/keyrings/ubuntu-archive-keyring.gpg";
const DEBIAN_ARCHIVE: &str = "https://deb.debian.org/debian";
const UBUNTU_ARCHIVE: &str = "https://archive.ubuntu.com/ubuntu";

pub(super) fn seed(stage: &Path) -> io::Result<()> {
    write_new(
        &stage.join("etc/apt/apt.conf.d/00fujiang"),
        b"APT::Sandbox::User \"root\";\nAcquire::https::Verify-Peer \"true\";\nAPT::Update::Error-Mode \"any\";\nAcquire::Retries \"0\";\n",
    )?;
    write_new(&stage.join("etc/apt/sources.list"), b"")?;
    for rel in [
        "var/lib/dpkg/info",
        "var/lib/dpkg/updates",
        "var/lib/apt/lists/partial",
        "var/cache/apt/archives/partial",
        "var/log/apt",
        "etc/apt/preferences.d",
        "etc/apt/sources.list.d",
    ] {
        let dir = stage.join(rel);
        fs::create_dir_all(&dir)?;
        let mut perms = fs::metadata(&dir)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dir, perms)?;
    }
    write_new(&stage.join("var/lib/dpkg/status"), b"")?;
    write_new(&stage.join("var/lib/dpkg/available"), b"")?;
    copy_sequoia_policy(stage)?;
    match read_os_release() {
        Ok(text) => seed_profile(stage, &text)?,
        Err(err) => write_new(
            &stage.join("etc/apt/remote-unsupported"),
            format!("remote unsupported: os-release: {err}\n").as_bytes(),
        )?,
    }
    Ok(())
}

fn seed_profile(stage: &Path, os_release: &str) -> io::Result<()> {
    let Some((id, codename)) = profile(os_release) else {
        return write_new(
            &stage.join("etc/apt/remote-unsupported"),
            b"remote unsupported: os-release profile\n",
        );
    };
    let (archive, key_guest, key_src, rel, source_rel) = match id {
        "debian" => (
            DEBIAN_ARCHIVE,
            DEBIAN_KEY_GUEST,
            "/usr/share/keyrings/debian-archive-keyring.gpg",
            "usr/share/keyrings/debian-archive-keyring.gpg",
            "etc/apt/sources.list.d/debian.sources",
        ),
        "ubuntu" => (
            UBUNTU_ARCHIVE,
            UBUNTU_KEY_GUEST,
            "/usr/share/keyrings/ubuntu-archive-keyring.gpg",
            "usr/share/keyrings/ubuntu-archive-keyring.gpg",
            "etc/apt/sources.list.d/ubuntu.sources",
        ),
        _ => {
            return write_new(
                &stage.join("etc/apt/remote-unsupported"),
                format!("remote unsupported: id={id}\n").as_bytes(),
            );
        }
    };
    let text = exact_source(archive, codename, key_guest)?;
    match fs::symlink_metadata(key_src) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return write_new(
                &stage.join("etc/apt/remote-unsupported"),
                format!("remote unsupported: keyring {key_src}\n").as_bytes(),
            );
        }
        Err(err) => return Err(err),
        Ok(_) => {
            let src = super::resolve_public(Path::new(key_src))?;
            super::copy_public_file(&src, stage, Path::new(rel), 0o644)?;
        }
    }
    write_new(&stage.join(source_rel), text.as_bytes())?;
    Ok(())
}

fn copy_sequoia_policy(stage: &Path) -> io::Result<()> {
    let path = Path::new("/usr/share/apt/default-sequoia.config");
    match fs::symlink_metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io::Error::new(err.kind(), "sequoia policy")),
        Ok(_) => {
            let src = super::resolve_public(path)?;
            super::copy_public_file(
                &src,
                stage,
                Path::new("usr/share/apt/default-sequoia.config"),
                0o644,
            )
        }
    }
}

pub(crate) fn exact_source(archive: &str, codename: &str, key: &str) -> io::Result<String> {
    if !codename_ok(codename) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "apt source"));
    }
    let paired = (archive == DEBIAN_ARCHIVE && key == DEBIAN_KEY_GUEST)
        || (archive == UBUNTU_ARCHIVE && key == UBUNTU_KEY_GUEST);
    if !paired {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsafe apt key path",
        ));
    }
    let text = format!(
        "Types: deb\nURIs: {archive}\nSuites: {codename}\nComponents: main\nSigned-By: {key}\n"
    );
    vet_exact(&text, archive, codename, "main", key)?;
    Ok(text)
}

pub(crate) fn vet_exact(
    text: &str,
    archive: &str,
    suites: &str,
    components: &str,
    key: &str,
) -> io::Result<()> {
    let lines: Vec<&str> = text.lines().collect();
    let uri = format!("URIs: {archive}");
    let suite = format!("Suites: {suites}");
    let comp = format!("Components: {components}");
    let signed = format!("Signed-By: {key}");
    let expect = [
        "Types: deb",
        uri.as_str(),
        suite.as_str(),
        comp.as_str(),
        signed.as_str(),
    ];
    if lines.len() != expect.len() || lines.iter().zip(expect.iter()).any(|(a, b)| a != b) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsafe apt source",
        ));
    }
    let paired = (archive == DEBIAN_ARCHIVE && key == DEBIAN_KEY_GUEST)
        || (archive == UBUNTU_ARCHIVE && key == UBUNTU_KEY_GUEST);
    if !paired {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsafe apt key path",
        ));
    }
    Ok(())
}

fn codename_ok(codename: &str) -> bool {
    !codename.is_empty()
        && codename.len() <= 32
        && codename
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn profile(text: &str) -> Option<(&str, &str)> {
    let mut id = "";
    let mut code = "";
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ID=") {
            id = v.trim_matches('"');
        } else if let Some(v) = line.strip_prefix("VERSION_CODENAME=") {
            code = v.trim_matches('"');
        }
    }
    if !codename_ok(code) {
        return None;
    }
    match id {
        "debian" | "ubuntu" => Some((id, code)),
        _ => None,
    }
}

fn read_os_release() -> io::Result<String> {
    let path = std::ffi::CString::new("/usr/lib/os-release")
        .map_err(|_| io::Error::other("os-release"))?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::other("os-release"));
    }
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    let st = file.metadata()?;
    let mode = st.mode();
    if !st.is_file()
        || st.uid() != 0
        || mode & 0o022 != 0
        || mode & 0o004 == 0
        || st.size() == 0
        || st.size() > 4096
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "os-release metadata",
        ));
    }
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    Ok(buf)
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    out.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_source_rejects_extra_fields() {
        let ok = exact_source(DEBIAN_ARCHIVE, "trixie", DEBIAN_KEY_GUEST).unwrap();
        assert!(ok.contains("Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg"));
        assert!(!ok.contains("Enabled:"));
        assert!(vet_exact(&ok, DEBIAN_ARCHIVE, "trixie", "main", DEBIAN_KEY_GUEST).is_ok());
        let ubuntu = exact_source(UBUNTU_ARCHIVE, "resolute", UBUNTU_KEY_GUEST).unwrap();
        assert!(ubuntu.contains("https://archive.ubuntu.com/ubuntu"));
        assert!(ubuntu.contains("Suites: resolute\n"));
        assert!(!ubuntu.contains("backports"));
        assert!(exact_source(DEBIAN_ARCHIVE, "trixie", UBUNTU_KEY_GUEST).is_err());
        assert!(exact_source(UBUNTU_ARCHIVE, "resolute", DEBIAN_KEY_GUEST).is_err());
        assert!(exact_source(DEBIAN_ARCHIVE, "trixie", "/etc/apt/auth.conf").is_err());
        for bad in [
            ok.clone() + "trusted=yes\n",
            ok.replace(DEBIAN_ARCHIVE, "https://deb.debian.org/debian.evil"),
            ok.replace(DEBIAN_ARCHIVE, "https://user@deb.debian.org/debian"),
            ok.replace(
                "Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg",
                "Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg\nSigned-By: /tmp/k",
            ),
            ok.replace(DEBIAN_KEY_GUEST, "/etc/apt/auth.conf"),
        ] {
            assert!(
                vet_exact(&bad, DEBIAN_ARCHIVE, "trixie", "main", DEBIAN_KEY_GUEST).is_err(),
                "{bad}"
            );
        }
        assert!(exact_source(DEBIAN_ARCHIVE, "Trixie", DEBIAN_KEY_GUEST).is_err());
        assert!(profile("ID=fedora\nVERSION_CODENAME=41\n").is_none());
        assert_eq!(
            profile("ID=debian\nVERSION_CODENAME=trixie\n"),
            Some(("debian", "trixie"))
        );
    }
}
