use super::*;
use std::collections::{BTreeSet, VecDeque};

impl HostArtifacts {
    /// BFS chooses the nearest immutable version of each node. All reachable
    /// links are still validated, including shadowed historical ancestors.
    pub(super) fn expanded_snapshots(
        &self,
        commits: &[CommitRef],
        expected: Option<(&str, &str)>,
    ) -> Result<Vec<(PathBuf, Manifest)>, GraphError> {
        let mut queue = VecDeque::new();
        let mut roots = BTreeSet::new();
        for commit in commits {
            if !roots.insert(commit.id.clone()) {
                return Err(corrupt("duplicate artifact input"));
            }
            queue.push_back((commit.clone(), 0_usize));
        }
        let mut run_graph = expected.map(|(run, graph)| (run.to_owned(), graph.to_owned()));
        let mut loaded = BTreeMap::<String, (CommitRef, usize)>::new();
        let mut selected = BTreeMap::<String, (String, usize)>::new();
        let mut adjacency = BTreeMap::<String, Vec<String>>::new();
        let mut snapshots = Vec::new();
        while let Some((commit, depth)) = queue.pop_front() {
            if let Some((existing, _)) = loaded.get(&commit.id) {
                if existing != &commit {
                    return Err(corrupt("conflicting artifact reference identity"));
                }
                continue;
            }
            let (path, manifest) = self.read_snapshot(&commit)?;
            let binding = run_graph.get_or_insert_with(|| {
                (
                    manifest.key.run_id.clone(),
                    manifest.key.graph_digest.clone(),
                )
            });
            if manifest.key.run_id != binding.0 || manifest.key.graph_digest != binding.1 {
                return Err(corrupt("artifact belongs to another Run or Graph"));
            }
            let parents = manifest
                .context
                .as_ref()
                .map(|context| context.input_commits.as_slice())
                .unwrap_or_default();
            adjacency.insert(
                commit.id.clone(),
                parents.iter().map(|parent| parent.id.clone()).collect(),
            );
            for parent in parents {
                queue.push_back((parent.clone(), depth + 1));
            }
            loaded.insert(commit.id.clone(), (commit.clone(), depth));
            match selected.get(&commit.node_id) {
                Some((id, selected_depth)) if *selected_depth == depth && id != &commit.id => {
                    return Err(corrupt(
                        "ambiguous artifact versions at the same ancestry depth",
                    ));
                }
                Some(_) => (),
                None => {
                    selected.insert(commit.node_id.clone(), (commit.id.clone(), depth));
                    snapshots.push((path, manifest));
                }
            }
        }
        // Detect cycles without recursive traversal or a recursion-depth limit.
        let mut indegree = adjacency
            .keys()
            .map(|id| (id.clone(), 0_usize))
            .collect::<BTreeMap<_, _>>();
        for parents in adjacency.values() {
            for parent in parents {
                *indegree
                    .get_mut(parent)
                    .ok_or_else(|| corrupt("missing artifact parent"))? += 1;
            }
        }
        let mut ready = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(id, _)| id.clone())
            .collect::<VecDeque<_>>();
        let mut visited = 0;
        while let Some(id) = ready.pop_front() {
            visited += 1;
            for parent in &adjacency[&id] {
                let degree = indegree.get_mut(parent).unwrap();
                *degree -= 1;
                if *degree == 0 {
                    ready.push_back(parent.clone());
                }
            }
        }
        if visited != adjacency.len() {
            return Err(corrupt("artifact parent links contain a cycle"));
        }
        Ok(snapshots)
    }
}
