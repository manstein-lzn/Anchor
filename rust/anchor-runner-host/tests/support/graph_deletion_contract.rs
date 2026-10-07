use super::*;

#[test]
fn conditional_graph_delete_restarts_rejects_stale_target_and_cleans_real_op_run() {
    let provider = Provider::new(std::iter::empty::<(&str, Vec<Reply>)>());
    let host = Host::new(&json!({
        "entry":"idle","ops":{"idle":{"run":"true"}},
        "nodes":[{"id":"idle","op":"idle"}],"edges":[]
    }));
    let server = host.serve(&provider);
    let mut definition = json!({
        "objective":"conditional deletion fixture","entry":"receipt",
        "ops":{"receipt":{"run":"sh -c 'printf conditional-delete-fixture > receipt.txt'"}},
        "nodes":[{"id":"receipt","op":"receipt"}],"edges":[]
    });
    let (status, result) = server.request(
        "POST",
        "/graphs",
        Some(&json!({"name":"delete-target","definition":definition})),
    );
    assert_eq!(status, 201, "{result}");
    let (status, before) = server.request("GET", "/graphs/delete-target/delete-precondition", None);
    assert_eq!(status, 200, "{before}");
    let stale = format!("\"{}\"", before["precondition"].as_str().unwrap());
    definition["objective"] = json!("changed target");
    let (status, result) = server.request(
        "PUT",
        "/graphs/delete-target",
        Some(&json!({"definition":definition})),
    );
    assert_eq!(status, 200, "{result}");
    let (status, result) =
        server.request_with_headers("DELETE", "/graphs/delete-target", &[("If-Match", &stale)]);
    assert_eq!(status, 409, "{result}");
    let (status, accepted) =
        server.request("POST", "/trigger", Some(&json!({"graph":"delete-target"})));
    assert_eq!(status, 202, "{accepted}");
    let run = accepted["run"].as_str().unwrap();
    server.wait_status(run, "completed");
    let record = host.record_for(run);
    let workspaces = host.workspace_files(run, "receipt.txt");
    assert_eq!(workspaces, vec![b"conditional-delete-fixture".to_vec()]);
    let artifacts_before = fixture::artifact_evidence(&host.root.path().join("state/artifacts"));
    let receipt = artifacts_before
        .iter()
        .find(|(path, _)| path.ends_with("/files/receipt.txt"))
        .map(|(_, contents)| contents)
        .unwrap();
    assert_eq!(receipt["text"], "conditional-delete-fixture");
    assert_eq!(
        receipt["sha256"],
        format!("{:x}", Sha256::digest(b"conditional-delete-fixture"))
    );
    let (status, current) =
        server.request("GET", "/graphs/delete-target/delete-precondition", None);
    assert_eq!(status, 200, "{current}");
    let expected = format!("\"{}\"", current["precondition"].as_str().unwrap());
    drop(server);
    let server = host.serve(&provider);
    let (status, reopened) =
        server.request("GET", "/graphs/delete-target/delete-precondition", None);
    assert_eq!(status, 200, "{reopened}");
    assert_eq!(reopened, current);
    let (status, result) = server.request_with_headers(
        "DELETE",
        "/graphs/delete-target",
        &[("If-Match", &expected)],
    );
    assert_eq!(status, 204, "{result}");
    assert!(!host.root.path().join("delete-target").exists());
    assert!(
        !host
            .root
            .path()
            .join("state/runs")
            .join(format!("{run}.json"))
            .exists()
    );
    assert!(host.workspace_files(run, "receipt.txt").is_empty());
    let (status, _) = server.request("GET", "/graphs/fixture", None);
    assert_eq!(status, 200);
    assert!(provider.requests().is_empty());
    provider.assert_consumed();
    fixture::evidence_rejection(
        "conditional-graph-deletion",
        &host,
        &provider,
        json!({"status":204}),
        json!({"stale_refused":true,"reopened_precondition":current,
            "receipt_before_deletion":String::from_utf8(workspaces[0].clone()).unwrap(),
            "removed_run":record,
            "artifacts_before_deletion":artifacts_before,
            "terminal_run_and_workspace_removed":true,"other_graph_preserved":true}),
    );
}
