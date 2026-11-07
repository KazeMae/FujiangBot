use super::parse_child_pid;

#[test]
fn package_budget_is_fixed_and_finite() {
    assert_eq!(super::WALL, std::time::Duration::from_secs(300));
    assert_eq!(super::CPU_SECONDS, 120);
    assert_eq!(super::TMPFS_SIZE, "67108864");
    assert_eq!(super::OUT_CAP, 64 * 1024);
    assert_eq!(super::REAP_BOUND, std::time::Duration::from_secs(1));
}

#[test]
fn parses_top_level_child_pid() {
    assert_eq!(
        parse_child_pid(
            br#"{"ok":true,"child-pid":42,"nested":{"child-pid":"9"}}"#,
            false
        ),
        Some(42)
    );
    assert_eq!(
        parse_child_pid(br#"{"child-pid":2147483647}"#, true),
        Some(2147483647)
    );
}

#[test]
fn rejects_child_pid_shapes() {
    let bad: &[&[u8]] = &[
        br#"{"child-pid":"1"}"#,
        br#"{"child-pid":-1}"#,
        br#"{"child-pid":0}"#,
        br#"{"child-pid":1.5}"#,
        br#"{"child-pid":2147483648}"#,
        br#"{"child-pid":{"n":1}}"#,
        br#"{"child-pid":1,"child-pid":2}"#,
        br#"{"child-pid":1}{}"#,
        br#"{"child-pid":"#,
        b"",
    ];
    for sample in bad {
        assert_eq!(parse_child_pid(sample, true), None);
    }
    assert_eq!(parse_child_pid(br#"{"child-pid":"#, false), None);
}

#[test]
fn child_set_limits_keeps_inherited_fsize() {
    use std::os::unix::process::CommandExt;

    let mut parent = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, &mut parent) },
        0
    );
    let parent_cur = parent.rlim_cur;
    let parent_max = parent.rlim_max;
    let mut cmd = std::process::Command::new("/bin/true");
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        cmd.pre_exec(move || {
            let mut inherited = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_FSIZE, &mut inherited) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if inherited.rlim_cur != parent_cur || inherited.rlim_max != parent_max {
                return Err(std::io::Error::other("inherited fsize differs from parent"));
            }
            super::set_limits()?;
            let mut after = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_FSIZE, &mut after) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if after.rlim_cur != inherited.rlim_cur || after.rlim_max != inherited.rlim_max {
                return Err(std::io::Error::other("set_limits changed fsize"));
            }
            Ok(())
        });
    }
    let status = cmd.status().expect("spawn /bin/true");
    assert!(status.success(), "{status}");
    let mut again = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, &mut again) },
        0
    );
    assert_eq!(again.rlim_cur, parent_cur);
    assert_eq!(again.rlim_max, parent_max);
}
