use super::*;

const SOURCE: &str = "shared-morning-source";
const PDF: &str = "%PDF-one-shared-scheduled-report";
const DESCRIPTION: &str = "Shared morning fixture: one production, private subscribers";
const INTRODUCTION: &str = "Here is the shared scheduled report.";
const CASE: &str = "tests/goose_persistent_assistant/task_inputs/subscriptions.rs";

fn subscribe_steps(schedule: &str, instruction: &str, reply: &'static str) -> Vec<Step> {
    vec![
        Step::tool("anchor_tools", json!({"plugin":"graph-tasks"})),
        Step::tool("anchor_tool_detail", json!({"name":"graph_tasks_schedule_discover"}))
            .after("graph_tasks_schedule_discover"),
        Step::tool(
            "anchor_tools_call",
            json!({"name":"graph_tasks_schedule_discover","arguments":{}}),
        )
        .after("graph_tasks_schedule_discover"),
        Step::tool("anchor_tool_detail", json!({"name":"graph_tasks_subscribe"}))
            .after(DESCRIPTION)
            .expect_request_contains(schedule),
        Step::tool(
            "anchor_tools_call",
            json!({"name":"graph_tasks_subscribe","arguments":{"schedule":schedule,"instruction":instruction}}),
        )
        .after("graph_tasks_subscribe"),
        Step::tool("anchor_tool_detail", json!({"name":"graph_tasks_subscriptions"}))
            .after("effective_at"),
        Step::tool(
            "anchor_tools_call",
            json!({"name":"graph_tasks_subscriptions","arguments":{}}),
        )
        .after("graph_tasks_subscriptions"),
        finish(reply).after("effective_at"),
    ]
}

fn read_result() -> Step {
    command(&format!(
        "set -eu; set -- /in/tasks/*/1-report.pdf; test \"$#\" -eq 1; test \"$(cat \"$1\")\" = {PDF}; test ! -e /in/tasks/*/internal.txt; if printf changed > \"$1\" 2>/dev/null; then exit 9; fi; cp \"$1\" delivered.pdf; printf shared-product-read"
    ))
    .expect_request_contains("task_result")
}

fn send_result(result: &TaskResult, reply: &'static str) -> Vec<Step> {
    vec![
        Step::tool("anchor_tools", json!({"name":"wecom_send_file"})).after("shared-product-read"),
        Step::tool("anchor_tool_detail", json!({"name":"wecom_send_file"}))
            .after("wecom_send_file"),
        Step::tool(
            "anchor_tools_call",
            json!({"name":"wecom_send_file","arguments":{
                "path":format!("/in/tasks/{}/{}",result.id,result.files[0].name),
                "content":INTRODUCTION}}),
        )
        .after("wecom_send_file"),
        Step::tool("task_result_resolve", json!({"action":"delivered"})).after("file_status"),
        finish(reply).after("resolved"),
    ]
}

fn schedule_update(server: &HttpHost, id: &str, at: &str, enabled: bool) -> Value {
    let (status, saved) = server.request(
        "PUT",
        &format!("/schedules/{id}"),
        Some(&json!({"graph":SOURCE,"rule":{"type":"once","at":at},"input":{},"enabled":enabled})),
    );
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["schedule"]["id"], id);
    assert_eq!(saved["schedule"]["enabled"], enabled);
    saved
}

#[test]
#[ignore = "requires pinned real Goose and Bubblewrap; channel transport is strictly local"]
fn one_schedule_produces_once_for_two_native_private_subscribers() {
    let provider = Provider::new(
        "goose-persistent-shared-schedule-subscriptions",
        vec![
            command("printf alice-initial-ready"),
            finish("alice-ready").after("alice-initial-ready"),
            command("printf bob-initial-ready"),
            finish("bob-ready").after("bob-initial-ready"),
        ],
    );
    let mut definition = graph();
    definition["nodes"][1]["plugins"] = json!([
        "wecom",{"id":"graph-tasks","graph_tasks":{"graphs":[]}},
        {"id":"network","network":"denied"}
    ]);
    let mut host = task_host(&definition);
    let gateway = TaskNoticeGateway::bind_for(&mut host, &[ALICE, BOB]);
    let package = host.base.root.path().join(SOURCE);
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("graph.json"),
        json!({"objective":"one shared scheduled publication","entry":"prepare",
            "ops":{
                "prepare":{"run":"sh -c 'printf private-working-file > internal.txt; printf private-submission-marker'","writes":["internal.txt"]},
                "produce":{"run":format!("sh -c 'printf %s {PDF} > report.pdf'"),"writes":["report.pdf"]}},
            "nodes":[{"id":"prepare","op":"prepare"},{"id":"produce","op":"produce"}],
            "edges":[{"from":"prepare","to":"produce"}]}).to_string(),
    )
    .unwrap();
    fs::write(
        package.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[]}).to_string(),
    )
    .unwrap();
    let server = host.serve(&provider);
    let (status, approved) = server.request(
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":definition})),
    );
    assert_eq!(status, 200, "{approved}");
    let alice = submit(
        &server,
        &event(ALICE, "shared-alice-initial", "alice-private-initial"),
        "alice-ready",
    );
    let bob = submit(
        &server,
        &event(BOB, "shared-bob-initial", "bob-private-initial"),
        "bob-ready",
    );
    let alice_run = alice["run"].as_str().unwrap();
    let bob_run = bob["run"].as_str().unwrap();
    let alice_session = alice["session"].as_str().unwrap();
    let bob_session = bob["session"].as_str().unwrap();
    assert_ne!(alice_session, bob_session);
    waiting(&host, alice_run, 1);
    waiting(&host, bob_run, 1);
    let far_future = (chrono::Utc::now() + chrono::Duration::hours(1))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    let (status, created) = server.request(
        "POST",
        "/schedules",
        Some(&json!({"graph":SOURCE,"rule":{"type":"once","at":far_future},"input":{}})),
    );
    assert_eq!(status, 201, "{created}");
    let schedule = created["schedule"]["id"].as_str().unwrap();
    schedule_update(&server, schedule, &far_future, false);
    let (status, shared) = server.request(
        "PUT", &format!("/schedules/{schedule}/sharing"),
        Some(&json!({"description":DESCRIPTION,"sessions":[alice_session,bob_session],"enabled":true})),
    );
    assert_eq!(status, 200, "{shared}");
    definition["nodes"][1]["plugins"][1]["graph_tasks"] = json!({"schedules":[schedule]});
    let (status, approved) = server.request(
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":definition})),
    );
    assert_eq!(status, 200, "{approved}");
    assert_eq!(runs(&host).len(), 2);
    for (user, event_id, instruction, expected, run, session) in [
        (
            ALICE,
            "shared-alice-subscribe",
            "alice-subscription-delivery: deliver the exact final PDF here",
            "alice-subscribed",
            alice_run,
            alice_session,
        ),
        (
            BOB,
            "shared-bob-subscribe",
            "bob-subscription-delivery: deliver the exact final PDF here",
            "bob-subscribed",
            bob_run,
            bob_session,
        ),
    ] {
        provider.append(subscribe_steps(schedule, instruction, expected));
        let subscribed = submit(
            &server,
            &event(
                user,
                event_id,
                "discover and subscribe to the shared morning schedule",
            ),
            expected,
        );
        assert_eq!(subscribed["run"], run);
        assert_eq!(subscribed["session"], session);
        waiting(&host, run, 2);
        assert_eq!(
            runs(&host).len(),
            2,
            "subscribing admitted a production Run"
        );
    }
    let (status, subscriptions) = server.request("GET", "/schedule-subscriptions", None);
    assert_eq!(status, 200, "{subscriptions}");
    let subscriptions = subscriptions["subscriptions"].as_array().unwrap().clone();
    assert_eq!(subscriptions.len(), 2);
    for subscription in &subscriptions {
        assert_eq!(subscription["schedule"], schedule);
        assert_eq!(subscription["active"], true);
        assert_eq!(subscription["authorized"], true);
        assert!(
            subscription["pending_results"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    action(&server, bob_run, "pause");
    let bob_paused = server.wait_status(bob_run, "paused");
    let gate = Gate::new();
    provider.append(vec![read_result().gated(&gate)]);
    let at = (chrono::Utc::now() + chrono::Duration::seconds(5))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    for subscription in &subscriptions {
        let parse = |value: &str| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").unwrap()
        };
        assert!(parse(&at) > parse(subscription["effective_at"].as_str().unwrap()));
    }
    schedule_update(&server, schedule, &at, true);
    gate.wait_entered();
    let sessions =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    wait_until(
        "both private subscription results are durably queued",
        || {
            [alice_session, bob_session].into_iter().all(|session| {
                sessions
                    .pending_task_results("local", session)
                    .unwrap()
                    .len()
                    == 1
            })
        },
    );
    let alice_pending = sessions
        .pending_task_results("local", alice_session)
        .unwrap();
    let bob_pending = sessions.pending_task_results("local", bob_session).unwrap();
    assert_eq!(alice_pending.len(), 1);
    assert_eq!(bob_pending.len(), 1);
    let alice_result = &alice_pending[0];
    let bob_result = &bob_pending[0];
    assert_ne!(alice_result.id, bob_result.id);
    assert_eq!(alice_result.source_run, bob_result.source_run);
    assert_eq!(alice_result.files, bob_result.files);
    assert_eq!(alice_result.files.len(), 1);
    assert_eq!(alice_result.outcome, "done");
    assert_eq!(bob_result.outcome, "done");
    for result in [alice_result, bob_result] {
        assert_eq!(result.content["subscription"]["schedule"], schedule);
        assert_eq!(result.content["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(result.content["nodes"][0]["node"], "produce");
        assert!(
            !result
                .content
                .to_string()
                .contains("private-submission-marker")
        );
    }
    let producer = alice_result.source_run.as_deref().unwrap();
    let producer_record = host.record(producer);
    assert_eq!(runs(&host).len(), 3);
    for node in ["prepare", "produce"] {
        assert_eq!(
            producer_record["results"][node].as_array().unwrap().len(),
            1
        );
    }
    let metadata = fixture::read_json(
        host.base
            .state_root()
            .join("run-metadata")
            .join(format!("{producer}.json")),
    );
    assert_eq!(metadata["schedule"], schedule);
    assert_eq!(metadata["scheduled_at"], at);
    assert_eq!(
        metadata["schedule_publication"],
        shared["sharing"]["revision"]
    );
    assert!(
        metadata["task"].is_null(),
        "producer origin was rewritten: {metadata}"
    );
    assert_eq!(producer_record["input"], json!({}));
    let file = &alice_result.files[0];
    let artifact = host
        .base
        .state_root()
        .join("artifacts")
        .join(&file.artifact_id);
    assert_eq!(
        fixture::artifact_file(&artifact, &file.path),
        PDF.as_bytes()
    );
    provider.append(send_result(alice_result, "alice-shared-file-delivered"));
    gate.open();
    let alice_done = waiting(&host, alice_run, 3);
    assert!(
        sessions
            .pending_task_results("local", alice_session)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        sessions.pending_task_results("local", bob_session).unwrap(),
        bob_pending
    );
    assert_eq!(gateway.requests().len(), 2);
    assert!(
        gateway
            .requests()
            .iter()
            .all(|request| request["userid"] == ALICE)
    );
    assert_eq!(
        host.record(bob_run)["results"]["reply"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    provider.append(vec![
        read_result().expect_request_contains("bob-subscription-delivery"),
    ]);
    provider.append(send_result(bob_result, "bob-shared-file-delivered"));
    action(&server, bob_run, "resume");
    let bob_done = waiting(&host, bob_run, 3);
    let requests = gateway.requests();
    assert_eq!(requests.len(), 4);
    for (offset, user) in [(0, ALICE), (2, BOB)] {
        assert_eq!(requests[offset]["userid"], user);
        assert_eq!(requests[offset]["operation"], "send_file");
        assert_eq!(requests[offset]["name"], "report.pdf");
        assert_eq!(
            STANDARD
                .decode(requests[offset]["data_base64"].as_str().unwrap())
                .unwrap(),
            PDF.as_bytes()
        );
        assert_eq!(requests[offset + 1]["userid"], user);
        assert_eq!(requests[offset + 1]["content"], INTRODUCTION);
    }
    assert_ne!(requests[0]["request_id"], requests[2]["request_id"]);
    assert_ne!(requests[1]["request_id"], requests[3]["request_id"]);
    for (session, run, result, done) in [
        (alice_session, alice_run, alice_result, &alice_done),
        (bob_session, bob_run, bob_result, &bob_done),
    ] {
        assert!(
            sessions
                .pending_task_results("local", session)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            sessions
                .task_result("local", session, &result.id)
                .unwrap()
                .as_ref(),
            Some(result)
        );
        assert_eq!(
            done["results"]["reply"][2]["completion"]["output"]["task_result_handled"],
            true
        );
        assert_eq!(
            fs::read(workspace(&host, run).join("delivered.pdf")).unwrap(),
            PDF.as_bytes()
        );
        assert_eq!(
            host.native_fact(run, "assistant", 1)["session_id"],
            host.native_fact(run, "assistant", 3)["session_id"]
        );
        let events = sessions.events("local", session, 0).unwrap();
        for kind in [
            "task.result.queued",
            "task.result.claimed",
            "task.result.handled",
        ] {
            assert_eq!(
                events.iter().filter(|event| event.kind == kind).count(),
                1,
                "{kind}"
            );
        }
    }
    let (status, plans) = server.request("GET", "/schedules", None);
    assert_eq!(status, 200, "{plans}");
    let production_plans = plans["schedules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|plan| plan["graph"] == SOURCE)
        .collect::<Vec<_>>();
    assert_eq!(production_plans.len(), 1);
    assert_eq!(production_plans[0]["id"], schedule);
    assert_eq!(production_plans[0]["enabled"], false);
    provider.assert_consumed();
    let idle_requests = provider.requests().len();
    idle(&provider, idle_requests);
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(runs(&host).len(), 3);
    assert_eq!(gateway.requests().len(), 4);
    idle(&provider, idle_requests);
    let alice_subscription = subscriptions
        .iter()
        .find(|subscription| subscription["session"] == alice_session)
        .unwrap();
    let (status, unsubscribed) = server.request(
        "DELETE",
        &format!(
            "/schedule-subscriptions/{}",
            alice_subscription["id"].as_str().unwrap()
        ),
        None,
    );
    assert_eq!(status, 200, "{unsubscribed}");
    assert_eq!(unsubscribed["active"], false);
    let (status, remaining) = server.request("GET", "/schedule-subscriptions", None);
    assert_eq!(status, 200, "{remaining}");
    let bob_subscription = remaining["subscriptions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|subscription| subscription["session"] == bob_session)
        .unwrap();
    assert_eq!(bob_subscription["active"], true);
    assert_eq!(bob_subscription["authorized"], true);
    let alice_stopped = stop(&server, &provider, alice_run);
    let bob_stopped = stop(&server, &provider, bob_run);
    evidence(
        &host,
        &provider,
        alice_run,
        json!({
            "case_source":CASE,"one_schedule_one_production_run_two_native_subscribers":true,
            "consumer_execute_permission":false,"subscribe_does_not_start_run":true,
            "native_discover_subscribe_list":true,"source_run":producer,"source_record":producer_record,
            "source_metadata":metadata,"scheduled_at_strictly_after_subscriptions":true,
            "subscriptions":subscriptions,"results":[alice_result,bob_result],"shared_artifact":file,
            "artifact_sha256":format!("{:x}",Sha256::digest(PDF.as_bytes())),
            "independent_pending_and_handled":true,"bob_paused":bob_paused,"bob_completed":bob_done,
            "alice_session":alice_session,"bob_session":bob_session,"gateway_requests":requests,
            "unsubscribe_is_session_scoped":remaining,"stopped":[alice_stopped,bob_stopped],
            "limits":{"real_model_requests":0,"real_wecom_requests":0,"gateway_transport":"local stub, not real gateway ledger","restart_tested":false,"feishu_tested":false}
        }),
    );
}
