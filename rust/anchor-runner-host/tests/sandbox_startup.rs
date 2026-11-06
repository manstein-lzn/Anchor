use std::{
    ffi::OsString,
    fs,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct HostProcess(Child);

impl Drop for HostProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn entries(path: &Path) -> Vec<OsString> {
    let mut names = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn missing_private_proc_exits_before_listening_or_mutating_deployment() {
    const PROC_ERROR: &str = "bwrap: Can't mount proc on /newroot/proc: Operation not permitted";
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let facts = state.join("facts");
    fs::create_dir_all(&facts).unwrap();
    let marker = facts.join(".anchor-write-Ab123x");
    let retained = b"retained state must not be cleaned during a failed startup\n";
    fs::write(&marker, retained).unwrap();
    let original_modes =
        [&state, &facts, &marker].map(|path| fs::metadata(path).unwrap().permissions().mode());
    let workspace = root.path().join("workspace");
    let catalog = root.path().join("catalog");
    let library = root.path().join("library");
    let bwrap = root.path().join("bwrap");
    fs::write(
        &bwrap,
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$0.argv\"\n\
             for argument do\n\
               if [ \"$argument\" = --proc ]; then\n\
                 printf '%s\\n' \"{PROC_ERROR}\" >&2\n\
                 exit 1\n\
               fi\n\
             done\n\
             exit 0\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&bwrap, fs::Permissions::from_mode(0o700)).unwrap();

    // Holding the address makes a misplaced bind fail before the proc probe,
    // rather than relying on a race to observe a short-lived listener.
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let stderr_path = root.path().join("host.stderr");
    let mut child = HostProcess(
        Command::new(env!("CARGO_BIN_EXE_anchor-runner-host"))
            .arg("serve")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("ANCHOR_BWRAP", &bwrap)
            .env("ANCHOR_RUNNER_STATE_ROOT", &state)
            .env("ANCHOR_RUNNER_WORKSPACE_ROOT", &workspace)
            .env("ANCHOR_RUNNER_CATALOG_ROOT", &catalog)
            .env("ANCHOR_RUNNER_LIBRARY_ROOT", &library)
            .env(
                "ANCHOR_RUNNER_LISTEN",
                reserved.local_addr().unwrap().to_string(),
            )
            .current_dir(root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&stderr_path).unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "Host did not exit after the proc probe failed: {}",
            fs::read_to_string(&stderr_path).unwrap()
        );
        thread::sleep(Duration::from_millis(10));
    };
    let stderr = fs::read_to_string(&stderr_path).unwrap();
    assert!(!status.success(), "{stderr}");
    assert!(
        stderr.contains("required sandbox capabilities are unavailable"),
        "{stderr}"
    );
    assert!(stderr.contains(PROC_ERROR), "{stderr}");

    let arguments = fs::read(root.path().join("bwrap.argv")).unwrap();
    let arguments = arguments.split(|byte| *byte == 0).collect::<Vec<_>>();
    assert!(arguments.contains(&b"--unshare-all".as_slice()));
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == [b"--proc".as_slice(), b"/proc".as_slice()])
    );
    assert_eq!(entries(&state), vec![OsString::from("facts")]);
    assert_eq!(
        entries(&facts),
        vec![marker.file_name().unwrap().to_owned()]
    );
    assert_eq!(fs::read(&marker).unwrap(), retained);
    assert_eq!(
        [&state, &facts, &marker].map(|path| fs::metadata(path).unwrap().permissions().mode()),
        original_modes
    );
    for path in [&workspace, &catalog, &library] {
        assert!(!path.exists(), "startup created {}", path.display());
    }
}
