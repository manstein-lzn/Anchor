use super::*;
use anchor_kernel::{Effect, MemoryStore, Resource};
use serde_json::json;

fn invocation() -> Invocation {
    Invocation {
        run_id: "rust-1".into(),
        graph_digest: "g".into(),
        node_id: "worker".into(),
        invocation: 1,
    }
}

fn configuration() -> AgentConfiguration {
    AgentConfiguration {
        version: VERSION,
        graph_digest: "g".into(),
        node_id: "worker".into(),
        model: "flash".into(),
        model_binding: "binding".into(),
        runtime_sha256: "goose".into(),
        instructions_sha256: text_digest("do it"),
        plugin_instructions_sha256: text_digest(""),
        plugins: vec![
            PluginIdentity {
                id: "b".into(),
                digest: "2".into(),
                authorization: "x".into(),
            },
            PluginIdentity {
                id: "a".into(),
                digest: "1".into(),
                authorization: "y".into(),
            },
        ],
        routes: vec!["verify".into(), "done".into()],
        host_sha256: String::new(),
        tools_sha256: String::new(),
        envelope_sha256: String::new(),
    }
}

fn tool_call_fact(ok: bool) -> Fact {
    Fact::ToolCall {
        tool: "anchor_run".into(),
        tool_call_id: Some("call_1".into()),
        model_request: 2,
        arguments_shape: arguments_shape(&json!({"command": ["cat", "x"]})),
        path: None,
        program: Some("cat".into()),
        shell_program: None,
        command: None,
        started_at_ms: Some(10),
        ended_at_ms: Some(25),
        ok,
        failure: (!ok).then_some(Failure::NotAuthorized),
    }
}

fn tool_call(ok: bool) -> Observation {
    Observation {
        version: VERSION,
        invocation: invocation(),
        facts: vec![tool_call_fact(ok)],
    }
}

fn kernel() -> Kernel<MemoryStore> {
    let mut kernel = Kernel::new(MemoryStore::default(), Box::new(RecorderOnly)).unwrap();
    install(&mut kernel).unwrap();
    kernel
        .create(Resource::new(invocation().trace(), OBSERVATION, Value::Null).configured("cfg"))
        .unwrap();
    kernel
}

fn record(kernel: &Kernel<MemoryStore>, principal: &Principal, content: Value) -> Reason {
    let seen = kernel.load(&invocation().trace()).unwrap().unwrap().version;
    kernel
        .submit(
            principal,
            0,
            Submission::new(invocation().trace(), seen, Action::Write).content(content),
        )
        .unwrap()
        .reason
}

#[test]
fn the_recorder_records_observations_of_the_named_invocation() {
    let kernel = kernel();
    let content = serde_json::to_value(tool_call(false)).unwrap();
    assert_eq!(
        record(&kernel, &recorder("cfg"), content.clone()),
        Reason::Ok
    );
    let history = kernel.history(&invocation().trace()).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].verdict.effect, Effect::Accepted);
    assert_eq!(history[0].stamp.config(), Some("cfg"));
    let read: Observation = serde_json::from_value(history[0].submission.content.clone()).unwrap();
    assert_eq!(read, tool_call(false));
    assert_eq!(content["facts"][0]["kind"], "tool_call");
    // The trace stays empty; only its history grows.
    assert_eq!(
        kernel.load(&invocation().trace()).unwrap().unwrap().state,
        Value::Null
    );
}

#[test]
fn only_the_recorder_may_record_and_only_well_formed_facts_of_its_trace() {
    let kernel = kernel();
    let content = serde_json::to_value(tool_call(true)).unwrap();
    let agent = Principal::new(RECORDER_ID, PrincipalKind::Agent);
    assert_eq!(
        record(&kernel, &agent, content.clone()),
        Reason::Unauthorized
    );
    let other = Principal::new("anchor/host", PrincipalKind::Code);
    assert_eq!(
        record(&kernel, &other, content.clone()),
        Reason::Unauthorized
    );

    let mut elsewhere = tool_call(true);
    elsewhere.invocation.invocation = 2;
    assert_eq!(
        record(
            &kernel,
            &recorder("cfg"),
            serde_json::to_value(elsewhere).unwrap()
        ),
        Reason::ContentInvalid
    );
    let mut future = content.clone();
    future["version"] = json!(VERSION + 1);
    assert_eq!(
        record(&kernel, &recorder("cfg"), future),
        Reason::ContentInvalid
    );
    assert_eq!(
        record(&kernel, &recorder("cfg"), json!({"kind": "tool_call"})),
        Reason::ContentInvalid
    );
    let mut empty = tool_call(true);
    empty.facts.clear();
    assert_eq!(
        record(
            &kernel,
            &recorder("cfg"),
            serde_json::to_value(empty).unwrap()
        ),
        Reason::ContentInvalid
    );
}

#[test]
fn configuration_manifests_are_content_addressed_and_immutable() {
    let mut kernel = Kernel::new(MemoryStore::default(), Box::new(AllowAll)).unwrap();
    install(&mut kernel).unwrap();
    let manifest = configuration();
    let digest = manifest.digest();
    kernel
        .create(
            Resource::new(
                configuration_id(&digest),
                CONFIGURATION,
                serde_json::to_value(&manifest).unwrap(),
            )
            .configured(&digest),
        )
        .unwrap();
    let verdict = kernel
        .submit(
            &Principal::new("owner", PrincipalKind::Human),
            0,
            Submission::new(configuration_id(&digest), 0, Action::Write).content(json!({})),
        )
        .unwrap();
    assert_eq!(verdict.reason, Reason::Unauthorized);

    // Order of Plugins and routes does not change the identity; content does.
    let mut reordered = configuration();
    reordered.plugins.reverse();
    reordered.routes.reverse();
    assert_eq!(reordered.digest(), digest);
    let mut changed = configuration();
    changed.model = "pro".into();
    assert_ne!(changed.digest(), digest);
}

struct AllowAll;
impl Authority for AllowAll {
    fn permits(&self, _: &Principal, _: Action, _: &str) -> bool {
        true
    }
}

#[test]
fn argument_shapes_ignore_values_but_not_structure() {
    let a = arguments_shape(&json!({"command": ["cat", "a"], "cwd": "/x"}));
    let b = arguments_shape(&json!({"cwd": "/y", "command": ["ls"]}));
    let c = arguments_shape(&json!({"command": ["cat", "a"]}));
    let d = arguments_shape(&json!({"command": "cat a"}));
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_ne!(c, d);
}

#[test]
fn failures_are_classified_from_the_host_refusal_wording() {
    let cases = [
        (
            "tool failed: anchor_run not_executed: command `find` is not authorized by the host sandbox policy",
            Failure::NotAuthorized,
        ),
        (
            "anchor_run not_executed: invalid arguments: absolute read paths must belong to an authorized read-only input",
            Failure::NotAuthorized,
        ),
        (
            "anchor_run not_executed: invalid arguments: missing field `command`",
            Failure::InvalidArguments,
        ),
        (
            "final_result.observed_receipt must match an anchor_receipt",
            Failure::CompletionRejected,
        ),
        ("Anchor invocation was cancelled", Failure::Cancelled),
        (
            "tool failed: anchor_run not_executed: busy",
            Failure::NotExecuted,
        ),
        ("exit status 2", Failure::ToolError),
    ];
    for (error, class) in cases {
        assert_eq!(classify_failure(error), class, "{error}");
    }
}

#[test]
fn tool_calls_without_shell_program_keep_their_json() {
    let old = json!({
        "kind": "tool_call", "tool": "anchor_run",
        "tool_call_id": "call_1", "model_request": 1,
        "arguments_shape": "shape", "program": "sh",
        "started_at_ms": 1, "ended_at_ms": 2,
        "ok": true, "failure": null
    });
    let fact: Fact = serde_json::from_value(old.clone()).unwrap();
    assert!(matches!(
        &fact,
        Fact::ToolCall {
            shell_program: None,
            ..
        }
    ));
    let written = serde_json::to_value(&fact).unwrap();
    assert!(written.get("shell_program").is_none());
    assert_eq!(written, old);
    // Adding a workspace observation kind must not change older trace records.
    let old_observation = json!({
        "version": VERSION,
        "invocation": invocation(),
        "facts": [old],
    });
    let observation: Observation = serde_json::from_value(old_observation.clone()).unwrap();
    assert_eq!(serde_json::to_value(observation).unwrap(), old_observation);
}

#[test]
fn every_fact_kind_round_trips_with_its_tag() {
    let facts = [
        Fact::Lifecycle {
            started_at_ms: 1,
            ended_at_ms: 9,
            outcome: Outcome::Interrupted,
            model_turns: 2,
            tool_calls: 3,
            tool_calls_dropped: 0,
            resumed: true,
        },
        Fact::ModelTurn {
            index: 1,
            message_id: Some("m".into()),
            observed_at_ms: Some(5),
            elapsed_ms: Some(4),
            time_to_first_token_ms: None,
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            cache_read_tokens: Some(8),
            cache_write_tokens: None,
            cost_usd: Some(0.001),
            compaction: false,
        },
        tool_call_fact(true),
        Fact::WorkspaceFingerprint {
            phase: FingerprintPhase::Start,
            head: Some("a".repeat(40)),
            dirty: Some(false),
            files: BTreeMap::from([("Cargo.lock".into(), text_digest("lock"))]),
        },
        Fact::WorkspaceFingerprint {
            phase: FingerprintPhase::End,
            head: None,
            dirty: None,
            files: BTreeMap::new(),
        },
    ];
    let observation = Observation {
        version: VERSION,
        invocation: invocation(),
        facts: facts.to_vec(),
    };
    let value = serde_json::to_value(&observation).unwrap();
    let kinds: Vec<_> = value["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fact| fact["kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "lifecycle",
            "model_turn",
            "tool_call",
            "workspace_fingerprint",
            "workspace_fingerprint",
        ]
    );
    assert_eq!(value["facts"][3]["phase"], "start");
    assert_eq!(value["facts"][4]["phase"], "end");
    assert!(value["facts"][4].get("head").is_none());
    assert!(value["facts"][4].get("dirty").is_none());
    assert_eq!(
        serde_json::from_value::<Observation>(value).unwrap(),
        observation
    );
}

fn grade(run: &str) -> Grade {
    Grade {
        version: VERSION,
        run_id: run.into(),
        task: "ledger".into(),
        task_version: 1,
        checks: vec![
            Check {
                name: "unpaid".into(),
                passed: true,
            },
            Check {
                name: "total".into(),
                passed: false,
            },
        ],
    }
}

#[test]
fn the_grader_records_named_checks_of_the_run_it_names() {
    let mut kernel = Kernel::new(MemoryStore::default(), Box::new(GraderOnly)).unwrap();
    install_grades(&mut kernel).unwrap();
    kernel
        .create(Resource::new(grade_id("r1"), GRADE, Value::Null))
        .unwrap();
    let submit = |principal: &Principal, content: Value| {
        let seen = kernel.load(&grade_id("r1")).unwrap().unwrap().version;
        kernel
            .submit(
                principal,
                0,
                Submission::new(grade_id("r1"), seen, Action::Write).content(content),
            )
            .unwrap()
            .reason
    };
    let ok = serde_json::to_value(grade("r1")).unwrap();
    assert_eq!(submit(&grader("g1"), ok.clone()), Reason::Ok);
    // Graded again by a changed grader: another record, same history.
    assert_eq!(submit(&grader("g2"), ok.clone()), Reason::Ok);
    let history = kernel.history(&grade_id("r1")).unwrap();
    assert_eq!(
        history.iter().map(|r| r.stamp.config()).collect::<Vec<_>>(),
        vec![Some("g1"), Some("g2")]
    );
    assert_eq!(history[0].verdict.verification, Verification::Mechanical);

    let model = Principal::new(GRADER_ID, PrincipalKind::Agent);
    assert_eq!(submit(&model, ok.clone()), Reason::Unauthorized);
    let elsewhere = serde_json::to_value(grade("r2")).unwrap();
    assert_eq!(submit(&grader("g1"), elsewhere), Reason::ContentInvalid);
    let mut twice = grade("r1");
    twice.checks[1].name = "unpaid".into();
    assert_eq!(
        submit(&grader("g1"), serde_json::to_value(twice).unwrap()),
        Reason::ContentInvalid
    );
    let mut none = grade("r1");
    none.checks.clear();
    assert_eq!(
        submit(&grader("g1"), serde_json::to_value(none).unwrap()),
        Reason::ContentInvalid
    );
}

#[test]
fn a_person_may_grade_where_the_authority_lets_them() {
    let mut kernel = Kernel::new(MemoryStore::default(), Box::new(AllowAll)).unwrap();
    install_grades(&mut kernel).unwrap();
    kernel
        .create(Resource::new(grade_id("r1"), GRADE, Value::Null))
        .unwrap();
    let verdict = kernel
        .submit(
            &Principal::new("owner", PrincipalKind::Human),
            0,
            Submission::new(grade_id("r1"), 0, Action::Write)
                .content(serde_json::to_value(grade("r1")).unwrap()),
        )
        .unwrap();
    assert!(verdict.is_accepted());
    assert_eq!(grade("r1").score(), 0.5);
    assert!(!grade("r1").passed());
}

#[test]
fn manifests_without_harness_identity_keep_their_digest() {
    // A manifest recorded before the fields existed reads back and hashes
    // the same: the fields are left out when empty.
    let old = configuration();
    let json = serde_json::to_value(&old).unwrap();
    assert!(
        json.get("host_sha256").is_none()
            && json.get("tools_sha256").is_none()
            && json.get("envelope_sha256").is_none()
    );
    assert_eq!(
        old.digest(),
        "1df310017104f6f90b3eca87a5193d2c2427162b5813f31246b8c2ca29218f4d"
    );
    let read: AgentConfiguration = serde_json::from_value(json).unwrap();
    assert_eq!(read.digest(), old.digest());
    let mut envelope = configuration();
    envelope.envelope_sha256 = "e".into();
    assert_ne!(envelope.digest(), old.digest());
    let mut harness = configuration();
    harness.host_sha256 = "h".into();
    assert_ne!(harness.digest(), old.digest());
    let mut tools = configuration();
    tools.tools_sha256 = "t".into();
    assert_ne!(tools.digest(), harness.digest());
}

#[test]
fn tool_digests_ignore_order_but_not_content() {
    let a = json!({"type": "object", "properties": {"path": {"type": "string"}, "limit": {"type": "integer"}}});
    let b = json!({"properties": {"limit": {"type": "integer"}, "path": {"type": "string"}}, "type": "object"});
    let run = json!({"type": "object"});
    let one = tools_digest([("read", "Read a file", &a), ("run", "Run", &run)]);
    let two = tools_digest([("run", "Run", &run), ("read", "Read a file", &b)]);
    assert_eq!(one, two);
    assert_ne!(one, tools_digest([("read", "Read a file", &a)]));
    assert_ne!(
        one,
        tools_digest([("read", "Read one file", &a), ("run", "Run", &run)])
    );
}
