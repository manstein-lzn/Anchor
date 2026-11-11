use super::*;
use anchor_platform_session::CreateSession;
use serde_json::json;
use std::time::Duration;

#[test]
fn builtin_guidance_keeps_host_boundaries_and_discovers_development_skills_on_demand() {
    for boundary in [
        "Anchor Pilot",
        "anchor_plugins",
        "anchor_plugin_read",
        "plugin_list/plugin_read",
        "plugin_mount",
        "原生确认",
        "当前 Turn 冻结",
        "Host 在同一 Session 自动续接",
        "纯 Skill 开关只控制指导披露",
        "credential_request",
        "oauth_status",
        "channel_status",
        "run_abandon",
        "graph_delete",
        "session_wait/run_status/graph_read",
        "#anchor/graph/<id>",
        "#anchor/run/<id>",
        "#anchor/artifact/<run>/<node>/<path>",
        "不能安装替换为自己提供指令的 Plugin",
    ] {
        assert!(
            INSTRUCTIONS.contains(boundary),
            "missing Host boundary: {boundary}"
        );
    }
    for tutorial in [
        "development_prepare",
        "runtime_prepare",
        "dependencies.python",
        "Cargo.toml",
        "requirements",
        "pip",
        "rebuild",
        "discard",
        "plugin_prepare",
        "plugin_test",
    ] {
        assert!(
            !INSTRUCTIONS.contains(tutorial),
            "development tutorial remains built-in: {tutorial}"
        );
    }
    assert!(INSTRUCTIONS.chars().count() < 1800);
}

#[tokio::test]
async fn continuation_facts_keep_stop_interrupt_supersession_and_replay_distinct() {
    let fixture = Fixture::new();
    let session = fixture.create("continuation");
    let (source, _) = fixture
        .sessions
        .create_turn("owner", &session.id, "source", Some("initialize"))
        .unwrap();
    fixture
        .sessions
        .append_event(
            "owner",
            &session.id,
            "pilot.continuation.requested",
            std::collections::BTreeMap::from([
                ("turn".into(), json!(source.id)),
                (
                    "request_id".into(),
                    json!(format!("continuation:{}", source.id)),
                ),
            ]),
        )
        .unwrap();
    fixture
        .sessions
        .finish_turn("owner", &session.id, &source.id, TurnStatus::Stopped, None)
        .unwrap();
    assert_eq!(
        continuation_status(&fixture.sessions, "owner", &session.id, &source.id).unwrap()["status"],
        "cancelled"
    );
    assert!(matches!(
        fixture
            .service
            .admit(fixture.admission(&session, &format!("continuation:{}", source.id)))
            .await,
        Err(AdmissionError::Session(SessionError::Conflict(_)))
    ));
    let (interrupted, _) = fixture
        .sessions
        .create_turn("owner", &session.id, "interrupted", Some("initialize"))
        .unwrap();
    fixture
        .sessions
        .append_event(
            "owner",
            &session.id,
            "pilot.continuation.requested",
            std::collections::BTreeMap::from([
                ("turn".into(), json!(interrupted.id)),
                (
                    "request_id".into(),
                    json!(format!("continuation:{}", interrupted.id)),
                ),
            ]),
        )
        .unwrap();
    fixture
        .sessions
        .finish_turn(
            "owner",
            &session.id,
            &interrupted.id,
            TurnStatus::Interrupted,
            None,
        )
        .unwrap();
    assert_eq!(
        continuation_status(&fixture.sessions, "owner", &session.id, &interrupted.id).unwrap()["status"],
        "pending"
    );
    let (latest, _) = fixture
        .sessions
        .create_turn("owner", &session.id, "user-newer", Some("continue"))
        .unwrap();
    fixture
        .sessions
        .finish_turn(
            "owner",
            &session.id,
            &latest.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    assert_eq!(
        continuation_status(&fixture.sessions, "owner", &session.id, &interrupted.id).unwrap()["status"],
        "superseded"
    );
    assert!(matches!(
        fixture
            .service
            .admit(fixture.admission(&session, &format!("continuation:{}", interrupted.id)))
            .await,
        Err(AdmissionError::Session(SessionError::Conflict(_)))
    ));
    let id = format!("continuation:{}", latest.id);
    let (next, _) = fixture
        .sessions
        .create_turn("owner", &session.id, &id, Some("inspect"))
        .unwrap();
    fixture
        .sessions
        .finish_turn("owner", &session.id, &next.id, TurnStatus::Completed, None)
        .unwrap();
    let replay = fixture
        .service
        .admit(fixture.admission(&session, &id))
        .await;
    assert!(matches!(replay,Ok(turn) if turn.id==next.id));
    assert_eq!(
        continuation_status(&fixture.sessions, "owner", &session.id, &latest.id).unwrap()["turn"],
        next.id
    );
}

#[tokio::test]
async fn credential_submission_and_stop_share_one_control_boundary() {
    let fixture = Fixture::new();
    let session = fixture.create("credential-boundary");
    let root = scope(&fixture.data, "owner", &session);
    let cancellation = Arc::new(AtomicBool::new(false));
    fixture.service.active.lock().unwrap().insert(
        root.clone(),
        ActivePilot {
            turn: "credential-turn".into(),
            cancellation: cancellation.clone(),
        },
    );
    let guard = fixture
        .service
        .credential_submission_guard(&root)
        .await
        .unwrap();
    let service = fixture.service.clone();
    let sessions = fixture.sessions.clone();
    let data = fixture.data.clone();
    let stop = tokio::spawn(async move {
        service
            .stop_session(sessions, data, "owner".into(), session)
            .await
    });
    tokio::task::yield_now().await;
    assert!(!stop.is_finished());
    assert!(!cancellation.load(Ordering::Acquire));
    drop(guard);
    assert!(stop.await.unwrap().unwrap());
    assert!(
        fixture
            .service
            .credential_submission_guard(&root)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn plugin_mount_changes_bind_one_request_to_the_registered_running_turn() {
    let fixture = Fixture::new();
    let session = fixture.create("mount-continuation");
    fixture
        .sessions
        .set_plugin_mount(
            "owner",
            &session.id,
            "demo",
            false,
            "operator",
            "initial policy",
        )
        .unwrap();
    let (source, _) = fixture
        .sessions
        .create_turn("owner", &session.id, "source", Some("original task"))
        .unwrap();
    fixture.register(&session, &source);
    let mounts = crate::plugin_mounts::SessionMounts::open(&fixture.data).unwrap();
    crate::node_host::freeze_session_switches(&fixture.data, &session.id, &source.id, &mounts)
        .unwrap();

    let guard = fixture.service.plugin_mount_guard().await;
    for on in [true, false, true] {
        let updated = fixture
            .sessions
            .set_plugin_mount("owner", &session.id, "demo", on, "operator", "web switch")
            .unwrap();
        let (effect, continuation) = guard
            .effect(&fixture.sessions, "owner", &updated, &fixture.data, true)
            .unwrap();
        assert_eq!(
            effect,
            json!({"status":"after_current_turn","source_turn":source.id})
        );
        assert_eq!(continuation["status"], "scheduled");
        assert_eq!(
            continuation["request_id"],
            format!("continuation:{}", source.id)
        );
    }
    assert_eq!(
        fixture
            .sessions
            .events("owner", &session.id, 0)
            .unwrap()
            .iter()
            .filter(|event| event.kind == "pilot.continuation.requested")
            .count(),
        1
    );
    let unchanged = guard
        .effect(&fixture.sessions, "owner", &session, &fixture.data, false)
        .unwrap();
    assert_eq!(unchanged.0["status"], "unchanged");
    assert_eq!(unchanged.1["status"], "none");
    let frozen = crate::plugin_mounts::read_snapshot(
        &crate::plugin_mounts::snapshot_path(&fixture.data, &session.id, &source.id),
        &session.id,
        &source.id,
    )
    .unwrap();
    assert_eq!(frozen.mounts.get("demo"), Some(&false));

    let service = fixture.service.clone();
    let admission = fixture.admission(&session, "source");
    let source_id = source.id.clone();
    let settled = tokio::spawn(async move {
        service
            .settle(admission, source_id, TurnStatus::Completed, None)
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        !settled.is_finished(),
        "terminal settlement slipped between switch and request"
    );
    assert_eq!(
        fixture
            .sessions
            .get_turn("owner", &session.id, &source.id)
            .unwrap()
            .status,
        TurnStatus::Running
    );
    drop(guard);
    assert_eq!(
        settled.await.unwrap().unwrap().status,
        TurnStatus::Completed
    );
    let (next, _) = fixture
        .sessions
        .create_turn(
            "owner",
            &session.id,
            &format!("continuation:{}", source.id),
            Some("continue original task"),
        )
        .unwrap();
    let fresh =
        crate::node_host::freeze_session_switches(&fixture.data, &session.id, &next.id, &mounts)
            .unwrap();
    assert_eq!(fresh.switches.get("demo"), Some(&true));
}

#[tokio::test]
async fn idle_terminal_cancelled_and_mismatched_sources_only_save_mount_policy() {
    for terminal in [
        None,
        Some(TurnStatus::Completed),
        Some(TurnStatus::Failed),
        Some(TurnStatus::Interrupted),
        Some(TurnStatus::Stopped),
    ] {
        let fixture = Fixture::new();
        let session = fixture.create("mount-idle");
        if let Some(status) = terminal {
            let (source, _) = fixture
                .sessions
                .create_turn("owner", &session.id, "source", Some("prior task"))
                .unwrap();
            fixture.register(&session, &source);
            fixture
                .sessions
                .finish_turn("owner", &session.id, &source.id, status, None)
                .unwrap();
        }
        let guard = fixture.service.plugin_mount_guard().await;
        let updated = fixture
            .sessions
            .set_plugin_mount("owner", &session.id, "demo", true, "operator", "web switch")
            .unwrap();
        let (effect, continuation) = guard
            .effect(&fixture.sessions, "owner", &updated, &fixture.data, true)
            .unwrap();
        assert_eq!(effect["status"], "next_turn");
        assert_eq!(continuation["status"], "none");
        assert!(
            continuation_request(&fixture.sessions, "owner", &session.id, "source")
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .sessions
                .events("owner", &session.id, 0)
                .unwrap()
                .iter()
                .all(|event| event.kind != "pilot.continuation.requested")
        );
    }

    for cancelled in [false, true] {
        let fixture = Fixture::new();
        let session = fixture.create("mount-invalid-source");
        let (source, _) = fixture
            .sessions
            .create_turn("owner", &session.id, "source", Some("original task"))
            .unwrap();
        let cancellation = fixture.register(&session, &source);
        if cancelled {
            cancellation.store(true, Ordering::Release);
        } else {
            fixture
                .service
                .active
                .lock()
                .unwrap()
                .get_mut(&scope(&fixture.data, "owner", &session))
                .unwrap()
                .turn = "unrelated-turn".into();
        }
        let guard = fixture.service.plugin_mount_guard().await;
        assert_eq!(
            guard
                .effect(&fixture.sessions, "owner", &session, &fixture.data, true)
                .unwrap()
                .0["status"],
            "next_turn"
        );
        assert!(
            continuation_request(&fixture.sessions, "owner", &session.id, &source.id)
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn stop_wins_over_success_computed_before_terminal_settlement() {
    let fixture = Fixture::new();
    let session = fixture.create("stop-before-settlement");
    let (source, _) = fixture
        .sessions
        .create_turn("owner", &session.id, "source", Some("original task"))
        .unwrap();
    fixture.register(&session, &source);
    let guard = fixture.service.plugin_mount_guard().await;
    guard
        .effect(&fixture.sessions, "owner", &session, &fixture.data, true)
        .unwrap();
    drop(guard);
    let computed_result = TurnStatus::Completed;
    assert!(fixture.stop(&session).await.unwrap());
    let saved = fixture
        .service
        .settle(
            fixture.admission(&session, "source"),
            source.id.clone(),
            computed_result,
            None,
        )
        .await
        .unwrap();
    assert_eq!(saved.status, TurnStatus::Stopped);
    assert_eq!(
        continuation_status(&fixture.sessions, "owner", &session.id, &source.id).unwrap()["status"],
        "cancelled"
    );
    assert!(matches!(
        fixture
            .service
            .admit(fixture.admission(&session, &format!("continuation:{}", source.id),))
            .await,
        Err(AdmissionError::Session(SessionError::Conflict(_)))
    ));
}

#[tokio::test]
async fn failed_interrupted_and_archived_sources_cannot_auto_continue() {
    for terminal in [
        TurnStatus::Failed,
        TurnStatus::Interrupted,
        TurnStatus::Completed,
    ] {
        let fixture = Fixture::new();
        let session = fixture.create("ineligible-continuation");
        let (source, _) = fixture
            .sessions
            .create_turn("owner", &session.id, "source", Some("original task"))
            .unwrap();
        fixture.register(&session, &source);
        let guard = fixture.service.plugin_mount_guard().await;
        guard
            .effect(&fixture.sessions, "owner", &session, &fixture.data, true)
            .unwrap();
        drop(guard);
        fixture
            .service
            .settle(
                fixture.admission(&session, "source"),
                source.id.clone(),
                terminal,
                None,
            )
            .await
            .unwrap();
        if terminal == TurnStatus::Completed {
            fixture
                .sessions
                .set_status("owner", &session.id, SessionStatus::Archived, "")
                .unwrap();
        }
        assert_eq!(
            continuation_status(&fixture.sessions, "owner", &session.id, &source.id).unwrap()["status"],
            if terminal == TurnStatus::Completed {
                "cancelled"
            } else {
                "pending"
            }
        );
        assert!(matches!(
            fixture
                .service
                .admit(fixture.admission(&session, &format!("continuation:{}", source.id),))
                .await,
            Err(AdmissionError::Session(SessionError::Conflict(_)))
        ));
        assert_eq!(
            fixture
                .sessions
                .list_turns("owner", &session.id)
                .unwrap()
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn stop_after_completed_settlement_cancels_pending_admission_idempotently() {
    let fixture = Fixture::new();
    let session = fixture.create("stop-completed-continuation");
    let (source, _) = fixture
        .sessions
        .create_turn("owner", &session.id, "source", Some("original task"))
        .unwrap();
    fixture.register(&session, &source);
    let guard = fixture.service.plugin_mount_guard().await;
    guard
        .effect(&fixture.sessions, "owner", &session, &fixture.data, true)
        .unwrap();
    drop(guard);
    let saved = fixture
        .service
        .settle(
            fixture.admission(&session, "source"),
            source.id.clone(),
            TurnStatus::Completed,
            None,
        )
        .await
        .unwrap();
    assert_eq!(saved.status, TurnStatus::Completed);

    // Production has finished settlement and has not acquired admission's
    // gate yet. The stop request must close that exact boundary durably.
    let stop = || {
        fixture.service.stop_session(
            fixture.sessions.clone(),
            fixture.data.clone(),
            "owner".into(),
            session.clone(),
        )
    };
    assert!(stop().await.unwrap());
    assert!(!stop().await.unwrap());
    let request = format!("continuation:{}", source.id);
    assert!(matches!(
        fixture
            .service
            .admit(fixture.admission(&session, &request))
            .await,
        Err(AdmissionError::Session(SessionError::Conflict(_)))
    ));
    assert_eq!(
        fixture
            .sessions
            .list_turns("owner", &session.id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .sessions
            .get_turn("owner", &session.id, &source.id)
            .unwrap()
            .status,
        TurnStatus::Completed,
        "stop must preserve the settled source history"
    );
    assert_eq!(
        continuation_status(&fixture.sessions, "owner", &session.id, &source.id).unwrap()["status"],
        "cancelled"
    );
    assert_eq!(
        fixture
            .sessions
            .events("owner", &session.id, 0)
            .unwrap()
            .iter()
            .filter(|event| event.kind == "pilot.continuation.cancelled")
            .count(),
        1
    );
    assert!(matches!(
        fixture
            .service
            .ready_to_continue(
                fixture.admission(&session, &request),
                &source.id,
                "late authorization",
            )
            .await,
        Err(AdmissionError::Session(SessionError::Conflict(_)))
    ));
    let restarted = PilotService::default();
    assert!(matches!(
        restarted.admit(fixture.admission(&session, &request)).await,
        Err(AdmissionError::Session(SessionError::Conflict(_)))
    ));
}

#[tokio::test]
async fn stop_completed_source_without_request_prevents_late_authorization_after_restart() {
    let fixture = Fixture::new();
    let session = fixture.create("stop-before-late-oauth");
    let (source, _) = fixture
        .sessions
        .create_turn("owner", &session.id, "source", Some("original task"))
        .unwrap();
    fixture
        .sessions
        .finish_turn(
            "owner",
            &session.id,
            &source.id,
            TurnStatus::Completed,
            None,
        )
        .unwrap();
    assert!(
        continuation_request(&fixture.sessions, "owner", &session.id, &source.id)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .service
            .stop_session(
                fixture.sessions.clone(),
                fixture.data.clone(),
                "owner".into(),
                session.clone(),
            )
            .await
            .unwrap()
    );
    assert!(
        !fixture
            .service
            .stop_session(
                fixture.sessions.clone(),
                fixture.data.clone(),
                "owner".into(),
                session.clone(),
            )
            .await
            .unwrap()
    );
    let restarted = PilotService::default();
    assert!(matches!(
        restarted
            .ready_to_continue(
                fixture.admission(&session, "late-callback"),
                &source.id,
                "late authorization",
            )
            .await,
        Err(AdmissionError::Session(SessionError::Conflict(_)))
    ));
    assert_eq!(
        fixture
            .sessions
            .list_turns("owner", &session.id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .sessions
            .get_turn("owner", &session.id, &source.id)
            .unwrap()
            .status,
        TurnStatus::Completed
    );
    assert_eq!(
        continuation_status(&fixture.sessions, "owner", &session.id, &source.id).unwrap()["status"],
        "cancelled"
    );
    assert!(
        fixture
            .sessions
            .events("owner", &session.id, 0)
            .unwrap()
            .iter()
            .all(|event| event.kind != "pilot.continuation.requested")
    );
}

#[tokio::test]
async fn automatic_continuation_preserves_scope_and_replaces_a_superseded_business_prompt() {
    let fixture = Fixture::new();
    let session = fixture.create("configuration-not-business");
    let admission = fixture.admission(&session, "old-user-request");
    let original_scope = admission.root.clone();
    let original_prompt = admission.prompt.clone();
    let continuation = admission.for_continuation("continuation:source".into());
    assert_eq!(continuation.request_id, "continuation:source");
    assert_eq!(continuation.root, original_scope);
    assert_eq!(continuation.session.created_at, session.created_at);
    assert_eq!(continuation.tools.owner, "owner");
    assert_eq!(continuation.tools.session, session.id);
    assert_ne!(continuation.prompt, original_prompt);
    assert!(
        fixture
            .sessions
            .list_turns("owner", &session.id)
            .unwrap()
            .is_empty()
    );
}

use crate::test_support::Environment;

struct Fixture {
    _root: tempfile::TempDir,
    catalog: PathBuf,
    bwrap: PathBuf,
    data: PathBuf,
    workspace: PathBuf,
    sessions: SessionStore,
    service: PilotService,
}

impl Fixture {
    async fn stop(&self, session: &Session) -> Result<bool, SessionError> {
        self.service
            .stop_session(
                self.sessions.clone(),
                self.data.clone(),
                "owner".into(),
                session.clone(),
            )
            .await
    }

    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().to_path_buf();
        let data = catalog.join("state");
        let workspace = catalog.join("workspaces");
        std::fs::create_dir_all(data.join("platform")).unwrap();
        crate::test_network_plugin(&catalog);
        let sessions = SessionStore::open(data.join("platform/sessions.sqlite")).unwrap();
        Self {
            _root: root,
            catalog,
            bwrap: "bwrap".into(),
            data,
            workspace,
            sessions,
            service: PilotService::default(),
        }
    }

    fn configure(&self) -> Environment {
        // This fixture exercises admission and real HTTP MCP startup only. A
        // cancelled startup must never invoke Goose or the model transport.
        let binary = self.catalog.join("unstarted-goose");
        let bytes = b"Goose must not be started by this admission fixture";
        std::fs::write(&binary, bytes).unwrap();
        let mut environment = Environment(Vec::new());
        environment.set("ANCHOR_RUNNER_AGENT_RUNTIME", "goose");
        environment.set("ANCHOR_GOOSE_BINARY", &binary);
        environment.set(
            "ANCHOR_GOOSE_BINARY_SHA256",
            format!("{:x}", Sha256::digest(bytes)),
        );
        environment.set("ANCHOR_GOOSE_LOCAL_NETWORK", "0");
        environment.set("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK", "1");
        environment.set("ANCHOR_MODEL_DIRECT", "0");
        environment.set("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true");
        environment.set("ANCHOR_PILOT_PYTHON_ENABLED", "0");
        environment.remove("ANCHOR_MODEL_ALIASES");
        environment.remove("ANCHOR_MODEL_CONTEXT_WINDOW");

        environment.remove("ANCHOR_RUNNER_FAKE_PLUGINS");
        environment.remove("ANCHOR_PILOT_RUSTUP");
        environment.remove("ANCHOR_PILOT_PYTHON");

        crate::model_configuration::ModelConfiguration::from_env(&self.data)
            .unwrap()
            .save(serde_json::from_value(json!({"endpoint":"http://127.0.0.1:1/v1","wire_api":"chat","model":"admission-fixture","api_key":{"action":"set","value":"admission-fixture-only"}})).unwrap())
            .unwrap();
        environment
    }

    fn create(&self, id: &str) -> Session {
        self.sessions
            .create(
                "owner",
                CreateSession {
                    id: Some(id.into()),
                    ..Default::default()
                },
            )
            .unwrap()
    }

    fn register(&self, session: &Session, turn: &Turn) -> Arc<AtomicBool> {
        let cancellation = Arc::new(AtomicBool::new(false));
        self.service.active.lock().unwrap().insert(
            scope(&self.data, "owner", session),
            ActivePilot {
                turn: turn.id.clone(),
                cancellation: cancellation.clone(),
            },
        );
        cancellation
    }

    fn mount_slow_plugin(&self, session: &Session, endpoint: &str) {
        let plugin = self.catalog.join("plugins/slow");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            json!({"name":"Slow startup", "mcpServers":{"remote":{"type":"http","url":endpoint}}})
                .to_string(),
        )
        .unwrap();
        self.sessions
            .set_plugin_mount("owner", &session.id, "slow", true, "operator", "fixture")
            .unwrap();
    }

    fn admission(&self, session: &Session, request: &str) -> PilotAdmission {
        let tools = PilotTools {
            application: crate::application::RunApplication::new(
                self.data.clone(),
                self.catalog.clone(),
            )
            .with_host_configuration(Arc::new(crate::HostConfiguration {
                state_root: Some(self.data.clone()),
                bwrap: self.bwrap.clone(),
                ..Default::default()
            })),
            graph_name: "pilot".into(),
            library_root: self.catalog.clone(),
            data_root: self.data.clone(),
            workspace_root: self.workspace.clone(),
            sessions: self.sessions.clone(),
            owner: "owner".into(),
            session: session.id.clone(),
            turn: String::new(),
            context: None,
        };
        PilotAdmission {
            sessions: self.sessions.clone(),
            owner: "owner".into(),
            session: session.clone(),
            request_id: request.into(),
            prompt: Some("inspect".into()),
            attachments: Default::default(),
            input_source: None,
            root: scope(&self.data, "owner", session),
            tools,
        }
    }

    async fn admit(&self, session: &Session, request: &str) -> Turn {
        match tokio::time::timeout(
            Duration::from_secs(2),
            self.service.admit(self.admission(session, request)),
        )
        .await
        .expect("MCP startup held Pilot admission")
        {
            Ok(turn) => turn,
            Err(AdmissionError::Session(error)) => panic!("admission failed: {error}"),
            Err(AdmissionError::Provider) => panic!("fixture provider configuration was rejected"),
        }
    }

    async fn delete(&self, session: &Session) -> Result<(), SessionError> {
        tokio::time::timeout(
            Duration::from_secs(2),
            self.service.delete_session(
                self.sessions.clone(),
                self.data.clone(),
                self.workspace.clone(),
                self.catalog.clone(),
                "owner".into(),
                session.clone(),
            ),
        )
        .await
        .expect("MCP startup held Pilot deletion")
    }

    async fn terminal(&self, session: &Session, turn: &Turn) -> Turn {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                {
                    // A terminal fact and release of the execution happen under
                    // one gate, so DELETE can only observe the complete boundary.
                    let _gate = self.service.gate.lock().await;
                    let saved = self
                        .sessions
                        .get_turn("owner", &session.id, &turn.id)
                        .unwrap();
                    if saved.status != TurnStatus::Running {
                        assert!(
                            !self
                                .service
                                .active
                                .lock()
                                .unwrap()
                                .contains_key(&scope(&self.data, "owner", session))
                        );
                        return saved;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled MCP startup did not settle its Turn")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(active) = self.service.active.lock() {
            for active in active.values() {
                active.cancellation.store(true, Ordering::Release);
            }
        }
    }
}

struct HangingHttpMcp {
    endpoint: String,
    started: tokio::sync::mpsc::UnboundedReceiver<()>,
    task: tokio::task::JoinHandle<()>,
}

impl HangingHttpMcp {
    async fn start() -> Self {
        let (send, started) = tokio::sync::mpsc::unbounded_channel();
        let app = axum::Router::new().route(
            "/mcp",
            axum::routing::post(move || {
                let send = send.clone();
                async move {
                    send.send(()).unwrap();
                    std::future::pending::<axum::http::StatusCode>().await
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            endpoint,
            started,
            task,
        }
    }

    async fn entered(&mut self) {
        tokio::time::timeout(Duration::from_secs(2), self.started.recv())
            .await
            .expect("Pilot did not reach HTTP MCP startup")
            .expect("HTTP MCP fixture exited");
    }
}

impl Drop for HangingHttpMcp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn slow_mcp_startup_keeps_controls_and_other_sessions_available() {
    let _env = crate::PROCESS_ENV_LOCK.lock().await;
    let fixture = Fixture::new();
    let _configured = fixture.configure();
    let mut server = HangingHttpMcp::start().await;
    let first = fixture.create("first");
    fixture.mount_slow_plugin(&first, &server.endpoint);
    let first_turn = fixture.admit(&first, "first-request").await;
    let first_scope = scope(&fixture.data, "owner", &first);
    assert!(
        fixture
            .service
            .active
            .lock()
            .unwrap()
            .contains_key(&first_scope)
    );
    server.entered().await;

    // Replaying an accepted input remains idempotent while its MCP is waiting;
    // a distinct input cannot overwrite the cancellation token or reenter.
    assert_eq!(
        fixture.admit(&first, "first-request").await.id,
        first_turn.id
    );
    let conflict = tokio::time::timeout(
        Duration::from_secs(2),
        fixture.service.admit(fixture.admission(&first, "overlap")),
    )
    .await
    .expect("MCP startup blocked the reentry check");
    assert!(matches!(
        conflict,
        Err(AdmissionError::Session(SessionError::Conflict(_)))
    ));
    assert!(matches!(
        fixture.delete(&first).await,
        Err(SessionError::Conflict(_))
    ));
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            fixture.service.stop_session(
                fixture.sessions.clone(),
                fixture.data.clone(),
                "other-owner".into(),
                first.clone(),
            ),
        )
        .await
        .unwrap(),
        Err(SessionError::Missing)
    ));

    let idle = fixture.create("idle");
    fixture.delete(&idle).await.unwrap();
    let second = fixture.create("second");
    fixture.mount_slow_plugin(&second, &server.endpoint);
    let second_turn = fixture.admit(&second, "second-request").await;
    server.entered().await;
    assert!(
        tokio::time::timeout(Duration::from_secs(2), fixture.stop(&second),)
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(
        fixture.terminal(&second, &second_turn).await.status,
        TurnStatus::Stopped
    );
    fixture.delete(&second).await.unwrap();
    assert_eq!(
        fixture
            .sessions
            .get_turn("owner", &first.id, &first_turn.id)
            .unwrap()
            .status,
        TurnStatus::Running
    );

    // Mount edits made after acceptance cannot affect this Turn's pinned policy.
    fixture
        .sessions
        .set_plugin_mount("owner", &first.id, "slow", false, "operator", "next Turn")
        .unwrap();
    let snapshot = crate::plugin_mounts::read_snapshot(
        &crate::plugin_mounts::snapshot_path(&fixture.data, &first.id, &first_turn.id),
        &first.id,
        &first_turn.id,
    )
    .unwrap();
    assert_eq!(snapshot.mounts.get("slow"), Some(&true));
    assert!(
        tokio::time::timeout(Duration::from_secs(2), fixture.stop(&first))
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(
        fixture.terminal(&first, &first_turn).await.status,
        TurnStatus::Stopped
    );
    fixture.delete(&first).await.unwrap();

    let recreated = fixture.create("first");
    assert_ne!(first.created_at, recreated.created_at);
    fixture.mount_slow_plugin(&recreated, &server.endpoint);
    let current_turn = fixture.admit(&recreated, "new-identity").await;
    server.entered().await;
    assert!(
        matches!(fixture.stop(&first).await, Err(SessionError::Conflict(reason)) if reason.contains("identity"))
    );
    assert!(
        matches!(fixture.delete(&first).await, Err(SessionError::Conflict(reason)) if reason.contains("identity"))
    );
    let stale = fixture
        .service
        .admit(fixture.admission(&first, "stale-identity"))
        .await;
    assert!(
        matches!(stale, Err(AdmissionError::Session(SessionError::Conflict(reason))) if reason.contains("identity"))
    );
    assert_eq!(
        fixture
            .sessions
            .get_turn("owner", &recreated.id, &current_turn.id)
            .unwrap()
            .status,
        TurnStatus::Running
    );
    assert!(fixture.stop(&recreated).await.unwrap());
    assert_eq!(
        fixture.terminal(&recreated, &current_turn).await.status,
        TurnStatus::Stopped
    );
    fixture.delete(&recreated).await.unwrap();
    assert!(fixture.service.active.lock().unwrap().is_empty());
}

#[tokio::test]
async fn mount_snapshot_failure_settles_the_turn_before_registering_execution() {
    let _env = crate::PROCESS_ENV_LOCK.lock().await;
    let fixture = Fixture::new();
    let _configured = fixture.configure();
    let session = fixture.create("snapshot-failure");
    std::fs::write(fixture.data.join("plugin-mounts"), "not a directory").unwrap();
    let result = fixture
        .service
        .admit(fixture.admission(&session, "failure"))
        .await;
    assert!(
        matches!(result, Err(AdmissionError::Session(SessionError::Storage(reason))) if reason.contains("snapshot"))
    );
    let turns = fixture.sessions.list_turns("owner", &session.id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, TurnStatus::Interrupted);
    assert!(fixture.service.active.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stopping_an_accepted_turn_before_startup_skips_the_sandbox() {
    let _env = crate::PROCESS_ENV_LOCK.lock().await;
    let mut fixture = Fixture::new();
    let _configured = fixture.configure();
    fixture.bwrap = fixture.catalog.join("missing-bwrap");
    let session = fixture.create("stop-before-startup");
    let turn = fixture.admit(&session, "stop").await;
    // Admission spawned the executor without yielding after active registration;
    // in this current-thread test the stop happens before its first poll.
    assert!(fixture.stop(&session).await.unwrap());
    assert_eq!(
        fixture.terminal(&session, &turn).await.status,
        TurnStatus::Stopped
    );
    assert!(
        !crate::pilot_resources::session_workspace(&fixture.workspace, "owner", &session.id)
            .exists()
    );
    fixture.delete(&session).await.unwrap();
}

#[tokio::test]
async fn background_context_failure_settles_and_releases_the_accepted_turn() {
    let _env = crate::PROCESS_ENV_LOCK.lock().await;
    let mut fixture = Fixture::new();
    let _configured = fixture.configure();
    fixture.bwrap = fixture.catalog.join("missing-bwrap");
    let session = fixture.create("context-failure");
    let turn = fixture.admit(&session, "fail").await;
    let saved = fixture.terminal(&session, &turn).await;
    assert_eq!(saved.status, TurnStatus::Interrupted);
    assert!(saved.error.is_some());
    assert_eq!(fixture.admit(&session, "fail").await.id, turn.id);
    fixture.delete(&session).await.unwrap();
}

fn process_identity(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm may contain spaces or parentheses. Field 22 (starttime), relative to
    // field 3 after the final ') ', identifies this exact process across PID reuse.
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}

fn processes_with_marker(marker: &str) -> Vec<(u32, String, bool)> {
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let command = std::fs::read(entry.path().join("cmdline")).ok()?;
            if !command
                .windows(marker.len())
                .any(|window| window == marker.as_bytes())
            {
                return None;
            }
            let status = std::fs::read_to_string(entry.path().join("status")).ok()?;
            let parent = status
                .lines()
                .find(|line| line.starts_with("PPid:"))?
                .split_whitespace()
                .nth(1)?
                .parse::<u32>()
                .ok()?;
            Some((pid, process_identity(pid)?, parent == std::process::id()))
        })
        .collect()
}

#[tokio::test]
async fn connected_stdio_mcp_is_reaped_before_a_failed_pilot_turn_can_be_deleted() {
    let _env = crate::PROCESS_ENV_LOCK.lock().await;
    let fixture = Fixture::new();
    let mut configured = fixture.configure();
    configured.set("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh");
    let session = fixture.create("cleanup");
    let plugin = fixture.catalog.join("plugins/healthy");
    std::fs::create_dir_all(&plugin).unwrap();
    let marker = format!(
        "pilot-mcp-cleanup-{}",
        fixture.catalog.file_name().unwrap().to_string_lossy()
    );
    // /usr/bin/python3 follows /etc/alternatives on this host, a path the
    // ordinary Plugin sandbox intentionally does not mount.
    let python = std::fs::canonicalize("/usr/bin/python3").unwrap();
    std::fs::write(plugin.join("plugin.json"), json!({
        "name":"Healthy stdio fixture",
        "mcpServers":{"local":{"command":"sh","args":["-c",format!("exec {} /plugins/healthy/server.py {marker} 2>/workspace/fixture-stderr", python.display())]}}
    }).to_string()).unwrap();
    std::fs::write(plugin.join("server.py"), r#"
import json, os, sys, time
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    if request["method"] == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "pilot-cleanup", "version": "1"}}
    elif request["method"] == "tools/list":
        with open("/workspace/connected", "w") as marker:
            marker.write("MCP tools/list reached")
        while not os.path.exists("/workspace/release-startup"):
            time.sleep(0.01)
        result = {"tools": []}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
# Reaching EOF alone must not masquerade as process exit; Host shutdown reaps it.
time.sleep(600)
"#).unwrap();
    fixture
        .sessions
        .set_plugin_mount("owner", &session.id, "healthy", true, "operator", "fixture")
        .unwrap();
    let turn = fixture.admit(&session, "cleanup-request").await;
    let workspace =
        crate::pilot_resources::session_workspace(&fixture.workspace, "owner", &session.id);
    let initialized = tokio::time::timeout(Duration::from_secs(2), async {
        while !workspace.join("connected").is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        initialized.is_ok(),
        "stdio MCP fixture did not initialize: {}",
        std::fs::read_to_string(workspace.join("fixture-stderr")).unwrap_or_default()
    );
    let processes = processes_with_marker(&marker);
    assert!(
        !processes.is_empty(),
        "MCP process was not observable while tools/list was blocked"
    );
    assert!(
        processes.iter().any(|(_, _, direct_child)| *direct_child),
        "Host-owned MCP child was not observed"
    );
    assert!(matches!(
        fixture.delete(&session).await,
        Err(SessionError::Conflict(_))
    ));
    std::fs::write(workspace.join("release-startup"), "go").unwrap();
    // The configuration-only Goose file cannot execute. This proves the error
    // path still closes already-connected MCPs before releasing active ownership.
    assert_eq!(
        fixture.terminal(&session, &turn).await.status,
        TurnStatus::Interrupted
    );
    for (pid, identity, direct_child) in &processes {
        if *direct_child {
            assert_ne!(
                process_identity(*pid).as_deref(),
                Some(identity.as_str()),
                "Host-owned MCP child {pid} was not reaped before its Turn settled"
            );
        }
    }
    // The outer child is already reaped. As in the Sandbox regressions, allow
    // bounded kernel convergence for namespace descendants receiving SIGKILL;
    // a zombie has exited, and cannot run Plugin code or keep writable handles.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let settled = processes.iter().all(|(pid, identity, _)| {
                if process_identity(*pid).as_deref() != Some(identity.as_str()) {
                    return true;
                }
                std::fs::read_to_string(format!("/proc/{pid}/status")).is_ok_and(|status| {
                    status.lines().any(|line| {
                        line.starts_with("State:") && line.split_whitespace().nth(1) == Some("Z")
                    })
                })
            });
            if settled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("MCP namespace descendants did not settle after the outer child was reaped");
    fixture.delete(&session).await.unwrap();
    assert!(!workspace.exists());
}

#[tokio::test]
async fn stale_admission_and_delete_cannot_reach_a_recreated_session() {
    let fixture = Fixture::new();
    let old = fixture.create("reused");
    fixture.sessions.delete("owner", "reused").unwrap();
    let current = fixture.create("reused");
    assert_ne!(old.created_at, current.created_at);
    crate::pilot_resources::register(&fixture.data, &fixture.workspace, "owner", &current).unwrap();
    let task = crate::pilot_resources::session_workspace(&fixture.workspace, "owner", "reused");
    std::fs::create_dir_all(&task).unwrap();
    std::fs::write(task.join("keep"), "new Session").unwrap();
    let rejected = fixture.delete(&old).await;
    assert!(matches!(rejected,Err(SessionError::Conflict(reason)) if reason.contains("identity")));
    let rejected = fixture
        .service
        .admit(fixture.admission(&old, "stale"))
        .await;
    assert!(
        matches!(rejected,Err(AdmissionError::Session(SessionError::Conflict(reason))) if reason.contains("identity"))
    );
    assert_eq!(
        fixture.sessions.get("owner", "reused").unwrap().created_at,
        current.created_at
    );
    assert!(
        fixture
            .sessions
            .list_turns("owner", "reused")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(task.join("keep")).unwrap(),
        "new Session"
    );
    assert!(fixture.service.active.lock().unwrap().is_empty());
}
