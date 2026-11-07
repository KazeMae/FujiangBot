use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::super::workspace_seed::VerifiedBase;
use super::super::{tree_digest, Owner, WorkspaceStore};
use super::{bind, parse_approval, parse_receipt, Approval, Receipt, POLICY, PRIMARY};

fn scratch() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = N.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("ws-acc-{n}-{id}"));
    fs::create_dir(&root).unwrap();
    mode(&root, 0o700);
    root
}

fn mode(path: &Path, bits: u32) {
    let mut perms = fs::symlink_metadata(path).unwrap().permissions();
    perms.set_mode(bits);
    fs::set_permissions(path, perms).unwrap();
}

fn dir(path: &Path) {
    fs::create_dir(path).unwrap();
    mode(path, 0o700);
}

fn file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    mode(path, 0o600);
}

fn approval() -> Approval {
    Approval {
        source: PathBuf::from("/var/empty/source"),
        host: PathBuf::from("/var/empty/host"),
        archive_sha256: "ab".repeat(32),
        tree: [7u8; 32],
    }
}

fn receipt(accepted: bool, sha: &str, tree: [u8; 32]) -> Receipt {
    Receipt {
        archive: "ubuntu-base-24.04-base-amd64.tar.gz".into(),
        sha256: sha.into(),
        bytes: 32,
        primary: PRIMARY.into(),
        extracted: true,
        execution_accepted: accepted,
        tree,
        tree_policy: POLICY.into(),
    }
}

#[test]
fn receipt_alone_is_not_approval() {
    let raw = br#"{"archive":"ubuntu-base-24.04-base-amd64.tar.gz","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","bytes":3,"primary":"843938DF228D22F7B3742BC0D94AA3F0EFE21092","extracted":true,"execution_accepted":false,"tree_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","tree_policy":"workspace-rootfs-runtime-v1"}"#;
    assert!(parse_approval(raw).is_err());
    let parsed = parse_receipt(raw).unwrap();
    assert!(!parsed.execution_accepted);
    assert!(bind(&approval(), &parsed).is_err());
}

#[test]
fn forged_or_stale_receipt_rejected() {
    let mut ok = approval();
    ok.archive_sha256 = "aa".repeat(32);
    ok.tree = [0xbb; 32];
    let good = receipt(false, &ok.archive_sha256, ok.tree);
    assert!(bind(&ok, &good).is_ok());
    assert!(bind(&ok, &receipt(true, &ok.archive_sha256, ok.tree)).is_err());
    assert!(bind(&ok, &receipt(false, &"cc".repeat(32), ok.tree)).is_err());
    assert!(bind(&ok, &receipt(false, &ok.archive_sha256, [1u8; 32])).is_err());
    let mut policy = good;
    policy.tree_policy = "other".into();
    assert!(bind(&ok, &policy).is_err());
}

#[test]
fn trust_symlink_mode_and_receipt_file_do_not_enable() {
    let root = scratch();
    let real = root.join("real");
    let host = root.join("host");
    dir(&real);
    dir(&host);
    let link = root.join("link");
    symlink(&real, &link).unwrap();
    assert!(super::acquire(&link).is_err());
    mode(&real, 0o707);
    assert!(super::acquire(&real).is_err());
    mode(&real, 0o700);
    file(
        &real.join("acceptance.json"),
        br#"{"archive":"a","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","bytes":1,"primary":"843938DF228D22F7B3742BC0D94AA3F0EFE21092","extracted":false,"execution_accepted":false}"#,
    );
    assert!(super::acquire(&real).is_err());
    assert!(!host.join("personal").exists());
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn approval_symlink_rejected() {
    let root = scratch();
    let trust = root.join("trust");
    let host = root.join("host");
    dir(&trust);
    dir(&host);
    let outside = root.join("outside.json");
    file(&outside, b"{}\n");
    symlink(&outside, trust.join("acceptance.json")).unwrap();
    assert!(super::acquire(&trust).is_err());
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn configured_approval_without_verifier_success_stays_closed() {
    let root = scratch();
    let trust = root.join("trust");
    let host = root.join("host");
    dir(&trust);
    dir(&host);
    let body = format!(
        r#"{{"version":1,"kind":"runtime_safety_approval","source":"{}/missing","host":"{}","archive_sha256":"{}","primary":"{}","policy":"{}","tree_sha256":"{}","approved":true}}"#,
        root.display(),
        host.display(),
        "ab".repeat(32),
        PRIMARY,
        POLICY,
        "cd".repeat(32),
    );
    file(&trust.join("acceptance.json"), body.as_bytes());
    let err = super::acquire(&trust).err().expect("closed");
    assert!(err.to_string().contains("verifier"), "{err}");
    assert!(!host.join("personal").exists());
    let held = trust.join("held");
    dir(&held);
    file(&held.join("sentinel"), b"keep");
    assert!(super::acquire_named(&trust, "held").is_err());
    assert_eq!(fs::read(held.join("sentinel")).unwrap(), b"keep");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn mutated_base_after_seal_does_not_publish() {
    let root = scratch();
    let host = root.join("host");
    let image = root.join("image");
    dir(&host);
    dir(&image);
    file(&image.join("payload"), b"aaaa");
    let fd = super::super::open_host(&image).unwrap();
    let digest = tree_digest(fd.as_raw_fd()).unwrap();
    drop(fd);
    let base = VerifiedBase::checking(&image, digest).unwrap();
    file(&image.join("payload"), b"bbbb");
    let err = WorkspaceStore::open(&host, Owner::personal(7).unwrap(), &base).unwrap_err();
    assert!(err.to_string().contains("tree"), "{err}");
    assert!(!host.join("personal/7/generation").exists());
    assert!(!host.join("personal/7/.stage").exists());
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn checked_clone_keeps_hardlink_and_absolute_symlink_and_adds_work() {
    let root = scratch();
    let host = root.join("host");
    let image = root.join("image");
    dir(&host);
    dir(&image);
    dir(&image.join("bin"));
    file(&image.join("bin/tool"), b"tool-bytes");
    fs::hard_link(image.join("bin/tool"), image.join("bin/alias")).unwrap();
    symlink("/proc/mounts", image.join("mtab")).unwrap();
    let fd = super::super::open_host(&image).unwrap();
    let digest = tree_digest(fd.as_raw_fd()).unwrap();
    drop(fd);
    let base = VerifiedBase::checking(&image, digest).unwrap();
    let store = WorkspaceStore::open(&host, Owner::personal(8).unwrap(), &base).unwrap();
    drop(store);
    let rootfs = host.join("personal/8/generation/rootfs");
    let tool = fs::metadata(rootfs.join("bin/tool")).unwrap();
    let alias = fs::metadata(rootfs.join("bin/alias")).unwrap();
    assert_eq!(tool.ino(), alias.ino());
    assert!(tool.nlink() >= 2);
    assert_eq!(
        fs::read_link(rootfs.join("mtab")).unwrap(),
        Path::new("/proc/mounts")
    );
    assert!(fs::metadata(rootfs.join("work")).unwrap().is_dir());
    assert!(!fs::symlink_metadata(rootfs.join("work"))
        .unwrap()
        .file_type()
        .is_symlink());
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn setid_and_sticky_bits_are_rejected() {
    let root = scratch();
    let trust = root.join("trust");
    let host = root.join("host");
    dir(&trust);
    dir(&host);
    for bits in [0o1700, 0o4700, 0o2700] {
        mode(&trust, bits);
        assert!(super::acquire(&trust).is_err(), "{bits:o}");
    }
    mode(&trust, 0o700);
    let body = format!(
        r#"{{"version":1,"kind":"runtime_safety_approval","source":"/tmp/missing-source","host":"{}","archive_sha256":"{}","primary":"{}","policy":"{}","tree_sha256":"{}","approved":true}}"#,
        host.display(),
        "ab".repeat(32),
        PRIMARY,
        POLICY,
        "cd".repeat(32),
    );
    file(&trust.join("acceptance.json"), body.as_bytes());
    mode(&host, 0o1700);
    assert!(super::acquire(&trust).is_err());
    mode(&host, 0o700);
    mode(&trust.join("acceptance.json"), 0o4600);
    assert!(super::acquire(&trust).is_err());
    assert!(!host.join("personal").exists());
    fs::remove_dir_all(&root).unwrap();
}

fn python(code: &str, arg: &str) -> Command {
    let mut cmd = Command::new("/usr/bin/python3");
    cmd.arg("-I")
        .arg("-c")
        .arg(code)
        .arg(arg)
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    cmd
}

fn alive(pid: i32) -> bool {
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

#[test]
fn stdout_cap_is_total_across_pumps() {
    let started = Instant::now();
    let err = super::collect_command(
        python(
            "import sys\nfor _ in range(40):\n sys.stdout.write('x'*300)\n sys.stdout.flush()\n",
            "",
        ),
        Instant::now() + Duration::from_secs(2),
        8192,
        65536,
        None,
    )
    .expect_err("capped");
    assert!(err.to_string().contains("verifier"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn closed_pipes_then_sleep_is_reaped_on_deadline() {
    let root = scratch();
    let pid_path = root.join("pid");
    let code = "import os,sys,time\nopen(sys.argv[1],'w').write(str(os.getpid()))\nsys.stdout.close()\nsys.stderr.close()\ntime.sleep(60)\n";
    let started = Instant::now();
    let err = super::collect_command(
        python(code, pid_path.to_str().unwrap()),
        Instant::now() + Duration::from_millis(400),
        8192,
        65536,
        None,
    )
    .expect_err("timeout");
    assert!(started.elapsed() < Duration::from_secs(3), "{err}");
    let pid: i32 = fs::read_to_string(&pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!alive(pid), "{pid}");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn failing_child_is_reaped() {
    let started = Instant::now();
    let err = super::collect_command(
        python("import sys; sys.exit(3)", ""),
        Instant::now() + Duration::from_secs(2),
        8192,
        65536,
        None,
    )
    .expect_err("exit");
    assert!(err.to_string().contains("verifier"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn timeout_kills_process_group_descendant() {
    let root = scratch();
    let marker = root.join("desc");
    let code = "import os,sys,time\npid=os.fork()\nif pid==0:\n open(sys.argv[1],'w').write(str(os.getpid()))\n time.sleep(30)\n open(sys.argv[1]+'.late','w').write('x')\n os._exit(0)\nos._exit(0)\n";
    let started = Instant::now();
    let err = super::collect_command(
        python(code, marker.to_str().unwrap()),
        Instant::now() + Duration::from_millis(500),
        8192,
        65536,
        None,
    )
    .expect_err("group");
    let pid: i32 = fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
    // Bounded termination of the orphan. The collector reaps only the direct
    // leader; init reaps the killed descendant asynchronously. A zombie still
    // answers kill(pid, 0) and must not pass.
    let observe = Instant::now() + Duration::from_secs(2);
    let budget = started + Duration::from_secs(3);
    let limit = if observe < budget { observe } else { budget };
    while Instant::now() < limit && !esrch(pid) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(esrch(pid), "descendant {pid} still visible");
    assert!(!Path::new(&format!("{}.late", marker.display())).exists());
    assert!(started.elapsed() < Duration::from_secs(3), "{err}");
    fs::remove_dir_all(&root).unwrap();
}

fn esrch(pid: i32) -> bool {
    if pid <= 0 || unsafe { libc::kill(pid, 0) } == 0 {
        return false;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[test]
fn import_lease_preserves_active_and_unmarked() {
    let root = scratch();
    let trust = root.join("trust");
    dir(&trust);
    dir(&trust.join("legacy-import"));
    file(&trust.join("legacy-import/sentinel"), b"legacy");
    dir(&trust.join("r2-2"));
    file(&trust.join("r2-2/sentinel"), b"mode");
    mode(&trust.join("r2-2"), 0o755);
    symlink("legacy-import", trust.join("r3-3")).unwrap();
    dir(&trust.join("r4-4"));
    file(&trust.join("r4-4/lease"), b"forged-marker-not-v1\n");
    file(&trust.join("r4-4/sentinel"), b"forged");
    dir(&trust.join("r5-5"));
    file(&trust.join("r5-5/lease"), b"fujiang-import-lease-v1\n");
    file(&trust.join("r5-5/sentinel"), b"nlink");
    fs::hard_link(trust.join("r5-5/lease"), trust.join("lease-alias")).unwrap();
    file(&trust.join("r9-1"), b"blocked");
    let trust_fd = super::open_mode(&trust, 0o700).unwrap();
    assert!(super::leases::create_owned(trust_fd.as_raw_fd(), "r9-1").is_err());
    assert_eq!(fs::read(trust.join("r9-1")).unwrap(), b"blocked");
    let bin = unsafe {
        libc::open(
            c"/bin".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    assert!(bin >= 0);
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstat(bin, &mut st) }, 0);
    unsafe { libc::close(bin) };
    if unsafe { libc::geteuid() } != 0 {
        assert_ne!(st.st_uid, unsafe { libc::geteuid() });
    }
    assert!(!super::leases::owned_private_dir(&st));
    let active = super::leases::create_owned(trust_fd.as_raw_fd(), "r8-8").unwrap();
    file(&trust.join("r8-8/sentinel"), b"active");
    let live = super::leases::create_owned(trust_fd.as_raw_fd(), "live-9-9").unwrap();
    assert_eq!(
        unsafe { libc::mkdirat(live.dir_fd(), c"rootfs".as_ptr(), 0o700) },
        0
    );
    let root_name = std::ffi::CString::new("rootfs").unwrap();
    let rootfs = super::open_dir(live.dir_fd(), &root_name).unwrap();
    let base = live.into_base(rootfs, [9u8; 32]).unwrap();
    let path = trust.clone();
    let concurrent = std::thread::spawn(move || {
        let fd = super::open_mode(&path, 0o700).unwrap();
        super::leases::reclaim(fd.as_raw_fd()).unwrap()
    });
    let raced = concurrent.join().unwrap();
    assert_eq!(raced.reclaimed, 0);
    let report = super::leases::reclaim(trust_fd.as_raw_fd()).unwrap();
    assert_eq!(report.reclaimed, 0, "{report:?}");
    assert_eq!(fs::read(trust.join("r8-8/sentinel")).unwrap(), b"active");
    assert!(trust.join("live-9-9").is_dir());
    assert_eq!(
        fs::read(trust.join("legacy-import/sentinel")).unwrap(),
        b"legacy"
    );
    assert_eq!(fs::read(trust.join("r2-2/sentinel")).unwrap(), b"mode");
    assert_eq!(fs::read(trust.join("r4-4/sentinel")).unwrap(), b"forged");
    assert_eq!(fs::read(trust.join("r5-5/sentinel")).unwrap(), b"nlink");
    assert!(trust
        .join("r3-3")
        .symlink_metadata()
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!report.truncated, "{report:?}");
    drop(active);
    drop(base);
    assert!(
        !trust.join("r8-8").exists(),
        "normal lease drop left its wrapper"
    );
    assert!(
        !trust.join("live-9-9").exists(),
        "normal base drop left its wrapper"
    );
    let gone = super::leases::reclaim(trust_fd.as_raw_fd()).unwrap();
    assert_eq!(gone.reclaimed, 0, "{gone:?}");
    let stale_held = super::leases::create_owned(trust_fd.as_raw_fd(), "r1-1").unwrap();
    file(&trust.join("r1-1/sentinel"), b"stale");
    stale_held.abandon();
    let stale = super::leases::reclaim(trust_fd.as_raw_fd()).unwrap();
    assert!(stale.reclaimed >= 1, "{stale:?}");
    assert!(!trust.join("r1-1").exists());
    assert_eq!(
        fs::read(trust.join("legacy-import/sentinel")).unwrap(),
        b"legacy"
    );
    assert_eq!(fs::read(trust.join("r2-2/sentinel")).unwrap(), b"mode");
    assert_eq!(fs::read(trust.join("r4-4/sentinel")).unwrap(), b"forged");
    assert_eq!(fs::read(trust.join("r5-5/sentinel")).unwrap(), b"nlink");
    assert_eq!(fs::read(trust.join("r9-1")).unwrap(), b"blocked");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn writer_lease_outlives_parent_then_gc() {
    if std::env::var_os("FUJIANG_LEASE_HELPER").is_some() {
        let trust = PathBuf::from(std::env::var("FUJIANG_LEASE_TRUST").unwrap());
        let namefile = PathBuf::from(std::env::var("FUJIANG_LEASE_NAME").unwrap());
        let pidfile = PathBuf::from(std::env::var("FUJIANG_LEASE_PID").unwrap());
        let trust_fd = super::open_mode(&trust, 0o700).unwrap();
        let name = format!("r{}-{}", std::process::id(), super::nanos().unwrap());
        let held = super::leases::create_owned(trust_fd.as_raw_fd(), &name).unwrap();
        let child = super::leases::spawn_writer(held.lease_fd(), &pidfile).unwrap();
        fs::write(&namefile, &name).unwrap();
        std::mem::forget(child);
        held.leak_for_child();
        std::process::exit(0);
    }
    let root = scratch();
    let trust = root.join("trust");
    dir(&trust);
    let namefile = root.join("name");
    let pidfile = root.join("pid");
    let helper = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("workspace_store::acceptance::tests::writer_lease_outlives_parent_then_gc")
        .env("FUJIANG_LEASE_HELPER", "1")
        .env("FUJIANG_LEASE_TRUST", &trust)
        .env("FUJIANG_LEASE_NAME", &namefile)
        .env("FUJIANG_LEASE_PID", &pidfile)
        .output()
        .unwrap();
    assert!(
        helper.status.success(),
        "{} {}",
        String::from_utf8_lossy(&helper.stdout),
        String::from_utf8_lossy(&helper.stderr)
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while !pidfile.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let name = fs::read_to_string(&namefile).unwrap();
    let pid: i32 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let trust_fd = super::open_mode(&trust, 0o700).unwrap();
    let report = super::leases::reclaim(trust_fd.as_raw_fd()).unwrap();
    assert_eq!(report.reclaimed, 0, "{report:?}");
    assert!(trust.join(name.trim()).is_dir(), "active writer removed");
    assert!(alive(pid), "{pid}");
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end && !esrch(pid) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(esrch(pid), "{pid}");
    let mut reclaimed = false;
    let stop = Instant::now() + Duration::from_secs(2);
    while Instant::now() < stop {
        let again = super::leases::reclaim(trust_fd.as_raw_fd()).unwrap();
        if again.reclaimed >= 1 && !trust.join(name.trim()).exists() {
            reclaimed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(reclaimed, "stale wrapper survived the writer");
    fs::remove_dir_all(&root).unwrap();
}
