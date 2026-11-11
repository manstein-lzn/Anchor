use std::{fs, os::unix::fs::PermissionsExt, process::Command};

const WEB_URL: &str = "http://127.0.0.1:8077";

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for program in ["systemctl", "journalctl", "xdg-open"] {
            let path = root.path().join(program);
            fs::write(
                &path,
                "#!/bin/sh\n\
                 printf '%s\\n' \"${0##*/}\" \"$@\" >> \"$ANCHOR_TEST_CALL\"\n\
                 printf '%s' \"${ANCHOR_TEST_STDOUT-}\"\n\
                 printf '%s' \"${ANCHOR_TEST_STDERR-}\" >&2\n\
                 exit \"${ANCHOR_TEST_EXIT-0}\"\n",
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self(root)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_anchor"));
        command
            .env_clear()
            .env("PATH", self.0.path())
            .env("ANCHOR_TEST_CALL", self.0.path().join("calls"));
        command
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.0.path().join("calls")).unwrap_or_default()
    }
}

#[test]
fn help_version_and_invalid_arguments_do_not_invoke_system_tools() {
    let fixture = Fixture::new();
    let output = fixture.command().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in ["start", "stop", "restart", "status", "logs", "open"] {
        assert!(help.contains(command), "{help}");
    }
    assert!(!help.contains("serve"), "{help}");
    let output = fixture.command().arg("--version").output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains(env!("CARGO_PKG_VERSION")));
    for arguments in [
        ["start", "other.service"],
        ["status", "--user"],
        ["serve", "other.service"],
        ["serve", "--listen=0.0.0.0:8077"],
    ] {
        let output = fixture.command().args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    assert!(fixture.calls().is_empty());
}

#[test]
fn lifecycle_uses_the_fixed_system_unit_and_relays_output_and_exit_status() {
    for (operation, exit) in [
        ("start", 0),
        ("stop", 5),
        ("restart", 1),
        ("status", 3),
        ("status", 4),
    ] {
        let fixture = Fixture::new();
        let output = fixture
            .command()
            .arg(operation)
            .env("ANCHOR_TEST_EXIT", exit.to_string())
            .env("ANCHOR_TEST_STDOUT", "service-output\n")
            .env("ANCHOR_TEST_STDERR", "service-error\n")
            .env("ANCHOR_MODEL_API_KEY", "must-not-appear")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(exit));
        let expected_output = if operation == "status" {
            format!("{WEB_URL}\nservice-output\n")
        } else {
            "service-output\n".to_owned()
        };
        assert_eq!(output.stdout, expected_output.as_bytes());
        assert_eq!(output.stderr, b"service-error\n");
        let pager = if operation == "status" {
            "--no-pager\n"
        } else {
            ""
        };
        assert_eq!(
            fixture.calls(),
            format!("systemctl\n{operation}\nanchor.service\n{pager}")
        );
    }
}

#[test]
fn default_command_shows_the_local_address_and_preserves_inactive_status() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .env("ANCHOR_TEST_EXIT", "3")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.stdout, format!("{WEB_URL}\n").as_bytes());
    assert_eq!(
        fixture.calls(),
        "systemctl\nstatus\nanchor.service\n--no-pager\n"
    );
}

#[test]
fn logs_selects_only_the_service_with_a_fixed_limit_and_optional_follow() {
    for follow in [false, true] {
        let fixture = Fixture::new();
        let mut command = fixture.command();
        command.arg("logs").env("ANCHOR_TEST_EXIT", "6");
        if follow {
            command.arg("--follow");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(6));
        let follow_argument = if follow { "--follow\n" } else { "" };
        assert_eq!(
            fixture.calls(),
            format!("journalctl\n-u\nanchor.service\n-n100\n--no-pager\n{follow_argument}")
        );
    }
}

#[test]
fn headless_open_only_prints_the_local_url_without_starting_anything() {
    let fixture = Fixture::new();
    let output = fixture.command().arg("open").output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, format!("{WEB_URL}\n").as_bytes());
    assert!(output.stderr.is_empty());
    assert!(fixture.calls().is_empty());
}

#[test]
fn desktop_open_never_runs_as_root_and_browser_failure_is_not_a_service_failure() {
    for display in ["DISPLAY", "WAYLAND_DISPLAY"] {
        let fixture = Fixture::new();
        let output = fixture
            .command()
            .arg("open")
            .env(display, "desktop-value-must-not-appear")
            .env("ANCHOR_TEST_EXIT", "17")
            .env("ANCHOR_TEST_STDOUT", "browser-output-must-not-appear")
            .env("ANCHOR_TEST_STDERR", "browser-error-must-not-appear")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, format!("{WEB_URL}\n").as_bytes());
        if rustix::process::getuid().is_root() || rustix::process::geteuid().is_root() {
            assert!(fixture.calls().is_empty());
            assert!(output.stderr.is_empty());
        } else {
            assert_eq!(fixture.calls(), format!("xdg-open\n{WEB_URL}\n"));
            assert_eq!(
                output.stderr,
                format!("anchor: browser unavailable; open {WEB_URL} manually\n").as_bytes()
            );
        }
    }
}

#[test]
fn missing_system_tool_reports_failure_without_using_a_fallback() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.0.path().join("systemctl")).unwrap();
    let output = fixture.command().arg("start").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("anchor: systemctl:"));
    assert!(fixture.calls().is_empty());
}
