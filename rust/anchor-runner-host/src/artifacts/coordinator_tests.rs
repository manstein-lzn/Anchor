use super::*;
use anchor_runtime::{Cancellation, graph::*};
use serde_json::json;
use std::sync::{Arc, atomic::AtomicBool};

struct Nodes<'a>(&'a HostArtifacts);
impl NodeExecutionPort for Nodes<'_> {
    fn capabilities(&self) -> NodeExecutionCapabilities {
        NodeExecutionCapabilities {
            agent: false,
            op_run: true,
            host_operations: false,
            exact_provider_request_budget: true,
        }
    }
    fn completion_fact<'a>(
        &'a self,
        _: &'a InvocationKey,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
        Box::pin(async { Ok(CompletionFact::NotStarted) })
    }
    fn execute<'a>(
        &'a self,
        request: NodeExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>> {
        Box::pin(async move {
            assert!(!["fanout", "join"].contains(&request.key.node_id.as_str()));
            let mounts = self.0.input_mounts(
                &request.input_commits,
                &request.key.run_id,
                &request.key.graph_digest,
            )?;
            let workspace = self.0.workspace_path(&request.key)?;
            fs::create_dir_all(&workspace)?;
            if request.key.node_id == "downstream" {
                for node in ["producer", "left1", "left2", "right"] {
                    let input = mounts
                        .iter()
                        .find(|input| input.destination == Path::new("/in").join(node))
                        .expect("every exact ancestor file must be mounted");
                    assert_eq!(
                        fs::read_to_string(input.source.join(format!("{node}.txt")))?,
                        node
                    );
                }
                let join = mounts
                    .iter()
                    .find(|input| input.destination == Path::new("/in/join"))
                    .unwrap();
                let output: Value =
                    serde_json::from_slice(&fs::read(join.source.join("join.json"))?).unwrap();
                assert_eq!(output["branches"].as_array().unwrap().len(), 2);
                for branch in output["branches"].as_array().unwrap() {
                    for node in branch["nodes"].as_array().unwrap() {
                        assert!(node.get("files").is_none());
                    }
                }
            }
            fs::write(
                workspace.join(format!("{}.txt", request.key.node_id)),
                &request.key.node_id,
            )?;
            Ok(NodeExecutionOutcome::Completed(NodeCompletion {
                submission: request.key.node_id.clone(),
                route: None,
                model_requests: 0,
                output: json!({"node":request.key.node_id}),
            }))
        })
    }
}
struct Control;
impl RunControl for Control {
    fn pause_requested(&self) -> bool {
        false
    }
    fn stop_requested(&self) -> bool {
        false
    }
    fn cancellation(&self) -> Cancellation {
        Arc::new(AtomicBool::new(false))
    }
}

#[tokio::test]
async fn shared_coordinator_pins_every_cursor_input_in_node_and_control_manifests() {
    let temp = tempfile::tempdir().unwrap();
    let artifacts = HostArtifacts::new(
        temp.path().join("artifacts"),
        temp.path().join("workspaces"),
    );
    let snapshot = GraphSnapshot::admit(json!({
        "objective":"parallel files", "entry":"producer",
        "ops":{"work":{"run":"true"},"fork":{"fanout":{"join":"join"}},"gather":{"join":{}}},
        "nodes":[{"id":"producer","op":"work"},{"id":"fanout","op":"fork"},{"id":"left1","op":"work"},{"id":"left2","op":"work"},{"id":"right","op":"work"},{"id":"join","op":"gather"},{"id":"downstream","op":"work"}],
        "edges":[{"from":"producer","to":"fanout"},{"from":"fanout","to":"left1"},{"from":"left1","to":"left2"},{"from":"left2","to":"join"},{"from":"fanout","to":"right"},{"from":"right","to":"join"},{"from":"join","to":"downstream"}]
    })).unwrap();
    let record = GraphRunRecord::create_with_id(snapshot, json!({}), "coordinator-test").unwrap();
    let store = FileRunStore::new(temp.path().join("runs"));
    let nodes = Nodes(&artifacts);
    let completed = GraphRunner::new(&store, &artifacts, &nodes, &Control)
        .run(record)
        .await
        .unwrap();
    assert_eq!(
        completed.status,
        RunStatus::Completed,
        "{:?}",
        completed.error
    );
    for (node, parents, kind) in [
        ("producer", vec![], ArtifactKind::Node),
        ("fanout", vec!["producer"], ArtifactKind::Fanout),
        ("left1", vec!["fanout"], ArtifactKind::Node),
        ("left2", vec!["left1"], ArtifactKind::Node),
        ("right", vec!["fanout"], ArtifactKind::Node),
        ("join", vec!["left1", "left2", "right"], ArtifactKind::Join),
        ("downstream", vec!["join"], ArtifactKind::Node),
    ] {
        let result = &completed.results[node][0];
        let manifest = artifacts.load_snapshot(&result.commit).unwrap().1;
        let context = manifest.context.unwrap();
        assert_eq!(context.kind, kind);
        assert_eq!(context.input_commits.len(), parents.len());
        for parent in parents {
            assert!(
                context
                    .input_commits
                    .contains(&completed.results[parent][0].commit)
            );
        }
    }
}
