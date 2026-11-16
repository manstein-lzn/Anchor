use super::*;
use crate::{
    application::scheduling::{GraphScheduleReference, ScheduleManagementPort},
    task_delegation::TaskGraphOwnership,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn owned_graph(application: &RunApplication, name: &str, source: &Path, recorded_name: &str) {
    let definition = definition(name);
    write_graph_bundle(&application.graph_bundle_path(name), &definition).unwrap();
    let owned = TaskGraphOwnership {
        graph: recorded_name.into(),
        source: metadata::graph_identity(source).unwrap(),
        scope: "private-scope-marker".into(),
        request: crate::application::run_access::admission_request(
            "private-request-marker".into(),
            name,
            &definition,
            &None,
        ),
    };
    crate::assistant::save_immutable(
        &crate::application::graph_tasks::task_graph_ownership_path(application, name).unwrap(),
        &owned,
    )
    .unwrap();
}

#[tokio::test]
async fn catalog_task_source_requires_stored_ownership_and_a_current_catalog_identity() {
    let (_root, application) = fixture();
    let source = application.graph_bundle_path("fixture");
    owned_graph(&application, "generated", &source, "generated");
    owned_graph(&application, "mismatched", &source, "another-graph");
    owned_graph(
        &application,
        "unknown-source",
        &application.catalog_root.join("not-in-catalog"),
        "unknown-source",
    );
    owned_graph(&application, "unreadable", &source, "unreadable");
    let unreadable =
        crate::application::graph_tasks::task_graph_ownership_path(&application, "unreadable")
            .unwrap();
    std::fs::write(unreadable, b"not json").unwrap();
    write_graph_bundle(
        &application.graph_bundle_path("task-pretend"),
        &definition("A hand-authored graph"),
    )
    .unwrap();
    let listed = application.list_graphs().await.unwrap();
    let graphs = listed["graphs"].as_array().unwrap();
    assert_eq!(graphs.len(), 6);
    for graph in graphs {
        if graph["graph"] == "generated" {
            assert_eq!(graph["task_source"], "fixture");
        } else {
            assert!(graph.get("task_source").is_none(), "{graph}");
        }
    }
    for private in ["private-scope-marker", "private-request-marker"] {
        assert!(!listed.to_string().contains(private));
    }
    assert!(!listed.to_string().contains(source.to_str().unwrap()));
    std::fs::remove_dir_all(source).unwrap();
    let missing = application.list_graphs().await.unwrap();
    assert_eq!(missing["graphs"].as_array().unwrap().len(), 6);
    assert!(
        missing["graphs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|graph| graph.get("task_source").is_none())
    );
}

struct Plans {
    unavailable: AtomicBool,
}

impl ScheduleManagementPort for Plans {
    fn graph_references(&self) -> Result<Vec<GraphScheduleReference>, ApplicationError> {
        if self.unavailable.load(Ordering::Acquire) {
            return Err(ApplicationError::Storage("schedule unavailable".into()));
        }
        Ok([(true, true), (true, false), (false, true)]
            .into_iter()
            .map(|(once, enabled)| GraphScheduleReference {
                graph: "fixture".into(),
                request_id: "private-schedule-request".into(),
                once,
                enabled,
            })
            .collect())
    }
    fn list(&self) -> Result<Value, ApplicationError> {
        panic!("catalog reads typed references, not private production inputs")
    }
    fn create(&self, _: &str, _: &Value, _: &Value) -> Result<Value, ApplicationError> {
        unreachable!()
    }
    fn update(
        &self,
        _: &str,
        _: &str,
        _: &Value,
        _: &Value,
        _: bool,
    ) -> Result<Value, ApplicationError> {
        unreachable!()
    }
    fn delete(&self, _: &str) -> Result<Value, ApplicationError> {
        unreachable!()
    }
}

#[tokio::test]
async fn catalog_schedule_summary_uses_schedule_facts_without_hiding_graphs_on_read_failure() {
    let (_root, application) = fixture();
    let plans = Arc::new(Plans {
        unavailable: AtomicBool::new(false),
    });
    let application = application.with_schedule_management(plans.clone());
    write_graph_bundle(
        &application.graph_bundle_path("unscheduled"),
        &definition("No schedule"),
    )
    .unwrap();
    let listed = application.list_graphs().await.unwrap();
    assert_eq!(
        listed["graphs"][0]["schedule_summary"],
        json!({"total":3,"enabled":2,"recurring":1})
    );
    assert!(listed["graphs"][1].get("schedule_summary").is_none());
    let relations = application.graph_relations("owner").await.unwrap();
    assert_eq!(relations["graphs"][0]["schedules"], 3);
    plans.unavailable.store(true, Ordering::Release);
    let unavailable = application.list_graphs().await.unwrap();
    assert_eq!(unavailable["graphs"].as_array().unwrap().len(), 2);
    assert!(
        unavailable["graphs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|graph| graph.get("schedule_summary").is_none())
    );
}
