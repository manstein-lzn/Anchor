//! Best-effort workspace observations, always under the invocation's authority.

use super::envelope::InvocationEnvelope;
use anchor_facts::{Fact, FingerprintPhase};
use anchor_runtime::{SandboxPort, SandboxRequest, SandboxStatus};
use std::{collections::BTreeMap, path::Path, time::Duration};

const LOCK_FILES: [&str; 5] = [
    "pixi.lock",
    "Cargo.lock",
    "rust-toolchain.toml",
    "uv.lock",
    "package-lock.json",
];

// The script and its positional arguments are code constants, never workspace
// content. Even repository-configured helpers (e.g. core.fsmonitor) execute only
// inside command_sandbox(), not with the host process's authority.
const SCRIPT: &str = r#"set -eu
export GIT_OPTIONAL_LOCKS=0
[ "$(git rev-parse --is-inside-work-tree)" = true ] || exit 1
head=$(git rev-parse --verify -q HEAD) || head=
status=$(git status --porcelain=v1 --untracked-files=normal)
dirty=false
[ -z "$status" ] || dirty=true
printf 'workspace-fingerprint-v1\nhead\t%s\ndirty\t%s\n' "$head" "$dirty"
for file do
    if [ -f "$file" ]; then
        sha256sum -- "$file"
    fi
done
printf 'end\n'
"#;

/// Missing tools, non-git workspaces, timeouts and malformed output all omit the
/// observation. A failed diagnostic must never change the invocation's outcome.
pub(crate) async fn capture(
    envelope: &InvocationEnvelope,
    workspace: &Path,
    phase: FingerprintPhase,
) -> Option<Fact> {
    let mut request = SandboxRequest::new(
        workspace,
        ["/bin/sh", "-c", SCRIPT, "workspace-fingerprint"]
            .into_iter()
            .chain(LOCK_FILES),
    );
    request.network = envelope.network;
    request.timeout = envelope.timeout_ceiling.min(Duration::from_secs(2));
    request.max_output_bytes = 4096;
    envelope
        .environment
        .apply_to_workspace_command(&mut request);
    let result = envelope.command_sandbox().run(request).await.ok()?;
    if result.status != SandboxStatus::Completed || result.exit_code != Some(0) || result.incomplete
    {
        return None;
    }
    parse(&result.stdout, phase)
}

fn parse(output: &str, phase: FingerprintPhase) -> Option<Fact> {
    let mut lines = output.lines();
    if lines.next()? != "workspace-fingerprint-v1" {
        return None;
    }
    let head = lines.next()?.strip_prefix("head\t")?;
    let head = if head.is_empty() {
        // A valid git workspace may not have its first commit yet.
        None
    } else if hex(head, 40) || hex(head, 64) {
        Some(head.to_owned())
    } else {
        return None;
    };
    let dirty = match lines.next()?.strip_prefix("dirty\t")? {
        "true" => Some(true),
        "false" => Some(false),
        _ => return None,
    };
    let mut files = BTreeMap::new();
    loop {
        let line = lines.next()?;
        if line == "end" {
            return lines
                .next()
                .is_none()
                .then_some(Fact::WorkspaceFingerprint {
                    phase,
                    head,
                    dirty,
                    files,
                });
        }
        let (digest, path) = line.split_once("  ")?;
        if !LOCK_FILES.contains(&path)
            || !hex(digest, 64)
            || files.insert(path.to_owned(), digest.to_owned()).is_some()
        {
            return None;
        }
    }
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_sandbox_bwrap::{BubblewrapPolicy, BubblewrapSandbox};
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn parses_head_including_unborn_and_sha256_repositories() {
        for head in ["".to_owned(), "a".repeat(40), "b".repeat(64)] {
            let output = format!("workspace-fingerprint-v1\nhead\t{head}\ndirty\tfalse\nend\n");
            let fact = parse(&output, FingerprintPhase::Start).unwrap();
            assert!(matches!(
                fact,
                Fact::WorkspaceFingerprint { head: observed, .. }
                    if observed == (!head.is_empty()).then_some(head)
            ));
        }
    }

    #[test]
    fn parses_dirty_without_recording_status_paths() {
        for dirty in [true, false] {
            let output = format!("workspace-fingerprint-v1\nhead\t\ndirty\t{dirty}\nend\n");
            assert!(matches!(
                parse(&output, FingerprintPhase::End),
                Some(Fact::WorkspaceFingerprint { dirty: Some(observed), .. }) if observed == dirty
            ));
        }
    }

    #[test]
    fn absent_lock_files_are_not_invented() {
        let digest = "c".repeat(64);
        let output =
            format!("workspace-fingerprint-v1\nhead\t\ndirty\tfalse\n{digest}  Cargo.lock\nend\n");
        assert!(matches!(
            parse(&output, FingerprintPhase::Start),
            Some(Fact::WorkspaceFingerprint { files, .. })
                if files == BTreeMap::from([("Cargo.lock".into(), digest)])
        ));
    }

    #[test]
    fn malformed_or_incomplete_output_is_omitted() {
        let prefix = "workspace-fingerprint-v1\nhead\t\ndirty\tfalse\n";
        let digest = "c".repeat(64);
        for output in [
            "garbage".into(),
            "workspace-fingerprint-v1\nhead\tprivate text\ndirty\tfalse\nend\n".into(),
            "workspace-fingerprint-v1\nhead\t\ndirty\tunknown\nend\n".into(),
            prefix.into(),
            format!("{prefix}not-a-digest  Cargo.lock\nend\n"),
            format!("{prefix}{digest}  /workspace/Cargo.lock\nend\n"),
            format!("{prefix}{digest}  other.lock\nend\n"),
            format!("{prefix}{digest}  Cargo.lock\n{digest}  Cargo.lock\nend\n"),
            format!("{prefix}end\nextra\n"),
        ] {
            assert!(
                parse(&output, FingerprintPhase::Start).is_none(),
                "{output}"
            );
        }
    }

    #[tokio::test]
    async fn real_command_sandbox_contains_repository_helpers_and_observes_changes() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let hidden = root.path().join("host-only");
        fs::write(&hidden, "host-only-marker").unwrap();
        let floor = BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(&workspace),
        )
        .unwrap();
        let envelope = super::super::envelope::invocation_envelope(
            &floor,
            crate::tool_environment::ToolEnvironment::default(),
            &[],
            false,
            &[],
            Vec::new(),
            None,
        )
        .unwrap();
        assert!(
            capture(&envelope, &workspace, FingerprintPhase::Start)
                .await
                .is_none()
        );

        // Fixture setup also runs in bwrap: no host git, including in this test.
        const SETUP: &str = "set -eu; git init -q; git -c user.name=Fixture -c user.email=fixture@example.invalid commit -q --allow-empty -m fixture; git config core.fsmonitor /workspace/.git/monitor";
        let mut request = SandboxRequest::new(&workspace, ["/bin/sh", "-c", SETUP]);
        envelope
            .environment
            .apply_to_workspace_command(&mut request);
        let setup = envelope.command_sandbox().run(request).await.unwrap();
        assert_eq!(setup.exit_code, Some(0), "{setup:?}");
        let monitor = workspace.join(".git/monitor");
        fs::write(
            &monitor,
            "#!/bin/sh\nset -eu\ntest \"$PWD\" = /workspace\ntest -e /proc/self/status\ntest ! -e .git/host-marker\nprintf sandbox > .git/monitor-observed\nprintf 'token\\0'\n",
        )
        .unwrap();
        fs::set_permissions(&monitor, fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&hidden, workspace.join(".git/host-marker")).unwrap();
        assert!(workspace.join(".git/host-marker").is_file());

        let start = capture(&envelope, &workspace, FingerprintPhase::Start)
            .await
            .unwrap();
        let Fact::WorkspaceFingerprint {
            head,
            dirty,
            files,
            phase,
        } = start
        else {
            panic!("expected fingerprint");
        };
        assert_eq!(phase, FingerprintPhase::Start);
        assert!(head.as_deref().is_some_and(|value| hex(value, 40)));
        assert_eq!(dirty, Some(false));
        assert!(files.is_empty());
        assert_eq!(
            fs::read_to_string(workspace.join(".git/monitor-observed")).unwrap(),
            "sandbox"
        );

        fs::write(workspace.join("Cargo.lock"), "fixture-lock\n").unwrap();
        let end = capture(&envelope, &workspace, FingerprintPhase::End)
            .await
            .unwrap();
        assert_eq!(
            end,
            Fact::WorkspaceFingerprint {
                phase: FingerprintPhase::End,
                head,
                dirty: Some(true),
                files: BTreeMap::from([(
                    "Cargo.lock".into(),
                    anchor_facts::text_digest("fixture-lock\n")
                )]),
            }
        );
    }
}
