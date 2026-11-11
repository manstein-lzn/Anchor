//! Session-level Plugin mounts, and the per-turn snapshot that pins them.
//!
//! A Plugin gets mounted for one of **two** reasons, and they are different facts:
//!
//! * a **Host-provided initial policy** saved at Session creation, or a human's
//!   later click in the Session view recorded by `session.plugin_mount.changed`;
//! * the **Graph's declaration** — `required: true`, or an optional one whose
//!   `default`/absence of a switch turns it on. Nothing was clicked and no event
//!   exists for these; they are the deployment's own default, which is why
//!   `ResolvedMounts` keeps the two apart.
//!
//! The Graph's declaration is never an authorization by itself: `required` is
//! honored only for the deployment's pilot Graph (see `node_host`), so an
//! assistant that authors another Graph cannot grant itself a Plugin.
//!
//! A Turn snapshots the Session's switches once, when the Turn starts, and every
//! node call in that Turn uses that snapshot. Otherwise "the tool set changed in
//! the middle of a Turn" would be a second, hidden semantics. The snapshot is
//! written **once** and never overwritten, so a retried Turn reuses it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anchor_platform_session::SessionStore;
use serde::{Deserialize, Serialize};

/// The read-only view of Session Plugin switches a node call needs.
///
/// The resolver does not open the Session database: whoever owns the Session
/// store injects one implementation when it builds the resolver. That keeps
/// "what did this human mount" a platform fact, while the runner-host only asks
/// the question.
pub(crate) trait PluginMountSource: Send + Sync {
    /// This Session's switches, by Plugin id. A Session that does not exist is
    /// "no override" rather than an error.
    fn session_mounts(&self, session: &str) -> Result<BTreeMap<String, bool>, String>;

    fn session_policy(&self, session: &str) -> Result<(BTreeMap<String, bool>, bool), String> {
        self.session_mounts(session).map(|mounts| (mounts, true))
    }
}

/// The production source: the platform Session store, read by Session id.
///
/// A deployment that has no Session store yet (no Session was ever created, or a
/// host under test) is not a failure: it simply mounted nothing.
pub(crate) struct SessionMounts {
    database: PathBuf,
    /// Opened when the Session database already existed. A store the platform
    /// creates later is opened on the call that first needs it.
    store: Option<SessionStore>,
}

impl SessionMounts {
    pub(crate) fn open(state_root: &Path) -> Result<Self, String> {
        let database = state_root.join("platform/sessions.sqlite");
        let store = if database.is_file() {
            Some(
                SessionStore::open(&database)
                    .map_err(|error| format!("Session store unavailable: {error}"))?,
            )
        } else {
            None
        };
        Ok(Self { database, store })
    }
}

impl PluginMountSource for SessionMounts {
    fn session_mounts(&self, session: &str) -> Result<BTreeMap<String, bool>, String> {
        self.session_policy(session).map(|(mounts, _)| mounts)
    }

    fn session_policy(&self, session: &str) -> Result<(BTreeMap<String, bool>, bool), String> {
        if !self.database.is_file() {
            // No Session store at all: no Session mounted anything.
            return Ok((BTreeMap::new(), true));
        }
        let switches = match &self.store {
            Some(store) => store.plugin_policy(session),
            // The platform created its store after this host was built.
            None => {
                SessionStore::open(&self.database).and_then(|store| store.plugin_policy(session))
            }
        };
        switches.map_err(|error| format!("Session Plugin mounts unavailable: {error}"))
    }
}

/// Directory of per-turn mount snapshots, relative to the Host state root.
const SNAPSHOT_DIR: &str = "plugin-mounts";

/// One Turn's frozen mount switches.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MountSnapshot {
    pub(crate) format: u32,
    pub(crate) session: String,
    pub(crate) turn: String,
    /// What the Session's switches were when this Turn started.
    pub(crate) mounts: BTreeMap<String, bool>,
    #[serde(default = "anchor_platform_session::network_allowed_default")]
    pub(crate) network_allowed: bool,
}

impl MountSnapshot {
    pub(crate) fn new(session: &str, turn: &str, mounts: &BTreeMap<String, bool>) -> Self {
        Self {
            format: 2,
            session: session.to_owned(),
            turn: turn.to_owned(),
            mounts: mounts.clone(),
            network_allowed: true,
        }
    }
}

/// One path component that cannot be `.`, `..`, or a separator.
///
/// A Session id and a Turn id both come from the Session store, but this is the
/// boundary that writes a path: it must not trust them.
fn component(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "_".to_owned()
    } else {
        cleaned
    }
}

pub(crate) fn snapshot_path(state_root: &Path, session: &str, turn: &str) -> PathBuf {
    state_root
        .join(SNAPSHOT_DIR)
        .join(component(session))
        .join(format!("{}.json", component(turn)))
}

/// Write the snapshot **only if it is not there yet**.
///
/// A retried Turn must reuse the switches its first attempt saw; overwriting them
/// would let a retry silently change the tool set. Returns whether this call wrote
/// it (false means an earlier attempt's snapshot was kept).
pub(crate) fn write_snapshot(
    path: &Path,
    snapshot: &MountSnapshot,
) -> Result<bool, std::io::Error> {
    if path.exists() {
        return Ok(false);
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "snapshot path has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec(snapshot).map_err(std::io::Error::other)?;
    use std::io::Write;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error.error),
    }
    std::fs::File::open(parent)?.sync_all()?;
    Ok(true)
}

/// A malformed or unreadable permission fact must never recover live grants.
pub(crate) fn read_snapshot_strict(
    path: &Path,
    session: &str,
    turn: &str,
) -> Result<Option<MountSnapshot>, String> {
    let Some(snapshot) = crate::assistant::read_json::<MountSnapshot>(path)
        .map_err(|error| format!("Turn Plugin mount snapshot unavailable: {error}"))?
    else {
        return Ok(None);
    };
    if !matches!(snapshot.format, 1 | 2) || snapshot.session != session || snapshot.turn != turn {
        return Err("Turn Plugin mount snapshot identity changed".into());
    }
    Ok(Some(snapshot))
}

/// Lenient reader for legacy fixture assertions. Execution uses the strict
/// reader so corrupt permission facts never select live Session switches.
#[cfg(test)]
pub(crate) fn read_snapshot(path: &Path, session: &str, turn: &str) -> Option<MountSnapshot> {
    let bytes = std::fs::read(path).ok()?;
    parse_snapshot(&bytes, session, turn)
}

fn parse_snapshot(bytes: &[u8], session: &str, turn: &str) -> Option<MountSnapshot> {
    let snapshot: MountSnapshot = serde_json::from_slice(bytes).ok()?;
    // A snapshot belongs to exactly one Turn of one Session: a file that names
    // something else is not this Turn's snapshot, and must not be presented as one.
    if !matches!(snapshot.format, 1 | 2) || snapshot.session != session || snapshot.turn != turn {
        return None;
    }
    Some(snapshot)
}

/// Reclaim only snapshots whose exact identity agrees with the Session store.
/// The legacy path encoding is shared by ids such as `a:b` and `a_b`: neither
/// the directory name nor a JSON identity without a real Turn proves ownership.
pub(crate) fn cleanup_snapshots(
    state_root: &Path,
    session: &str,
    turns: &[String],
) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, fsync, openat, statat, unlinkat};
    use rustix::io::Errno;
    use std::io::Read;

    let Some(root) = open_snapshot_directory(&state_root.join(SNAPSHOT_DIR))? else {
        return Ok(());
    };
    let session_component = component(session);
    let directory = match openat(
        &root,
        session_component.as_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for turn in turns {
        let path = snapshot_path(state_root, session, turn);
        let name = path.file_name().expect("snapshot file name");
        let metadata = match statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(metadata) => metadata,
            Err(Errno::NOENT) => continue,
            Err(error) => return Err(error.into()),
        };
        if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
            continue;
        }
        let file = match openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(file) => file,
            Err(Errno::NOENT | Errno::LOOP) => continue,
            Err(error) => return Err(error.into()),
        };
        let mut file = std::fs::File::from(file);
        if !file.metadata()?.is_file() {
            continue;
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if parse_snapshot(&bytes, session, turn).is_none() {
            continue;
        }
        match unlinkat(&directory, name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(error) => return Err(error.into()),
        }
    }
    fsync(&directory)?;
    // Unknown files, malformed facts and another Session's snapshots keep this
    // shared directory alive. Never remove its contents recursively.
    match unlinkat(&root, session_component.as_str(), AtFlags::REMOVEDIR) {
        Ok(()) => fsync(&root).map_err(Into::into),
        Err(Errno::NOENT | Errno::NOTEMPTY) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn open_snapshot_directory(path: &Path) -> std::io::Result<Option<rustix::fd::OwnedFd>> {
    use rustix::fs::{Mode, OFlags, open, openat};
    use std::path::Component;

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = open("/", flags, Mode::empty())?;
    for component in std::path::absolute(path)?.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => match openat(&directory, name, flags, Mode::empty()) {
                Ok(child) => directory = child,
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(error) => return Err(error.into()),
            },
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "snapshot directory must be an absolute host path without symlinks",
                ));
            }
        }
    }
    Ok(Some(directory))
}

/// The mounts one Plugin-bearing call actually uses, and why the others are off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedMounts {
    /// The switches this call applies, by Plugin id.
    pub(crate) mounts: BTreeMap<String, bool>,
    /// Whether a Turn snapshot was available.
    pub(crate) snapshot: bool,
    /// Plugins mounted because the Graph's declaration defaulted them on, with no
    /// Session switch involved (so no audit event exists for them).
    pub(crate) by_declaration: Vec<String>,
    /// Plugins mounted because the Session's switches (a human's click) said so.
    pub(crate) by_switch: Vec<String>,
    /// One line per Plugin that is *not* mounted, and why.
    pub(crate) reasons: Vec<String>,
}

/// Apply the Session's switches (or its snapshot) on top of the Graph's defaults.
///
/// `declared` scopes the reasons: the returns mounts may contain a Plugin the
/// Graph never declared (the human mounted it), which is exactly the point.
pub(crate) fn resolve(
    snapshot: Option<&MountSnapshot>,
    session: &BTreeMap<String, bool>,
    declared: &[(&str, bool, Option<bool>)],
    installed: &BTreeMap<String, bool>,
) -> ResolvedMounts {
    let (switches, from_snapshot) = match snapshot {
        Some(snapshot) => (&snapshot.mounts, true),
        None => (session, false),
    };
    let mut mounts = BTreeMap::new();
    let mut reasons = Vec::new();
    let mut by_declaration = Vec::new();
    let mut by_switch = Vec::new();
    for (id, required, default) in declared {
        if *id == crate::plugin_network::NETWORK_PLUGIN {
            continue;
        }
        if !installed.get(*id).copied().unwrap_or(false) {
            reasons.push(format!("Plugin `{id}` is not available in this Library"));
            continue;
        }
        if *required {
            mounts.insert((*id).to_owned(), true);
            by_declaration.push((*id).to_owned());
            continue;
        }
        let switched = switches.get(*id).copied();
        let on = switched.unwrap_or_else(|| default.unwrap_or(true));
        if on {
            mounts.insert((*id).to_owned(), true);
            if switched.is_some() {
                by_switch.push((*id).to_owned());
            } else {
                by_declaration.push((*id).to_owned());
            }
        } else {
            reasons.push(format!("Plugin `{id}` is switched off for this Session"));
        }
    }
    // A Plugin the human mounted that the Graph does not declare is still
    // authorized by that click: the Graph's declaration is a default, not a gate.
    for (id, on) in switches {
        if id == crate::plugin_network::NETWORK_PLUGIN || !*on || mounts.contains_key(id) {
            continue;
        }
        if !installed.get(id).copied().unwrap_or(false) {
            reasons.push(format!("Plugin `{id}` is not available in this Library"));
            continue;
        }
        mounts.insert(id.clone(), true);
        by_switch.push(id.clone());
    }
    by_declaration.sort();
    by_switch.sort();
    ResolvedMounts {
        mounts,
        snapshot: from_snapshot,
        by_declaration,
        by_switch,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn switches(entries: &[(&str, bool)]) -> BTreeMap<String, bool> {
        entries
            .iter()
            .map(|(id, on)| ((*id).to_owned(), *on))
            .collect()
    }

    #[test]
    fn a_snapshot_is_written_once_and_a_retry_keeps_the_first_one() {
        let root = tempfile::tempdir().unwrap();
        let path = snapshot_path(root.path(), "session-one", "turn-1");
        let first = MountSnapshot::new("session-one", "turn-1", &switches(&[("demo", true)]));
        assert!(write_snapshot(&path, &first).unwrap());
        // The retry sees a different Session value, and must still read the first.
        let retry = MountSnapshot::new("session-one", "turn-1", &switches(&[("demo", false)]));
        assert!(!write_snapshot(&path, &retry).unwrap());
        let read = read_snapshot(&path, "session-one", "turn-1").unwrap();
        assert_eq!(read.mounts, switches(&[("demo", true)]));
        assert_eq!(read.turn, "turn-1");
    }

    #[test]
    fn a_missing_snapshot_reads_as_none_instead_of_failing() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_snapshot(&snapshot_path(root.path(), "s", "t"), "s", "t").is_none());
        // A malformed file is also "no snapshot": an old Run must still run.
        let path = snapshot_path(root.path(), "s", "t");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(read_snapshot(&path, "s", "t").is_none());
    }

    #[test]
    fn required_is_always_mounted_and_optional_follows_the_session() {
        let session = switches(&[("optional-four", false), ("mounted-not-declared", true)]);
        let installed = switches(&[
            ("required-one", true),
            ("optional-two", true),
            ("optional-four", true),
            ("mounted-not-declared", true),
        ]);
        let resolved = resolve(
            None,
            &session,
            &[
                ("required-one", true, None),
                ("optional-two", false, None),
                ("optional-three", false, Some(false)),
                ("optional-four", false, Some(true)),
            ],
            &installed,
        );
        // Required is on; an undeclared default is on; `default: false` keeps it
        // off; the Session's explicit off wins; and a mounted-but-undeclared Plugin
        // stays mounted.
        assert_eq!(
            resolved.mounts,
            switches(&[
                ("mounted-not-declared", true),
                ("optional-two", true),
                ("required-one", true),
            ])
        );
        assert!(!resolved.snapshot);
        // Why each one is mounted is part of the result, not an absence of reasons.
        assert_eq!(resolved.by_declaration, ["optional-two", "required-one"]);
        assert_eq!(resolved.by_switch, ["mounted-not-declared"]);
        assert_eq!(resolved.reasons.len(), 2, "{:?}", resolved.reasons);
        assert!(
            resolved
                .reasons
                .iter()
                .any(|reason| reason.contains("optional-four"))
        );
        assert!(
            resolved
                .reasons
                .iter()
                .any(|reason| reason.contains("optional-three"))
        );
    }

    #[test]
    fn a_snapshot_wins_over_the_sessions_current_switches() {
        let session = switches(&[("demo", true)]);
        let snapshot = MountSnapshot::new("s", "t", &switches(&[("demo", false)]));
        let resolved = resolve(
            Some(&snapshot),
            &session,
            &[("demo", false, None)],
            &switches(&[("demo", true)]),
        );
        assert!(resolved.mounts.is_empty(), "{:?}", resolved.mounts);
        assert!(resolved.snapshot);
    }

    #[test]
    fn an_uninstalled_plugin_is_never_mounted_and_says_so() {
        let resolved = resolve(
            None,
            &switches(&[("gone", true)]),
            &[("gone", true, None)],
            &BTreeMap::new(),
        );
        assert!(resolved.mounts.is_empty(), "{:?}", resolved.mounts);
        assert!(
            resolved.reasons[0].contains("is not available in this Library"),
            "{:?}",
            resolved.reasons
        );
    }
}
