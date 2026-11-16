//! Static calls and result subscriptions are separate catalog relationships.
use super::*;
use std::collections::BTreeSet;

impl RunApplication {
    pub(crate) async fn graph_relations(&self, owner: &str) -> Result<Value, GraphManagementError> {
        // Publication can temporarily replace a bundle; do not project that
        // window as a lasting subscription authorization change.
        let _catalog = self.graph_catalog_mutation_guard().await;
        let catalog = self.list_graphs().await?;
        let schedules = self.schedule_management()?.graph_references()?;
        let mut names = BTreeSet::new();
        let mut graphs = Vec::new();
        let mut calls = Vec::new();
        for graph in catalog["graphs"].as_array().into_iter().flatten() {
            let Some(name) = graph["graph"].as_str() else {
                continue;
            };
            names.insert(name.to_owned());
            match self.load_graph(name) {
                Ok((_, bundle)) => {
                    for node in &bundle.snapshot.nodes {
                        let Some(op_name) = node.op.as_deref() else {
                            continue;
                        };
                        let Some(call) = bundle
                            .snapshot
                            .ops
                            .get(op_name)
                            .and_then(|op| op.get("call"))
                            .and_then(Value::as_object)
                        else {
                            continue;
                        };
                        let (Some(target), Some(mode)) = (
                            call.get("graph").and_then(Value::as_str),
                            call.get("mode").and_then(Value::as_str),
                        ) else {
                            continue;
                        };
                        calls.push(json!({
                            "graph": name,
                            "node": node.id,
                            "op": op_name,
                            "target": target,
                            "mode": mode,
                        }));
                    }
                }
                Err(GraphManagementError::Missing(_)) => {}
                Err(failure) => return Err(failure),
            }
            let mut relation = json!({
                "graph": name,
                "schedules": schedules.iter().filter(|plan| plan.graph == name).count(),
            });
            for field in ["objective", "task_source"] {
                if let Some(value) = graph.get(field) {
                    relation[field] = value.clone();
                }
            }
            graphs.push(relation);
        }
        let subscriptions = self
            .graph_subscription_relations(owner)?
            .into_iter()
            .filter(|relation| {
                ["graph", "target"].iter().all(|field| {
                    relation[*field]
                        .as_str()
                        .is_some_and(|name| names.contains(name))
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"graphs":graphs,"calls":calls,"subscriptions":subscriptions}))
    }
}
