use super::*;

#[test]
fn optional_credentials_select_auth_servers_without_disabling_local_git() {
    let root = tempfile::tempdir().unwrap();
    let plugin = root.path().join("plugins/git");
    std::fs::create_dir_all(plugin.join("bin")).unwrap();
    std::fs::create_dir_all(plugin.join("skills/git")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        include_bytes!("../../../plugins/git/plugin.json"),
    )
    .unwrap();
    std::fs::write(
        plugin.join("skills/git/SKILL.md"),
        include_bytes!("../../../plugins/git/skills/git/SKILL.md"),
    )
    .unwrap();
    std::fs::copy(
        std::env::current_exe().unwrap(),
        plugin.join("bin/anchor-git-tools"),
    )
    .unwrap();
    let catalog = anchor_graph_host::FilePluginCatalog::new(root.path());
    let definition = catalog.definition("git").unwrap();
    let resolve = |lookup: &dyn Fn(&str) -> Option<String>| {
        catalog
            .resolve_mcp_servers_with(&definition, lookup)
            .unwrap()
            .into_iter()
            .map(|server| server.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(resolve(&|_| None), ["git"]);
    assert_eq!(
        resolve(&|name| match name {
            "ANCHOR_GIT_SSH_PRIVATE_KEY" | "ANCHOR_GIT_SSH_KNOWN_HOSTS" => Some("fixture".into()),
            _ => None,
        }),
        ["git", "ssh"]
    );
    assert_eq!(
        resolve(&|name| match name {
            "ANCHOR_GIT_HTTPS_URL" => Some("https://git.example/team/project.git".into()),
            "ANCHOR_GIT_HTTPS_USERNAME" | "ANCHOR_GIT_HTTPS_TOKEN" => Some("fixture".into()),
            _ => None,
        }),
        ["git", "https"]
    );
    assert_eq!(
        resolve(&|name| (name == "ANCHOR_GIT_SSH_CONFIG").then(|| "/operator/ssh/config".into())),
        ["git", "host"]
    );
}

fn server(workspace: &Path) -> GitServer {
    GitServer::new(
        workspace,
        Config {
            author_name: Some("Anchor fixture".into()),
            author_email: Some("fixture@example.invalid".into()),
            ..Config::default()
        },
        std::env::current_exe().unwrap(),
    )
    .unwrap()
}

async fn run(server: &GitServer, cwd: &str, args: &[&str]) -> Value {
    let result = server
        .run(
            GitRequest {
                args: args.iter().map(|s| (*s).into()).collect(),
                cwd: cwd.into(),
                timeout_seconds: 10,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "exited", "{result}");
    assert_eq!(result["exit_code"], 0, "{result}");
    result
}

#[tokio::test]
async fn native_git_clones_edits_tests_commits_pushes_and_reopens_the_same_worktree() {
    let root = tempfile::tempdir().unwrap();
    let server = server(root.path());
    run(&server, ".", &["init", "--bare", "remote.git"]).await;
    run(&server, ".", &["clone", "remote.git", "project"]).await;
    std::fs::write(root.path().join("project/answer.txt"), "42\n").unwrap();
    // Editing and project tests operate directly on the same files as Git.
    let tested = Command::new("sh")
        .args(["-c", "test \"$(cat answer.txt)\" = 42"])
        .current_dir(root.path().join("project"))
        .status()
        .await
        .unwrap();
    assert!(tested.success());
    run(&server, "project", &["switch", "-c", "fix/answer"]).await;
    run(&server, "project", &["add", "--", "answer.txt"]).await;
    run(
        &server,
        "project",
        &["commit", "-m", "Fix answer; $(false) is literal"],
    )
    .await;
    run(&server, "project", &["push", "-u", "origin", "fix/answer"]).await;
    let reopened = self::server(root.path());
    let log = run(
        &reopened,
        ".",
        &[
            "--git-dir=remote.git",
            "log",
            "fix/answer",
            "--format=%s|%an|%ae",
        ],
    )
    .await;
    assert_eq!(
        log["stdout"],
        "Fix answer; $(false) is literal|Anchor fixture|fixture@example.invalid\n"
    );
    let content = run(
        &reopened,
        ".",
        &["--git-dir=remote.git", "show", "fix/answer:answer.txt"],
    )
    .await;
    assert_eq!(content["stdout"], "42\n");
    assert_eq!(
        run(&reopened, "project", &["status", "--porcelain"]).await["stdout"],
        ""
    );
    let failed = reopened
        .run(
            GitRequest {
                args: vec!["not-a-git-command".into()],
                cwd: ".".into(),
                timeout_seconds: 10,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_ne!(failed["exit_code"], 0);
    assert!(
        failed["stderr"]
            .as_str()
            .unwrap()
            .contains("not a git command")
    );
}

#[tokio::test]
async fn timeout_and_cancellation_reap_git_helpers_and_bound_output() {
    let root = tempfile::tempdir().unwrap();
    let server = server(root.path());
    let request = |name: &str, script: &str| GitRequest {
        args: vec!["-c".into(), format!("alias.{name}=!{script}"), name.into()],
        cwd: ".".into(),
        timeout_seconds: 1,
    };
    let output = server
        .run(
            request("many", "yes output | head -c 100000"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["truncated"], true);
    assert_eq!(output["stdout"].as_str().unwrap().len(), OUTPUT_LIMIT);
    let timed = server
        .run(
            request("slow", "echo $$ > timeout.pid; sleep 30; touch too-late"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(timed["status"], "timed_out");
    assert!(!root.path().join("too-late").exists());
    let token = CancellationToken::new();
    let call = server.run(
        request("cancel", "echo $$ > cancel.pid; sleep 30; touch too-late"),
        token.clone(),
    );
    let cancel = async {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !root.path().join("cancel.pid").exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        token.cancel();
    };
    let (cancelled, ()) = tokio::join!(call, cancel);
    assert_eq!(cancelled.unwrap()["status"], "cancelled");
    for file in ["timeout.pid", "cancel.pid"] {
        let pid = std::fs::read_to_string(root.path().join(file)).unwrap();
        let alive = std::process::Command::new("sh")
            .args(["-c", &format!("kill -0 {} 2>/dev/null", pid.trim())])
            .status()
            .unwrap();
        // A descendant can briefly remain a zombie until its namespace reaper
        // runs; no executable process may survive the cancellation boundary.
        if alive.success() {
            let stat =
                std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).unwrap_or_default();
            assert!(
                stat.is_empty() || stat.split_whitespace().nth(2) == Some("Z"),
                "{stat}"
            );
        }
    }
}

#[tokio::test]
async fn cwd_stays_in_workspace_and_configured_credentials_are_scoped_and_redacted() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("outside")).unwrap();
    let config = Config {
        https_url: Some("https://git.example/team/project.git".into()),
        https_username: Some("oauth2".into()),
        https_token: Some("fixture-private-token".into()),
        ..Config::default()
    };
    let fields = "protocol=https\nhost=git.example\npath=team/project.git\n\n";
    assert_eq!(
        credential_response(&config, "get", fields).unwrap(),
        "username=oauth2\npassword=fixture-private-token\n\n"
    );
    for fields in [
        "protocol=http\nhost=git.example\npath=team/project.git",
        "protocol=https\nhost=other.example\npath=team/project.git",
        "protocol=https\nhost=git.example:8443\npath=team/project.git",
        "protocol=https\nhost=git.example\npath=team/project.git-other",
        "protocol=https\nhost=git.example\npath=team/project.git/../other.git",
        "protocol=https\nhost=git.example\npath=another/project.git",
    ] {
        assert_eq!(credential_response(&config, "get", fields).unwrap(), "");
    }
    assert_eq!(credential_response(&config, "store", fields).unwrap(), "");
    let server = GitServer::new(root.path(), config, std::env::current_exe().unwrap()).unwrap();
    for cwd in ["../", "/tmp", "outside"] {
        assert!(
            server
                .run(
                    GitRequest {
                        args: vec!["status".into()],
                        cwd: cwd.into(),
                        timeout_seconds: 1
                    },
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
    }
    let output = run(
        &server,
        ".",
        &[
            "-c",
            "alias.print=!printf '%s' \"$ANCHOR_GIT_HTTPS_TOKEN\"",
            "print",
        ],
    )
    .await;
    assert_eq!(output["stdout"], "[REDACTED]");
}
