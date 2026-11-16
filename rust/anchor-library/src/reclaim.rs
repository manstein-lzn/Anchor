//! Reclaim what no living view points at (standard invariant 8).
//!
//! A Plugin exists once (`plugins/<id>`) and declares which packages it needs. For
//! each installed declaration the host keeps one *view*
//! (`dependencies/views/<id>/<declaration digest>`) whose links point into the
//! shared store (`dependencies/store/<package>/<version>`). A package version may
//! be shared by many views, and a view becomes garbage the moment the Plugin's
//! declaration no longer maps to it.
//!
//! Two passes, in this order, because a view is what references a store entry:
//!
//! 1. remove orphan views — directories under `views/` that no *installed* Plugin's
//!    current declaration resolves to;
//! 2. recompute and remove every `store/<package>/<version>` that no remaining
//!    (living) view references, then the package directory if it is empty.
//!
//! Nothing here is optimistic. A declaration that cannot be read, a view that
//! cannot be walked, an unexpected shape under `views/`: all of them mean "this
//! might be referenced", and then nothing is deleted at all. Deleting is the
//! exceptional outcome, and the report always says what was kept and why.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use anchor_graph_host::FilePluginCatalog;

/// What one reclamation pass did, or refused to do.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReclaimReport {
    /// Whether this run was allowed to delete (`false` is a dry run).
    pub applied: bool,
    /// Why nothing was deleted at all, when that is the honest answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred: Option<String>,
    /// Why the *second* pass (package versions) was abandoned. The first pass may
    /// already have removed views, and the report keeps them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_phase_abandoned: Option<String>,
    /// Orphan views: on disk, referenced by no installed declaration.
    pub orphan_views: Vec<String>,
    /// Package versions no living view references, as `<package>/<version>`.
    pub unreferenced_package_versions: Vec<String>,
    pub removed_views: Vec<String>,
    pub removed_package_versions: Vec<String>,
    /// Package versions a living view still points at.
    pub kept_referenced: usize,
    /// Reasons a candidate was conservatively kept.
    pub kept_reasons: Vec<String>,
    /// Bytes of everything this run judged removable, dry run or not.
    pub reclaimable_bytes: u64,
    /// Bytes the removal actually freed (0 for a dry run, and for a refused run).
    pub reclaimed_bytes: u64,
}

impl ReclaimReport {
    fn refused(reason: impl Into<String>) -> Self {
        Self {
            applied: false,
            deferred: Some(reason.into()),
            store_phase_abandoned: None,
            orphan_views: Vec::new(),
            unreferenced_package_versions: Vec::new(),
            removed_views: Vec::new(),
            removed_package_versions: Vec::new(),
            kept_referenced: 0,
            kept_reasons: Vec::new(),
            reclaimable_bytes: 0,
            reclaimed_bytes: 0,
        }
    }
}

#[derive(Debug)]
pub enum ReclaimError {
    /// The Library could not be locked: another writer is installing or landing a
    /// view right now.
    Busy,
    Io(io::Error),
}

impl std::fmt::Display for ReclaimError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => write!(formatter, "another writer holds the Library lock"),
            Self::Io(error) => write!(formatter, "reclamation failed: {error}"),
        }
    }
}

impl std::error::Error for ReclaimError {}

impl From<io::Error> for ReclaimError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reclaim everything no living view points at, taking the Library's write locks.
///
/// `apply == false` reports exactly what would be deleted and touches nothing.
pub fn reclaim(root: &Path, apply: bool) -> Result<ReclaimReport, ReclaimError> {
    reclaim_with(root, apply, false)
}

/// Reclaim while the caller already holds the Plugin installation lock.
pub fn reclaim_after_install(root: &Path, apply: bool) -> Result<ReclaimReport, ReclaimError> {
    reclaim_with(root, apply, true)
}

fn reclaim_with(
    root: &Path,
    apply: bool,
    plugins_lock_held: bool,
) -> Result<ReclaimReport, ReclaimError> {
    // The store/view lock first, then the Plugin lock: an installation lands a
    // view (store lock) before it publishes the Plugin (Plugin lock), and never
    // holds both, so this order cannot deadlock. Neither acquisition waits: a
    // reclamation that raced a writer would rather report "busy" than hold a CLI
    // open for the minutes a dependency install can take.
    let dependencies = root.join(anchor_graph_host::DEPENDENCIES_DIRECTORY);
    if !dependencies.is_dir() {
        // Nothing was ever landed here: an empty report, not a failure. (The host
        // sweeps at every start-up, and an error line per start-up for a Library
        // that has no dependencies at all would be noise, not information.)
        return Ok(ReclaimReport {
            applied: apply,
            deferred: None,
            store_phase_abandoned: None,
            orphan_views: Vec::new(),
            unreferenced_package_versions: Vec::new(),
            removed_views: Vec::new(),
            removed_package_versions: Vec::new(),
            kept_referenced: 0,
            kept_reasons: Vec::new(),
            reclaimable_bytes: 0,
            reclaimed_bytes: 0,
        });
    }
    let _store_lock = match lock_nonblocking(&dependencies, ".lock", apply) {
        Ok(lock) => Some(lock),
        Err(LockError::Absent) => None,
        Err(LockError::Busy) => {
            return Ok(ReclaimReport::refused(
                "another writer is landing packages or views; nothing was deleted, run reclaim again",
            ));
        }
        Err(LockError::Io(error)) => return Err(error.into()),
    };
    let plugins = root.join("plugins");
    let _plugins_lock = if plugins_lock_held {
        None
    } else if plugins.is_dir() {
        match lock_nonblocking(&plugins, ".install.lock", apply) {
            Ok(lock) => Some(lock),
            Err(LockError::Absent) => None,
            Err(LockError::Busy) => {
                return Ok(ReclaimReport::refused(
                    "a Plugin installation is in progress; nothing was deleted, run reclaim again",
                ));
            }
            Err(LockError::Io(error)) => return Err(error.into()),
        }
    } else {
        None
    };
    // Both locks stay held for the whole pass: a view must not be created while we
    // are deciding it is garbage, and a package must not land while we are deciding
    // it is unreferenced.
    reclaim_locked(root, apply)
}

enum LockError {
    Busy,
    /// No lock file exists and this run may not create one (a dry run): there is no
    /// writer to wait for.
    Absent,
    Io(io::Error),
}

/// `create` is false for a dry run: a report must not leave new files behind, and
/// a missing lock file means no writer has ever started here.
fn lock_nonblocking(
    directory: &Path,
    name: &str,
    create: bool,
) -> Result<anchor_file_lease::FileLease, LockError> {
    if !directory.is_dir() {
        return Err(LockError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", directory.display()),
        )));
    }
    let (_, handle) = crate::filesystem::directory(directory, false)
        .map_err(|error| LockError::Io(io::Error::other(error.to_string())))?;
    crate::filesystem::lease_named(
        &handle,
        name,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
        create,
    )
    .map_err(|error| match error {
        crate::InstallError::CatalogBusy => LockError::Busy,
        crate::InstallError::Io(error) if !create && error.kind() == io::ErrorKind::NotFound => {
            LockError::Absent
        }
        crate::InstallError::Io(error) => LockError::Io(error),
        error => LockError::Io(io::Error::other(error.to_string())),
    })
}

/// The two passes, with the caller's locks already held.
fn reclaim_locked(root: &Path, apply: bool) -> Result<ReclaimReport, ReclaimError> {
    let views = root
        .join(anchor_graph_host::DEPENDENCIES_DIRECTORY)
        .join(anchor_graph_host::VIEWS_DIRECTORY);
    let store = anchor_graph_host::store_root(root);
    let plugins = root.join("plugins");

    // Temporary views carry durable ownership and exact references in Library.
    // They survive restart and are released only by trusted Session cleanup.
    // Missing or corrupt facts do not authorize deleting any dependency bytes.
    let development = match crate::development::development_views(root) {
        Ok(views) => views,
        Err(error) => {
            let mut report = ReclaimReport::refused(
                "development dependency references could not be verified; nothing was deleted",
            );
            report.kept_reasons.push(error.to_string());
            return Ok(report);
        }
    };
    let revisions = match crate::revisions::revision_views(root) {
        Ok(views) => views,
        Err(error) => {
            let mut report = ReclaimReport::refused(
                "retained Plugin revision references could not be verified; nothing was deleted",
            );
            report.kept_reasons.push(error.to_string());
            return Ok(report);
        }
    };
    let canonical_store = store.canonicalize().unwrap_or_else(|_| store.clone());
    for view in development.iter().chain(&revisions) {
        if let Err(error) = store_references(view, &canonical_store).and_then(|found| {
            if found.is_empty() {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "temporary dependency view has no package references",
                ))
            } else {
                Ok(found)
            }
        }) {
            let mut report = ReclaimReport::refused(
                "development view package references could not be verified; nothing was deleted",
            );
            report
                .kept_reasons
                .push(format!("{}: {error}", display_relative(root, view)));
            return Ok(report);
        }
    }

    // Pass one: which views are alive, and which Plugin ids we can speak about.
    // A declaration we cannot read might point anywhere, so it stops the whole
    // run before anything is deleted.
    let mut live: BTreeSet<PathBuf> = revisions.iter().cloned().collect();
    let mut uncertain: Vec<String> = Vec::new();
    let mut known_ids = BTreeSet::new();
    let catalog = FilePluginCatalog::new(root);
    for entry in read_directory(&plugins)? {
        if !entry.is_dir() {
            continue;
        }
        let id = entry
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !anchor_runtime::identifier::is_plugin_id(&id) {
            // The installer stages its transaction inside `plugins/`
            // (`.anchor-plugin-<random>`). It is not a Plugin, and enumerating it
            // as one would (correctly) fail closed and block every reclamation.
            continue;
        }
        known_ids.insert(id.clone());
        match catalog.definition(&id) {
            Ok(definition) => {
                if definition.dependencies.is_empty() {
                    // No declaration: no view is live for this Plugin.
                    continue;
                }
                live.insert(anchor_graph_host::plugin_view_path(
                    root,
                    &id,
                    &definition.dependencies.declaration_digest(),
                ));
            }
            Err(error) => uncertain.push(format!(
                "Plugin `{id}`: its declaration could not be read ({error}), so everything it might reference is kept"
            )),
        }
    }
    if !uncertain.is_empty() {
        let mut report = ReclaimReport::refused(
            "a Plugin manifest could not be read; a declaration we cannot parse might point anywhere, so nothing was deleted",
        );
        report.kept_reasons = uncertain;
        return Ok(report);
    }
    // No installed Plugin at all, but the shared store or the views are not empty:
    // every byte in them would look unreferenced, and clearing the store of a
    // deployment whose `plugins/` is missing (a mistyped Library root, a partly
    // restored backup) is the one mistake that cannot be undone from here.
    if known_ids.is_empty()
        && development.is_empty()
        && revisions.is_empty()
        && !match crate::development::has_released_development(root) {
            Ok(released) => released,
            Err(error) => {
                let mut report = ReclaimReport::refused(
                    "released development ownership could not be verified; nothing was deleted",
                );
                report.kept_reasons.push(error.to_string());
                return Ok(report);
            }
        }
        && (!read_directory(&views)?.is_empty() || !read_directory(&store)?.is_empty())
    {
        return Ok(ReclaimReport::refused(
            "no Plugin is installed while dependency views or the shared store are not empty; refusing to delete anything",
        ));
    }

    let mut report = ReclaimReport {
        applied: apply,
        deferred: None,
        store_phase_abandoned: None,
        orphan_views: Vec::new(),
        unreferenced_package_versions: Vec::new(),
        removed_views: Vec::new(),
        removed_package_versions: Vec::new(),
        kept_referenced: 0,
        kept_reasons: Vec::new(),
        reclaimable_bytes: 0,
        reclaimed_bytes: 0,
    };

    // Canonical spelling, so a comparison cannot be fooled by `..` or symlinks in
    // the Library path itself.
    let live = live
        .iter()
        .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()))
        .collect::<BTreeSet<_>>();

    // Views this pass judges to be garbage, whether or not a dry run removes them:
    // the second pass must see the world the *plan* describes.
    let mut planned_orphans: BTreeSet<PathBuf> = BTreeSet::new();
    for view in read_directory(&views)? {
        if !view.is_dir() || is_private_name(&view) {
            report.kept_reasons.push(format!(
                "{}: not a view directory",
                display_relative(root, &view)
            ));
            continue;
        }
        let id = view
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        for declaration in read_directory(&view)? {
            if !declaration.is_dir() {
                report.kept_reasons.push(format!(
                    "{}: not a view directory",
                    display_relative(root, &declaration)
                ));
                continue;
            }
            let canonical = declaration
                .canonicalize()
                .unwrap_or_else(|_| declaration.clone());
            if live.contains(&canonical) {
                continue;
            }
            if !known_ids.contains(&id) {
                // No installed Plugin carries this id: a failed installation's
                // leftover, or a tree an operator removed by hand. We cannot prove
                // what a future installation of that id would need, so we keep it
                // and say so instead of guessing.
                report.kept_reasons.push(format!(
                    "{}: no installed Plugin has the id `{id}`",
                    display_relative(root, &declaration)
                ));
                continue;
            }
            planned_orphans.insert(canonical.clone());
            let relative = display_relative(root, &declaration);
            report.orphan_views.push(relative.clone());
            let size = directory_size(&declaration)?;
            report.reclaimable_bytes += size;
            if apply {
                report.reclaimed_bytes += size;
                fs::remove_dir_all(&declaration)?;
                report.removed_views.push(relative);
            }
        }
    }

    // Pass two: what *every surviving view* references — including the ones pass one
    // kept because their Plugin id is not installed. A view that is still on disk
    // may still be the only thing pointing at a package version, and deleting that
    // package would leave the view dangling.
    let mut surviving = surviving_views(root, &planned_orphans)?;
    surviving.extend(development);
    let mut referenced: BTreeSet<PathBuf> = BTreeSet::new();
    let mut unreadable_view = None;
    let canonical_store = store.canonicalize().unwrap_or_else(|_| store.clone());
    for view in &surviving {
        match store_references(view, &canonical_store) {
            Ok(found) => referenced.extend(found),
            Err(error) => {
                unreadable_view = Some((view.clone(), error));
                break;
            }
        }
    }
    if let Some((view, error)) = unreadable_view {
        // Not knowing what a surviving view points at means every store entry might
        // be referenced. Keep them all, and keep the report of pass one: those
        // deletions really happened.
        abandon_store_phase(&mut report, root, &view, &error);
        return Ok(report);
    }

    for package in read_directory(&store)? {
        if !package.is_dir() || is_private_name(&package) {
            if is_private_name(&package) {
                continue;
            }
            report.kept_reasons.push(format!(
                "{}: not a package directory",
                display_relative(root, &package)
            ));
            continue;
        }
        let name = package
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        for version in read_directory(&package)? {
            if !version.is_dir() {
                report.kept_reasons.push(format!(
                    "{}: not a version directory",
                    display_relative(root, &version)
                ));
                continue;
            }
            let canonical = version.canonicalize().unwrap_or_else(|_| version.clone());
            if referenced.contains(&canonical) {
                report.kept_referenced += 1;
                continue;
            }
            let relative = format!(
                "{name}/{}",
                version
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            );
            report.unreferenced_package_versions.push(relative.clone());
            let size = directory_size(&version)?;
            report.reclaimable_bytes += size;
            if apply {
                report.reclaimed_bytes += size;
                fs::remove_dir_all(&version)?;
                report.removed_package_versions.push(relative);
            }
        }
        if apply && read_directory(&package)?.is_empty() {
            let _ = fs::remove_dir(&package);
        }
    }
    Ok(report)
}

/// Record that the package pass was abandoned without erasing pass one.
///
/// The deletions pass one performed are facts: the report keeps them, the byte
/// count keeps them, and `store_phase_abandoned` says explicitly that the second
/// half of the sweep did not happen, so an operator reading the line knows the
/// store was left untouched and why.
fn abandon_store_phase(report: &mut ReclaimReport, root: &Path, view: &Path, error: &io::Error) {
    report.kept_reasons.push(format!(
        "{}: a surviving view could not be read ({error})",
        display_relative(root, view)
    ));
    report.store_phase_abandoned =
        Some("a surviving view could not be read, so no package version was deleted".to_owned());
}

/// Every view declaration directory that exists right now.
///
/// This is deliberately *not* the live set: a view whose Plugin id is unknown was
/// kept by pass one, and it still references whatever it references.
fn surviving_views(
    root: &Path,
    planned_orphans: &BTreeSet<PathBuf>,
) -> Result<Vec<PathBuf>, ReclaimError> {
    let views = root
        .join(anchor_graph_host::DEPENDENCIES_DIRECTORY)
        .join(anchor_graph_host::VIEWS_DIRECTORY);
    let mut surviving = Vec::new();
    for view in read_directory(&views)? {
        if !view.is_dir() || is_private_name(&view) {
            continue;
        }
        for declaration in read_directory(&view)? {
            if !declaration.is_dir() {
                continue;
            }
            let canonical = declaration
                .canonicalize()
                .unwrap_or_else(|_| declaration.clone());
            if planned_orphans.contains(&canonical) {
                continue;
            }
            surviving.push(canonical);
        }
    }
    surviving.sort();
    Ok(surviving)
}

/// The `store/<package>/<version>` directories one view's links point at.
pub(crate) fn store_references(view: &Path, store: &Path) -> Result<BTreeSet<PathBuf>, io::Error> {
    let mut references = BTreeSet::new();
    let mut pending = vec![view.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !kind.is_symlink() {
                continue;
            }
            let target = fs::read_link(entry.path())?;
            let resolved = if target.is_absolute() {
                target
            } else {
                entry.path().parent().unwrap_or(Path::new("/")).join(target)
            };
            let normalized = normalize(&resolved);
            let Ok(relative) = normalized.strip_prefix(store) else {
                if entry
                    .path()
                    .components()
                    .any(|component| component.as_os_str() == "site-packages")
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "dependency view points outside the shared store",
                    ));
                }
                continue;
            };
            let mut components = relative.components();
            let (Some(package), Some(version)) = (components.next(), components.next()) else {
                continue;
            };
            let package_version = store.join(package).join(version);
            if !package_version.is_dir() || !normalized.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "dependency view points at missing shared package bytes",
                ));
            }
            references.insert(package_version);
        }
    }
    Ok(references)
}

/// The installer's own files (`.lock`, `.install.lock`, staged transactions).
fn is_private_name(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
}

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::Normal(name) => normalized.push(name),
            std::path::Component::Prefix(_) => {}
        }
    }
    normalized
}

fn read_directory(directory: &Path) -> Result<Vec<PathBuf>, io::Error> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut paths = entries
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn directory_size(directory: &Path) -> Result<u64, io::Error> {
    let mut total = 0;
    let mut pending = vec![directory.to_owned()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let metadata = entry.metadata();
            match metadata {
                Ok(metadata) if metadata.is_dir() => pending.push(entry.path()),
                Ok(metadata) => total += metadata.len(),
                Err(_) => {}
            }
        }
    }
    Ok(total)
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// A compact view of the report for a log line: what it did, what it kept.
pub fn summarize(report: &ReclaimReport) -> BTreeMap<String, String> {
    let mut summary = BTreeMap::new();
    summary.insert("applied".to_owned(), report.applied.to_string());
    if let Some(deferred) = &report.deferred {
        summary.insert("deferred".to_owned(), deferred.clone());
    }
    if let Some(abandoned) = &report.store_phase_abandoned {
        summary.insert("store_phase_abandoned".to_owned(), abandoned.clone());
    }
    summary.insert(
        "removed_views".to_owned(),
        report.removed_views.len().to_string(),
    );
    summary.insert(
        "removed_package_versions".to_owned(),
        report.removed_package_versions.len().to_string(),
    );
    summary.insert(
        "kept_referenced".to_owned(),
        report.kept_referenced.to_string(),
    );
    summary.insert(
        "kept_uncertain".to_owned(),
        report.kept_reasons.len().to_string(),
    );
    summary.insert(
        "reclaimable_bytes".to_owned(),
        report.reclaimable_bytes.to_string(),
    );
    summary.insert(
        "reclaimed_bytes".to_owned(),
        report.reclaimed_bytes.to_string(),
    );
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Abandoning the package pass must not erase what the view pass already did.
    #[test]
    fn an_abandoned_package_pass_keeps_the_removals_that_happened() {
        let mut report = ReclaimReport {
            applied: true,
            deferred: None,
            store_phase_abandoned: None,
            orphan_views: vec!["dependencies/views/demo/old".to_owned()],
            unreferenced_package_versions: vec!["idna/3.6".to_owned()],
            removed_views: vec!["dependencies/views/demo/old".to_owned()],
            removed_package_versions: Vec::new(),
            kept_referenced: 3,
            kept_reasons: Vec::new(),
            reclaimable_bytes: 128,
            reclaimed_bytes: 128,
        };
        let error = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        abandon_store_phase(
            &mut report,
            Path::new("/library"),
            Path::new("/library/dependencies/views/demo/current"),
            &error,
        );
        assert_eq!(
            report.removed_views,
            vec!["dependencies/views/demo/old".to_owned()],
            "a deletion that happened stays in the report"
        );
        assert_eq!(report.reclaimed_bytes, 128, "and so does the byte count");
        assert_eq!(report.orphan_views.len(), 1);
        assert_eq!(
            report.store_phase_abandoned.as_deref(),
            Some("a surviving view could not be read, so no package version was deleted")
        );
        let summary = summarize(&report);
        assert_eq!(summary.get("removed_views").map(String::as_str), Some("1"));
        assert_eq!(
            summary.get("reclaimed_bytes").map(String::as_str),
            Some("128")
        );
        assert!(summary.contains_key("store_phase_abandoned"));
    }
}
