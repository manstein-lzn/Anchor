use super::*;
use crate::{
    application::{RunApplication, channel_management::fake::FakeChannels},
    node_tools::NodeTools,
    run_metadata::{AssistantSource, RunMetadata},
};
use anchor_platform_session::{ChannelIdentity, ChannelInboundRequest, SessionStore, TurnStatus};
use anchor_runtime::graph::InvocationKey;
use anchor_sandbox_bwrap::{BubblewrapPolicy, BubblewrapSandbox};
use serde_json::json;
use std::{fs, os::unix::fs::symlink, path::Path, sync::atomic::AtomicBool, time::Duration};

const GRAPH: &str = "fixture-manager";

fn grants(project: &Path, owner: &str, exclude: &[&str]) -> Arc<ProjectGrants> {
    Arc::new(
        ProjectGrants::parse(
            &json!({GRAPH:{"owner":owner,"path":project,"exclude":exclude}}).to_string(),
        )
        .unwrap(),
    )
}

#[test]
fn grants_require_explicit_identities_canonical_directories_and_existing_exclusions() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir_all(project.join(".local")).unwrap();
    fs::write(project.join("file.txt"), "not a directory").unwrap();
    symlink(&project, root.path().join("alias")).unwrap();
    symlink(project.join(".local"), project.join("linked")).unwrap();
    let valid = json!({"owner":"local","path":project,"exclude":[".local"]});
    let parsed = grants(&project, "local", &[".local"]);
    parsed.0[GRAPH].verify().unwrap();
    assert!(ProjectGrants::parse("{}").unwrap().0.is_empty());
    assert!(
        ProjectGrants::parse(&json!({GRAPH:{"owner":"local","path":project}}).to_string()).is_ok()
    );
    for invalid in [
        json!([]),
        json!({"*":valid}),
        json!({"../manager":valid}),
        json!({GRAPH:{"path":project}}),
    ] {
        assert!(
            ProjectGrants::parse(&invalid.to_string()).is_err(),
            "{invalid}"
        );
    }
    for (field, value) in [
        ("owner", json!("*")),
        ("owner", json!("../local")),
        ("path", json!("/")),
        ("path", json!("relative-project")),
        ("path", json!(root.path().join("alias"))),
        ("path", json!(project.join("file.txt"))),
        ("path", json!(project.join("missing"))),
        ("exclude", json!([""])),
        ("exclude", json!([".."])),
        ("exclude", json!(["/tmp"])),
        ("exclude", json!([".local/../.local"])),
        ("exclude", json!(["missing"])),
        ("exclude", json!(["file.txt"])),
        ("exclude", json!(["linked"])),
        ("unexpected", json!(true)),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = value;
        assert!(
            ProjectGrants::parse(&json!({GRAPH:invalid}).to_string()).is_err(),
            "accepted invalid grant: {invalid}"
        );
    }
}

#[test]
fn replacing_the_project_or_linking_an_exclusion_invalidates_the_grant() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let moved = root.path().join("original");
    fs::create_dir_all(project.join(".local")).unwrap();
    let parsed = grants(&project, "local", &[".local"]);
    let grant = &parsed.0[GRAPH];
    grant.verify().unwrap();
    fs::rename(&project, &moved).unwrap();
    fs::create_dir_all(project.join(".local")).unwrap();
    assert!(grant.verify().unwrap_err().contains("directory changed"));
    fs::remove_dir_all(&project).unwrap();
    fs::rename(&moved, &project).unwrap();
    grant.verify().unwrap();
    fs::rename(project.join(".local"), root.path().join("protected")).unwrap();
    symlink(root.path().join("protected"), project.join(".local")).unwrap();
    assert!(grant.verify().is_err());
}

struct Fixture {
    root: tempfile::TempDir,
    project: PathBuf,
    application: RunApplication,
    key: InvocationKey,
    channels: Arc<FakeChannels>,
    sessions: SessionStore,
    session: String,
    turn: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join(".local/state");
        let library = root.path().join(".local/library");
        let project = root.path().join("project");
        for directory in [state.join("platform"), library.clone(), project.clone()] {
            fs::create_dir_all(directory).unwrap();
        }
        let channels = Arc::new(FakeChannels::new().with(FakeChannels::bot(
            GRAPH,
            "local",
            "work",
            &["alice"],
        )));
        let sessions = SessionStore::open(state.join("platform/sessions.sqlite")).unwrap();
        let admitted = sessions
            .admit_channel_inbound(
                "local",
                ChannelInboundRequest {
                    inbound_id: "initial".into(),
                    identity: ChannelIdentity {
                        source: "wecom".into(),
                        account: Some("corp-a".into()),
                        conversation_kind: Default::default(),
                        conversation_id: "project-private".into(),
                        sender_id: "alice".into(),
                    },
                    graph: GRAPH.into(),
                    reply_node: "work".into(),
                    text: Some("inspect the project".into()),
                    attachments: Default::default(),
                    run_id: None,
                    replace_running: true,
                    bot_origin: None,
                    platform_message_id: None,
                },
            )
            .unwrap();
        let session = admitted.session.id;
        sessions
            .bind_channel_assistant("local", &session, "run", "wait", "work", "reply")
            .unwrap();
        let input = sessions
            .claim_assistant_input("local", &session, "run", "initial")
            .unwrap()
            .unwrap();
        let key = InvocationKey {
            run_id: "run".into(),
            graph_digest: "digest".into(),
            node_id: "work".into(),
            invocation: 2,
        };
        let mut metadata =
            RunMetadata::new("run".into(), GRAPH.into(), "digest".into(), root.path()).unwrap();
        metadata.oauth_owner = Some("local".into());
        metadata.assistant = Some(AssistantSource {
            owner: "local".into(),
            session: session.clone(),
            wait_node: "wait".into(),
            work_node: "work".into(),
            reply_node: "reply".into(),
        });
        crate::run_metadata::save(&state, &metadata).unwrap();
        let turn =
            crate::assistant::bind(&state, &key, "initial".into(), &input, vec![], false, None)
                .unwrap()
                .turn;
        let mut application =
            RunApplication::new(state, library).with_channel_management(channels.clone());
        application.project_workspaces = grants(&project, "local", &[]);
        Self {
            root,
            project,
            application,
            key,
            channels,
            sessions,
            session,
            turn,
        }
    }

    fn stop_turn(&self) {
        self.sessions
            .finish_turn(
                "local",
                &self.session,
                &self.turn,
                TurnStatus::Stopped,
                None,
            )
            .unwrap();
    }

    fn tools(&self) -> ProjectTools {
        let grant = self.application.authorized_project(&self.key).unwrap();
        let cancellation = Arc::new(AtomicBool::new(false));
        let sandbox = BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", ["sh"]).authorize_workspace_root(&self.project),
        )
        .unwrap();
        let application = self.application.clone();
        let key = self.key.clone();
        ProjectTools {
            inner: Arc::new(NoTools),
            tools: Arc::new(NodeTools::new(
                Arc::new(NoTools),
                Arc::new(sandbox),
                self.project.clone(),
                vec![],
                cancellation.clone(),
            )),
            authorize: Arc::new(move || application.authorized_project(&key)),
            grant,
            cancellation,
        }
    }
}

#[test]
fn only_the_granted_graph_owner_and_current_assistant_work_turn_are_authorized() {
    let mut fixture = Fixture::new();
    assert!(fixture.application.authorized_project(&fixture.key).is_ok());
    let allowed = fixture.application.project_workspaces.clone();
    fixture.application.project_workspaces = Arc::default();
    assert!(
        fixture
            .application
            .authorized_project(&fixture.key)
            .is_err()
    );
    fixture.application.project_workspaces = grants(&fixture.project, "another-owner", &[]);
    assert!(
        fixture
            .application
            .authorized_project(&fixture.key)
            .err()
            .unwrap()
            .contains("owner")
    );
    fixture.application.project_workspaces = allowed;

    let original = fixture
        .application
        .metadata(&fixture.key.run_id)
        .unwrap()
        .unwrap();
    let mut changed = original.clone();
    changed.graph = "ungranted-manager".into();
    crate::run_metadata::save(&fixture.application.data_root, &changed).unwrap();
    assert!(
        fixture
            .application
            .authorized_project(&fixture.key)
            .is_err()
    );
    changed = original.clone();
    changed.assistant = None;
    crate::run_metadata::save(&fixture.application.data_root, &changed).unwrap();
    assert!(
        fixture
            .application
            .authorized_project(&fixture.key)
            .is_err()
    );
    crate::run_metadata::save(&fixture.application.data_root, &original).unwrap();

    for key in [
        InvocationKey {
            run_id: "missing-run".into(),
            ..fixture.key.clone()
        },
        InvocationKey {
            graph_digest: "another-digest".into(),
            ..fixture.key.clone()
        },
        InvocationKey {
            node_id: "wait".into(),
            ..fixture.key.clone()
        },
        InvocationKey {
            invocation: fixture.key.invocation + 1,
            ..fixture.key.clone()
        },
    ] {
        assert!(
            fixture.application.authorized_project(&key).is_err(),
            "{key:?}"
        );
    }
    let bot = fixture.channels.get(GRAPH).unwrap();
    for change in 0..4 {
        fixture.channels.update(GRAPH, |current| match change {
            0 => current.enabled = false,
            1 => current.owner = "another-owner".into(),
            2 => current.account = Some("another-account".into()),
            _ => current.reply_node = Some("another-node".into()),
        });
        assert!(
            fixture
                .application
                .authorized_project(&fixture.key)
                .is_err()
        );
        fixture.channels.put(bot.clone());
    }
    assert!(fixture.application.authorized_project(&fixture.key).is_ok());
    fixture.stop_turn();
    assert!(
        fixture
            .application
            .authorized_project(&fixture.key)
            .err()
            .unwrap()
            .contains("active assistant Turn")
    );
    fixture
        .sessions
        .retire_channel_assistant("local", &fixture.session, &fixture.key.run_id)
        .unwrap();
    assert!(
        fixture
            .application
            .authorized_project(&fixture.key)
            .err()
            .unwrap()
            .contains("retired")
    );
}

#[test]
fn a_project_containing_host_state_and_library_must_exclude_both() {
    let mut fixture = Fixture::new();
    for exclude in [vec![], vec![".local/state"], vec![".local/library"]] {
        fixture.application.project_workspaces = grants(fixture.root.path(), "local", &exclude);
        let error = fixture
            .application
            .authorized_project(&fixture.key)
            .err()
            .expect("Host directories must stay protected");
        assert!(
            error.contains("must exclude Host state and library"),
            "{error}"
        );
    }
    for exclude in [vec![".local"], vec![".local/state", ".local/library"]] {
        fixture.application.project_workspaces = grants(fixture.root.path(), "local", &exclude);
        assert!(fixture.application.authorized_project(&fixture.key).is_ok());
    }
    fixture.application.project_workspaces = grants(&fixture.application.data_root, "local", &[]);
    assert!(
        fixture
            .application
            .authorized_project(&fixture.key)
            .is_err()
    );

    let library = fixture.project.join("library");
    fs::create_dir(&library).unwrap();
    let alias = fixture.root.path().join("library-alias");
    symlink(&library, &alias).unwrap();
    fixture.application.library_root_override = Some(alias.clone());
    fixture.application.project_workspaces = grants(&fixture.project, "local", &[]);
    let error = fixture
        .application
        .authorized_project(&fixture.key)
        .err()
        .expect("a Library alias must not bypass project exclusions");
    assert!(
        error.contains("must exclude Host state and library"),
        "{error}"
    );
    fixture.application.project_workspaces = grants(&fixture.project, "local", &["library"]);
    assert!(fixture.application.authorized_project(&fixture.key).is_ok());
    fs::remove_file(&alias).unwrap();
    assert!(
        fixture
            .application
            .authorized_project(&fixture.key)
            .is_err()
    );
}

#[tokio::test]
async fn busy_project_tools_have_no_effect_and_recheck_the_turn_after_release() {
    let fixture = Fixture::new();
    let path = fixture.project.join("notes.txt");
    fs::write(&path, "original").unwrap();
    let tools = fixture.tools();
    let edit = json!({"path":"notes.txt","old_string":"original","new_string":"changed"});
    let lease = anchor_file_lease::ProjectLease::acquire(&fixture.project).unwrap();
    for (name, arguments) in [
        ("project_read", json!({"path":"notes.txt"})),
        ("project_edit", edit.clone()),
        (
            "project_run",
            json!({"command":["sh","-c","printf started > started.txt"]}),
        ),
    ] {
        let error = tools.call(name, arguments).await.unwrap_err().to_string();
        assert!(
            error.contains("project busy") && error.contains("no operation started"),
            "{name}: {error}"
        );
    }
    assert_eq!(fs::read(&path).unwrap(), b"original");
    assert_eq!(fs::read_dir(&fixture.project).unwrap().count(), 1);
    drop(lease);
    tools
        .call("project_read", json!({"path":"notes.txt"}))
        .await
        .unwrap();
    fixture.stop_turn();
    let error = tools
        .call("project_edit", edit)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("active assistant Turn"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), b"original");
    assert!(!fixture.project.join("started.txt").exists());
    anchor_file_lease::ProjectLease::acquire(&fixture.project).unwrap();
}

#[tokio::test]
async fn dropping_the_caller_keeps_the_project_leased_until_cancellation_finishes() {
    let fixture = Fixture::new();
    let tools = Arc::new(fixture.tools());
    let caller_tools = tools.clone();
    let caller = tokio::spawn(async move {
        caller_tools.call(
            "project_run",
            json!({"command":["sh","-c","set -eu; printf started > started.txt; while [ ! -f continue.txt ]; do sleep 0.02; done; printf alive > after-drop.txt; while :; do sleep 1; done"],"timeout_seconds":10}),
        ).await
    });
    let started = tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.project.join("started.txt").is_file() {
            assert!(
                !caller.is_finished(),
                "project command exited before its checkpoint"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if started.is_err() {
        tools.cancellation.store(true, Ordering::Release);
    }
    started.expect("project command must reach its checkpoint");
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert_eq!(
        anchor_file_lease::ProjectLease::acquire(&fixture.project)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock,
        "dropping the caller must not release a live command's lease"
    );
    fs::write(fixture.project.join("continue.txt"), "continue").unwrap();
    let continued = tokio::time::timeout(Duration::from_secs(3), async {
        while !fixture.project.join("after-drop.txt").is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let while_running = anchor_file_lease::ProjectLease::acquire(&fixture.project);
    tools.cancellation.store(true, Ordering::Release);
    continued.expect("the detached command must still run under its lease");
    assert_eq!(
        while_running.unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let released = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match anchor_file_lease::ProjectLease::acquire(&fixture.project) {
                Ok(lease) => break lease,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("unexpected lease error: {error}"),
            }
        }
    })
    .await
    .expect("cancellation must finish and release the detached command's lease");
    assert_eq!(released.path(), fixture.project);
    let error = tools
        .call(
            "project_edit",
            json!({"path":"cancelled.txt","content":"must not run"}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("cancelled before execution"), "{error}");
    assert!(!fixture.project.join("cancelled.txt").exists());
}
