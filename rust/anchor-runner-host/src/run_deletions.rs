//! Immutable tombstones for conversation Runs an operator deleted on purpose.
//!
//! A conversation Run names the Run whose Session it took over
//! (`conversation.previous_run`). Deleting a Run in the middle of that chain
//! would otherwise leave a later Run pointing at a missing predecessor, which
//! every lineage walk treats as retained corruption and refuses to touch. The
//! tombstone is the durable host fact that turns that gap into an explained
//! end-of-lineage, so a walk stops there instead of failing closed.
//!
//! A tombstone also carries the predecessor the deleted Run itself named, so the
//! walk can carry on past it: the remaining history stays one path instead of
//! splitting into orphaned segments, and links can be deleted in any order.
//! Tombstones written before that field existed carry no bridge and end the
//! chain they are found in, which is exactly what they meant at the time.
//!
//! Two boundaries keep that tolerance from hiding real damage:
//!
//! - A missing predecessor without a matching tombstone still fails closed.
//! - A tombstone only explains the conversation it was deleted from: the same
//!   Graph bundle identity, Session and reply node. The Graph digest is
//!   deliberately not part of that identity, because a new assistant instance
//!   after a Graph update still names the previous revision's Run as its
//!   predecessor (its workspace scene is simply not inherited).
//!
//! Tombstones are never removed. After the Run files are gone they are inert
//! unless some surviving Run still names the deleted Run, and keeping them makes
//! a partially completed cascade deletion replayable instead of corrupt.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Directory of durable deletion tombstones, relative to the Host state root.
const DELETION_DIR: &str = "run-deletions";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunDeletion {
    format: u32,
    run_id: String,
    graph: String,
    session: String,
    reply_node: String,
    /// The predecessor the deleted Run named, so a lineage walk continues past
    /// this link. Absent means the chain it is found in ends here.
    #[serde(default)]
    previous_run: Option<String>,
    deleted_at: String,
}

impl RunDeletion {
    pub(crate) fn new(
        run_id: &str,
        graph: &str,
        session: &str,
        reply_node: &str,
        previous_run: Option<&str>,
    ) -> Self {
        Self {
            format: 1,
            run_id: run_id.into(),
            graph: graph.into(),
            session: session.into(),
            reply_node: reply_node.into(),
            previous_run: previous_run.map(str::to_owned),
            deleted_at: chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
                .to_rfc3339(),
        }
    }

    /// The link this deleted Run held in its chain, when it recorded one.
    pub(crate) fn previous_run(&self) -> Option<&str> {
        self.previous_run.as_deref()
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    /// True only for the conversation this Run was deleted from.
    pub(crate) fn explains(&self, graph: &str, session: &str, reply_node: &str) -> bool {
        self.graph == graph && self.session == session && self.reply_node == reply_node
    }

    fn same_deletion(&self, other: &Self) -> bool {
        self.run_id == other.run_id
            && self.graph == other.graph
            && self.session == other.session
            && self.reply_node == other.reply_node
            // A tombstone written before bridges existed records no predecessor;
            // re-recording the same deletion with the link it held is the same
            // deletion, and the recorded file is left as it is.
            && (self.previous_run.is_none() || self.previous_run == other.previous_run)
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

fn path(root: &Path, run_id: &str) -> Result<PathBuf, String> {
    if !valid_id(run_id) {
        return Err(format!("unsafe Run id `{run_id}`"));
    }
    Ok(root.join(DELETION_DIR).join(format!("{run_id}.json")))
}

pub(crate) fn load(root: &Path, run_id: &str) -> Result<Option<RunDeletion>, String> {
    let path = path(root, run_id)?;
    let Some(deletion) = crate::assistant::read_json::<RunDeletion>(&path)? else {
        return Ok(None);
    };
    if deletion.format != 1 || deletion.run_id != run_id {
        return Err("Run deletion tombstone identity changed".into());
    }
    Ok(Some(deletion))
}

/// Every tombstone in this state root, oldest Run id first.
pub(crate) fn list(root: &Path) -> Result<Vec<RunDeletion>, String> {
    let directory = root.join(DELETION_DIR);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut deletions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(run_id) = name.strip_suffix(".json") else {
            continue;
        };
        if run_id.is_empty() || run_id.starts_with('.') {
            continue;
        }
        if let Some(deletion) = load(root, run_id)? {
            deletions.push(deletion);
        }
    }
    deletions.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    Ok(deletions)
}

/// Record a deletion before its Run files are removed, so a Host that dies in
/// the middle of the cleanup still leaves an explained gap.
///
/// Replaying the same deletion succeeds: the recorded timestamp belongs to the
/// first attempt and two tombstones for the same conversation Run may not
/// disagree about the identity they explain.
pub(crate) fn save(root: &Path, deletion: &RunDeletion) -> Result<(), String> {
    let path = path(root, &deletion.run_id)?;
    if let Some(existing) = load(root, &deletion.run_id)? {
        return if existing.same_deletion(deletion) {
            Ok(())
        } else {
            Err("Run deletion tombstone conflicts with the recorded deletion".into())
        };
    }
    let parent = path.parent().ok_or("deletion tombstone has no parent")?;
    crate::create_durable_directory(parent).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(deletion).map_err(|error| error.to_string())?;
    let temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if load(root, &deletion.run_id)?
                .is_none_or(|existing| !existing.same_deletion(deletion))
            {
                return Err("Run deletion tombstone conflicts with a concurrent deletion".into());
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    std::fs::File::open(parent)
        .map_err(|error| error.to_string())?
        .sync_all()
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tombstones_are_immutable_and_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let deletion = RunDeletion::new(
            "channel-1",
            "/graphs/fixture",
            "alice",
            "work",
            Some("channel-0"),
        );
        save(root.path(), &deletion).unwrap();
        // The same deletion may be replayed with a different clock reading.
        save(
            root.path(),
            &RunDeletion::new(
                "channel-1",
                "/graphs/fixture",
                "alice",
                "work",
                Some("channel-0"),
            ),
        )
        .unwrap();
        assert_eq!(load(root.path(), "channel-1").unwrap(), Some(deletion));
        assert_eq!(
            load(root.path(), "channel-1")
                .unwrap()
                .unwrap()
                .previous_run(),
            Some("channel-0")
        );
        let conflict = save(
            root.path(),
            &RunDeletion::new(
                "channel-1",
                "/graphs/fixture",
                "bob",
                "work",
                Some("channel-0"),
            ),
        );
        assert!(conflict.unwrap_err().contains("conflicts"));
        assert!(
            save(
                root.path(),
                &RunDeletion::new("../escape", "g", "s", "n", None)
            )
            .is_err()
        );
    }

    /// Tombstones written before the bridge existed must still load, end their
    /// chain, and accept a replay of the same deletion.
    #[test]
    fn a_tombstone_without_a_bridge_still_loads_and_replays() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("run-deletions")).unwrap();
        std::fs::write(
            root.path().join("run-deletions/channel-1.json"),
            br#"{"format":1,"run_id":"channel-1","graph":"/graphs/fixture",
                 "session":"alice","reply_node":"work","deleted_at":"2026-10-09T00:00:00+00:00"}"#,
        )
        .unwrap();
        let recorded = load(root.path(), "channel-1").unwrap().unwrap();
        assert_eq!(recorded.previous_run(), None);
        save(
            root.path(),
            &RunDeletion::new(
                "channel-1",
                "/graphs/fixture",
                "alice",
                "work",
                Some("channel-0"),
            ),
        )
        .unwrap();
        // The recorded file is left untouched: it explains an end-of-lineage.
        assert_eq!(
            load(root.path(), "channel-1")
                .unwrap()
                .unwrap()
                .previous_run(),
            None
        );
    }

    #[test]
    fn a_tombstone_only_explains_its_own_conversation() {
        let deletion = RunDeletion::new(
            "channel-1",
            "/graphs/fixture",
            "alice",
            "work",
            Some("channel-0"),
        );
        assert!(deletion.explains("/graphs/fixture", "alice", "work"));
        assert!(!deletion.explains("/graphs/fixture", "bob", "work"));
        assert!(!deletion.explains("/graphs/other", "alice", "work"));
        assert!(!deletion.explains("/graphs/fixture", "alice", "reply"));
    }
}
