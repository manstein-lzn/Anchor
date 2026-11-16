//! How one Plugin's dependency view becomes per-invocation authority.
//!
//! A view is a light-weight Python environment: a `pyvenv.cfg`, a `bin/python`
//! that is a symlink to the deployment's interpreter, and one symlink per
//! installed distribution into the shared store. It is read-only content, so an
//! invocation mounts it and the parts of the store it points into, and adds its
//! `bin` to the command PATH. Nothing is copied and nothing is writable.
//!
//! Both halves of the authority (the sandbox grant and the request's mounts) are
//! assembled from the same value, because a grant that never reaches the request
//! would leave the server running without its environment, and a request the
//! grant does not authorize is refused by the sandbox adapter.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::plugin_library::Catalog;
use anchor_graph_host::{
    PluginDefinition, PluginDependencies, plugin_view_path, resolved_library_root, store_root,
};
#[cfg(test)]
use anchor_runtime::graph::PluginBinding;
use anchor_runtime::{ReadOnlyInput, SandboxRequest, WritableInput};
use anchor_sandbox_bwrap::BubblewrapSandbox;

/// Why one Plugin's dependency view cannot be used.
///
/// The boundary keeps two causes apart: a view this invocation needs but cannot
/// build or find is a deployment problem, while content that is present but not
/// usable is corruption. Collapsing them into one string is how the path used to
/// relabel the first as the second.
#[derive(Debug)]
pub(crate) enum Unusable {
    /// A configuration or authorization check completed with a known refusal.
    KnownFailure(String),
    /// The recorded fact cannot be honored: the content is missing, changed, or
    /// writable again.
    Recorded(String),
    /// This host cannot serve the view at all.
    Unconfigured(String),
}

impl std::fmt::Display for Unusable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KnownFailure(reason) | Self::Recorded(reason) | Self::Unconfigured(reason) => {
                formatter.write_str(reason)
            }
        }
    }
}

impl Unusable {
    /// The Kernel scheduler's class for this cause, so every path agrees.
    pub(crate) fn into_graph_error(self) -> anchor_runtime::graph::GraphError {
        use anchor_runtime::graph::GraphError;
        match self {
            Self::Recorded(reason) => GraphError::CorruptRun(reason),
            Self::KnownFailure(reason) | Self::Unconfigured(reason) => {
                GraphError::Unsupported(reason)
            }
        }
    }

    pub(crate) fn recorded(message: impl Into<String>) -> Self {
        Self::Recorded(message.into())
    }

    pub(crate) fn unconfigured(message: impl Into<String>) -> Self {
        Self::Unconfigured(message.into())
    }
}

impl From<crate::tool_host::PreparationError> for Unusable {
    fn from(error: crate::tool_host::PreparationError) -> Self {
        match error {
            crate::tool_host::PreparationError::KnownFailure(reason) => Self::KnownFailure(reason),
            crate::tool_host::PreparationError::Unavailable(reason) => Self::Unconfigured(reason),
        }
    }
}

impl From<anchor_library::RevisionError> for Unusable {
    fn from(error: anchor_library::RevisionError) -> Self {
        match error {
            anchor_library::RevisionError::DefaultUnavailable => {
                Self::KnownFailure("plugin_default_publication_pending".into())
            }
            error => Self::recorded(error.to_string()),
        }
    }
}

/// The file that makes a directory a Python environment. Without it `sys.prefix`
/// collapses to the interpreter's own prefix and the view imports nothing.
pub(crate) const VIEW_CONFIGURATION: &str = "pyvenv.cfg";

#[derive(Debug, Clone)]
pub(crate) struct PreparedView {
    view: PathBuf,
    /// The declaration this view belongs to: the identity a Run records as the
    /// view it actually used.
    declaration_digest: String,
    grants: Vec<ReadOnlyInput>,
}

impl PreparedView {
    pub(crate) fn for_candidate(
        view: &Path,
        store: &Path,
        declaration_digest: &str,
    ) -> Result<Self, String> {
        let view = view.canonicalize().map_err(|error| error.to_string())?;
        let configuration = std::fs::symlink_metadata(view.join(VIEW_CONFIGURATION))
            .map_err(|error| error.to_string())?;
        if !configuration.is_file() || configuration.file_type().is_symlink() {
            return Err("dependency view has no regular pyvenv.cfg".into());
        }
        let mut grants = vec![ReadOnlyInput::new(&view, &view)];
        for directory in referenced_store_directories(&view, store)? {
            grants.push(ReadOnlyInput::new(&directory, &directory));
        }
        grants.extend(interpreter_grants(&view)?);
        Ok(Self {
            view,
            declaration_digest: declaration_digest.into(),
            grants,
        })
    }
    /// The view this server launches on. Absolute and canonical.
    pub(crate) fn view(&self) -> &Path {
        &self.view
    }

    /// The declaration this view belongs to, as the run record observes it.
    pub(crate) fn declaration_digest(&self) -> &str {
        &self.declaration_digest
    }

    /// The directory that provides the view's own interpreter.
    fn bin(&self) -> PathBuf {
        self.view().join("bin")
    }

    /// Grant this invocation exactly the view and the store content it points at.
    pub(crate) fn authorize(
        &self,
        sandbox: Arc<BubblewrapSandbox>,
    ) -> Result<Arc<BubblewrapSandbox>, String> {
        let extended = sandbox
            .with_readonly_grants(&self.grants)
            .map_err(|error| error.to_string())?;
        extended
            .with_executable_dirs(&[self.bin()])
            .map(Arc::new)
            .map_err(|error| error.to_string())
    }

    /// Put the same mounts and PATH entry into this invocation's request.
    pub(crate) fn apply(&self, request: &mut SandboxRequest) {
        for grant in &self.grants {
            if !request.readonly_inputs.contains(grant) {
                request.readonly_inputs.push(grant.clone());
            }
        }
        let bin = self.bin();
        if !request.tool_dirs.contains(&bin) {
            request.tool_dirs.push(bin);
        }
    }
}

/// Verify one installed Plugin's view and describe how to mount it.
///
/// `None` means the Plugin declares no dependencies, which is not an error.
/// Everything else fails closed: a declared view that is missing, incomplete, or
/// points outside the shared store refuses the invocation instead of silently
/// running the server on the deployment's own interpreter.
#[cfg(test)]
pub(crate) fn prepare(
    library_root: &Path,
    binding: &PluginBinding,
) -> Result<Option<PreparedView>, Unusable> {
    // The binding names which Plugin this invocation is for. There is no digest
    // comparison: the Run uses the *current* Plugin (standard invariant 6), and
    // the version it actually used is recorded as an observation instead.
    prepare_for_plugin(library_root, &binding.id)
}

/// Whether this Plugin declares a view that is not present and usable.
///
/// The one installation gate asks this before it reports an already-installed
/// Plugin as up to date: a missing view is exactly what a repair has to fix.
pub(crate) fn view_needs_repair(library_root: &Path, plugin: &str) -> bool {
    // `Ok(None)` is "nothing declared", `Ok(Some(_))` is a complete view; only a
    // declared view that cannot be prepared is repair work.
    prepare_for_plugin(library_root, plugin).is_err()
}

/// The same preparation for one installed Plugin, without a Run binding.
///
/// The installer's repair path uses this to answer "does this Plugin already have
/// the view its declaration maps to?" without inventing a binding.
pub(crate) fn prepare_for_plugin(
    library_root: &Path,
    plugin: &str,
) -> Result<Option<PreparedView>, Unusable> {
    let catalog = Catalog::new(library_root);
    let definition = catalog
        .definition(plugin)
        .map_err(|error| Unusable::recorded(error.to_string()))?;
    prepare_definition_view(library_root, &definition)
}

pub(crate) fn prepare_definition_view(
    library_root: &Path,
    definition: &PluginDefinition,
) -> Result<Option<PreparedView>, Unusable> {
    prepare_dependency_view(library_root, &definition.id, &definition.dependencies)
}

pub(crate) fn prepare_dependency_view(
    library_root: &Path,
    plugin: &str,
    dependencies: &PluginDependencies,
) -> Result<Option<PreparedView>, Unusable> {
    if dependencies.is_empty() {
        return Ok(None);
    }
    let root = resolved_library_root(library_root);
    let view = plugin_view_path(&root, plugin, &dependencies.declaration_digest());
    let view = view.canonicalize().map_err(|_| {
        Unusable::recorded(format!(
            "Plugin `{plugin}` declares dependencies but its view `{}` is missing; install the Plugin again to rebuild it",
            view.display()
        ))
    })?;
    if !view.join(VIEW_CONFIGURATION).is_file() {
        return Err(Unusable::recorded(format!(
            "Plugin `{plugin}` view `{}` has no `{VIEW_CONFIGURATION}`, so it is not a usable environment",
            view.display()
        )));
    }
    let bin = view.join("bin");
    if !bin.join("python").exists() {
        return Err(Unusable::recorded(format!(
            "Plugin `{plugin}` view `{}` provides no interpreter at `bin/python`",
            view.display()
        )));
    }
    let store = store_root(&root)
        .canonicalize()
        .map_err(|_| Unusable::recorded("the shared dependency store is missing".to_owned()))?;
    let mut grants = vec![ReadOnlyInput::new(&view, &view)];
    grants.extend(interpreter_grants(&view).map_err(Unusable::recorded)?);
    for directory in referenced_store_directories(&view, &store).map_err(Unusable::recorded)? {
        let grant = ReadOnlyInput::new(&directory, &directory);
        if !grants.contains(&grant) {
            grants.push(grant);
        }
    }
    Ok(Some(PreparedView {
        view,
        declaration_digest: dependencies.declaration_digest(),
        grants,
    }))
}

/// Per-Plugin filesystem authority, independent of its implementation language.
/// Python views are optional; declared data paths keep the same write contract.
#[derive(Debug, Clone)]
pub(crate) struct PreparedPlugin {
    dependency_view: Option<PreparedView>,
    writable: Vec<WritableInput>,
}

impl PreparedPlugin {
    pub(crate) fn from_definition(
        library_root: &Path,
        definition: &PluginDefinition,
    ) -> Result<Self, Unusable> {
        Ok(Self {
            dependency_view: prepare_definition_view(library_root, definition)?,
            writable: mutable_grants(definition, &definition.directory)?,
        })
    }

    pub(crate) fn from_source(
        source: &anchor_library::ResolvedPluginSource,
    ) -> Result<Self, Unusable> {
        let definition = source.definition();
        let dependency_view = match source.dependency_identity() {
            Some(identity) => Some(
                PreparedView::for_candidate(
                    source.dependency_view().ok_or_else(|| {
                        Unusable::recorded("retained Plugin dependency view is missing")
                    })?,
                    &store_root(source.library_root()),
                    &identity.declaration_digest,
                )
                .map_err(Unusable::recorded)?,
            ),
            None => prepare_definition_view(source.library_root(), definition)?,
        };
        Ok(Self {
            dependency_view,
            writable: mutable_grants(definition, source.mutable_root())?,
        })
    }

    pub(crate) fn for_candidate(
        catalog: &Catalog,
        id: &str,
        dependency_view: Option<PreparedView>,
    ) -> Result<Self, String> {
        let definition = catalog.definition(id).map_err(|error| error.to_string())?;
        Ok(Self {
            dependency_view,
            writable: mutable_grants(&definition, &definition.directory)
                .map_err(|error| error.to_string())?,
        })
    }

    pub(crate) fn view(&self) -> Option<&Path> {
        self.dependency_view.as_ref().map(PreparedView::view)
    }

    pub(crate) fn declaration_digest(&self) -> Option<&str> {
        self.dependency_view
            .as_ref()
            .map(PreparedView::declaration_digest)
    }

    pub(crate) fn authorize(
        &self,
        sandbox: Arc<BubblewrapSandbox>,
    ) -> Result<Arc<BubblewrapSandbox>, String> {
        let sandbox = match &self.dependency_view {
            Some(view) => view.authorize(sandbox)?,
            None => sandbox,
        };
        sandbox
            .with_writable_grants(&self.writable)
            .map(Arc::new)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn apply(&self, request: &mut SandboxRequest) {
        if let Some(view) = &self.dependency_view {
            view.apply(request);
        }
        for grant in &self.writable {
            if !request.writable_inputs.contains(grant) {
                request.writable_inputs.push(grant.clone());
            }
        }
    }
}

fn mutable_grants(
    definition: &PluginDefinition,
    mutable_root: &Path,
) -> Result<Vec<WritableInput>, Unusable> {
    let plugin = &definition.id;
    let mut writable = Vec::with_capacity(definition.mutable.paths().len());
    let resolved = definition
        .mutable
        .resolve(mutable_root)
        .map_err(Unusable::recorded)?;
    for (declared, path) in definition.mutable.paths().iter().zip(resolved) {
        if !path.exists() {
            return Err(Unusable::recorded(format!(
                "Plugin `{plugin}` declares mutable path `{declared}`, which does not exist; install the Plugin again to provide it"
            )));
        }
        writable.push(WritableInput::new(
            &path,
            format!("/plugins/{plugin}/{}", declared.trim_end_matches('/')),
        ));
    }
    Ok(writable)
}

/// Mount only the deployment interpreter's installation prefix, preserving the
/// original path needed by its shared libraries. No home or tool-cache grant.
pub(crate) fn interpreter_grants(view: &Path) -> Result<Vec<ReadOnlyInput>, String> {
    let interpreter = view
        .join("bin/python")
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let prefix = interpreter
        .parent()
        .and_then(Path::parent)
        .ok_or("interpreter has no installation prefix")?;
    interpreter_prefix_grants(prefix)
}

pub(crate) fn interpreter_prefix_grants(prefix: &Path) -> Result<Vec<ReadOnlyInput>, String> {
    if ["/usr", "/bin", "/lib", "/lib64", "/sbin"]
        .iter()
        .any(|root| prefix.starts_with(root))
    {
        return Ok(Vec::new());
    }
    crate::tool_environment::validate_installation_prefix(prefix)?;
    Ok(vec![ReadOnlyInput::new(prefix, prefix)])
}

/// The `(package, version)` directories one view's pointers actually name.
///
/// Only these are mounted: a Plugin gets the bytes its own view references, not
/// every other Plugin's packages. A pointer that leaves the store is refused
/// rather than mounted, so a view cannot widen what a server may read.
pub(crate) fn referenced_store_directories(
    view: &Path,
    store: &Path,
) -> Result<Vec<PathBuf>, String> {
    let store = store
        .canonicalize()
        .map_err(|error| format!("the shared dependency store is unusable: {error}"))?;
    let mut referenced = BTreeSet::new();
    let mut pending = site_packages_directories(view)?;
    while let Some(site_packages) = pending.pop() {
        let entries = std::fs::read_dir(&site_packages)
            .map_err(|error| format!("{} is not readable: {error}", site_packages.display()))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| format!("{} is not readable: {error}", site_packages.display()))?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("{} is not readable: {error}", path.display()))?;
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !metadata.file_type().is_symlink() {
                // A view seeds its own files (`_virtualenv.py` and friends); they
                // live inside the view and need no store grant.
                continue;
            }
            let target = std::fs::read_link(&path)
                .map_err(|error| format!("{} is not readable: {error}", path.display()))?;
            let target = if target.is_absolute() {
                target
            } else {
                path.parent().unwrap_or(Path::new("/")).join(target)
            };
            let resolved = target.canonicalize().map_err(|_| {
                format!(
                    "view `{}` points at `{}`, which does not exist",
                    view.display(),
                    target.display()
                )
            })?;
            let relative = resolved.strip_prefix(&store).map_err(|_| {
                format!(
                    "view `{}` points at `{}`, which is outside the shared dependency store",
                    view.display(),
                    resolved.display()
                )
            })?;
            let Some(name) = relative.components().next() else {
                return Err(format!(
                    "view `{}` points at the store root itself",
                    view.display()
                ));
            };
            let version = relative.components().nth(1).ok_or_else(|| {
                format!(
                    "view `{}` points at `{}`, which is not a stored (package, version)",
                    view.display(),
                    resolved.display()
                )
            })?;
            let directory = store.join(name).join(version);
            if !directory.is_dir() {
                return Err(format!(
                    "view `{}` points at `{}`, which is not a stored (package, version)",
                    view.display(),
                    directory.display()
                ));
            }
            referenced.insert(directory);
        }
    }
    Ok(referenced.into_iter().collect())
}

/// Every `site-packages` directory a view provides.
pub(crate) fn site_packages_directories(view: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = BTreeSet::new();
    for lib in [view.join("lib"), view.join("lib64")] {
        let entries = match std::fs::read_dir(&lib) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{} is not readable: {error}", lib.display())),
        };
        for entry in entries {
            let entry =
                entry.map_err(|error| format!("{} is not readable: {error}", lib.display()))?;
            let candidate = entry.path().join("site-packages");
            if candidate.is_dir() {
                found.insert(candidate.canonicalize().unwrap_or(candidate));
            }
        }
    }
    if found.is_empty() {
        return Err(format!(
            "view `{}` has no site-packages directory",
            view.display()
        ));
    }
    Ok(found.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_library::tools::prepare_plugins_async;
    use anchor_graph_host::PluginCatalog;
    use anchor_runtime::SandboxPort;
    use anchor_sandbox_bwrap::BubblewrapPolicy;
    use std::fs;

    #[tokio::test]
    async fn mutable_authority_works_without_a_python_view_for_installed_and_candidate_plugins() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/stateful");
        let workspace = root.path().join("work");
        fs::create_dir_all(plugin.join("data")).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"Stateful","mutable":["data/"]}"#,
        )
        .unwrap();
        fs::write(plugin.join("code.txt"), "unchanged").unwrap();
        let catalog = Catalog::new(root.path());
        let binding = catalog.resolve(&["stateful".into()]).unwrap().remove(0);
        let prepared = prepare_plugins_async(
            vec![binding],
            Some(root.path().to_path_buf()),
            crate::tool_host::PluginToolHost::new(Vec::<String>::new()),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .unwrap();
        let installed = &prepared.views["stateful"];
        let candidate = PreparedPlugin::for_candidate(&catalog, "stateful", None).unwrap();
        assert!(installed.view().is_none());
        assert!(installed.declaration_digest().is_none());
        for authority in [installed, &candidate] {
            let mount = ReadOnlyInput::new(&plugin, "/plugins/stateful");
            let sandbox = Arc::new(
                BubblewrapSandbox::new(
                    BubblewrapPolicy::new("bwrap", ["sh"])
                        .authorize_workspace_root(&workspace)
                        .authorize_readonly_input_root(&plugin)
                        .authorize_readonly_destination_root("/plugins/stateful"),
                )
                .unwrap(),
            );
            let sandbox = authority.authorize(sandbox).unwrap();
            let mut request = SandboxRequest::new(
                &workspace,
                [
                    "sh",
                    "-c",
                    "printf mutable > /plugins/stateful/data/result; if printf forbidden > /plugins/stateful/code.txt 2>/dev/null; then exit 1; fi",
                ],
            );
            request.readonly_inputs.push(mount);
            authority.apply(&mut request);
            let result = sandbox.run(request).await.unwrap();
            assert_eq!(result.exit_code, Some(0), "{}", result.stderr);
            assert_eq!(
                fs::read_to_string(plugin.join("data/result")).unwrap(),
                "mutable"
            );
            assert_eq!(
                fs::read_to_string(plugin.join("code.txt")).unwrap(),
                "unchanged"
            );
        }
    }

    /// A view whose single pointer names a stored (package, version).
    fn fixture(root: &Path) -> (PathBuf, PathBuf) {
        let store = store_root(root).join("probe").join("1.2.3");
        fs::create_dir_all(store.join("probe")).unwrap();
        fs::write(store.join("probe/__init__.py"), "__version__ = '1.2.3'\n").unwrap();
        let view = root.join("views/demo/abc");
        let site = view.join("lib/python3.12/site-packages");
        fs::create_dir_all(&site).unwrap();
        fs::write(view.join(VIEW_CONFIGURATION), "home = /usr/bin\n").unwrap();
        fs::create_dir_all(view.join("bin")).unwrap();
        std::os::unix::fs::symlink("/usr/bin/python3.12", view.join("bin/python")).unwrap();
        std::os::unix::fs::symlink(store.join("probe"), site.join("probe")).unwrap();
        (view, store)
    }

    #[test]
    fn a_view_mounts_the_store_directories_it_points_at() {
        let root = tempfile::tempdir().unwrap();
        let (view, store) = fixture(root.path());
        let directories = referenced_store_directories(&view, &store_root(root.path())).unwrap();
        assert_eq!(directories, vec![store.clone()]);
        // The view's own seeded files grant nothing.
        fs::write(
            view.join("lib/python3.12/site-packages/_virtualenv.py"),
            "# seeded\n",
        )
        .unwrap();
        assert_eq!(
            referenced_store_directories(&view, &store_root(root.path())).unwrap(),
            vec![store]
        );
    }

    #[test]
    fn a_pointer_outside_the_store_or_at_missing_bytes_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let (view, _store) = fixture(root.path());
        let site = view.join("lib/python3.12/site-packages");

        // Outside the store: a view must not widen what a server may read.
        let outside = root.path().join("elsewhere");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, site.join("outside")).unwrap();
        let error = referenced_store_directories(&view, &store_root(root.path())).unwrap_err();
        assert!(
            error.contains("outside the shared dependency store"),
            "{error}"
        );
        fs::remove_file(site.join("outside")).unwrap();

        // A dangling pointer names bytes the Run would have used and cannot.
        std::os::unix::fs::symlink(
            store_root(root.path()).join("gone/0.0.1/gone"),
            site.join("gone"),
        )
        .unwrap();
        let error = referenced_store_directories(&view, &store_root(root.path())).unwrap_err();
        assert!(error.contains("does not exist"), "{error}");
    }

    #[test]
    fn a_declared_view_that_is_missing_or_not_an_environment_refuses() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/probe");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"Probe","dependencies":{"python":["idna==3.6"]}}"#,
        )
        .unwrap();
        let catalog = Catalog::new(root.path());
        let binding = catalog.resolve(&["probe".into()]).unwrap().remove(0);

        // Nothing installed under the declared view yet.
        let unusable = prepare(root.path(), &binding).unwrap_err();
        assert!(
            matches!(unusable, Unusable::Recorded(ref reason) if reason.contains("is missing")),
            "{unusable:?}"
        );

        // A directory that is not an environment is not one either.
        let view = catalog.view_path("probe").unwrap().unwrap();
        fs::create_dir_all(&view).unwrap();
        let unusable = prepare(root.path(), &binding).unwrap_err();
        assert!(
            matches!(unusable, Unusable::Recorded(ref reason) if reason.contains(VIEW_CONFIGURATION)),
            "{unusable:?}"
        );
        // The class a Run sees: this is a fact the Run recorded, not a missing
        // deployment capability.
        assert!(matches!(
            unusable.into_graph_error(),
            anchor_runtime::graph::GraphError::CorruptRun(_)
        ));

        // A missing Library Plugin is an unavailable authority, not a Plugin
        // with an empty dependency declaration.
        let mut elsewhere = binding.clone();
        elsewhere.id = "not-installed-here".to_owned();
        assert!(prepare(root.path(), &elsewhere).is_err());

        // A Plugin that declares nothing needs no view at all.
        let plain = root.path().join("plugins/plain");
        fs::create_dir_all(&plain).unwrap();
        fs::write(plain.join("plugin.json"), r#"{"name":"Plain"}"#).unwrap();
        let binding = catalog.resolve(&["plain".into()]).unwrap().remove(0);
        assert!(prepare(root.path(), &binding).unwrap().is_none());
    }

    /// A digest the Run recorded no longer matching the installed code is not a
    /// refusal: the Run uses the *current* Plugin (standard invariant 6), and the
    /// view it actually used is recorded as an observation instead.
    #[test]
    fn a_changed_digest_binds_to_the_current_plugin_and_its_current_view() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/probe");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"Probe","dependencies":{"python":["idna==3.6"]}}"#,
        )
        .unwrap();
        let catalog = Catalog::new(root.path());
        let mut binding = catalog.resolve(&["probe".into()]).unwrap().remove(0);
        binding.digest = "b".repeat(64);
        // Nothing about the view itself changed, so the only failure left is that
        // the view has not been built yet — never "the Plugin changed".
        let unusable = prepare(root.path(), &binding).unwrap_err();
        assert!(
            matches!(unusable, Unusable::Recorded(ref reason) if reason.contains("is missing")),
            "{unusable:?}"
        );
        // With the view in place the changed digest prepares normally.
        let root_path = root.path().canonicalize().unwrap();
        let declaration = catalog
            .definition("probe")
            .unwrap()
            .dependencies
            .declaration_digest();
        let store = anchor_graph_host::store_root(&root_path);
        std::fs::create_dir_all(store.join("idna/3.6/idna")).unwrap();
        let view = anchor_graph_host::plugin_view_path(&root_path, "probe", &declaration);
        let site = view.join("lib/python3.12/site-packages");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::create_dir_all(view.join("bin")).unwrap();
        fs::write(view.join(VIEW_CONFIGURATION), "home = /usr/bin").unwrap();
        std::os::unix::fs::symlink("/bin/sh", view.join("bin/python")).unwrap();
        std::os::unix::fs::symlink(store.join("idna/3.6/idna"), site.join("idna")).unwrap();
        let prepared = prepare(root.path(), &binding)
            .unwrap()
            .expect("the current declaration's view");
        assert_eq!(
            prepared.declaration_digest(),
            catalog
                .definition("probe")
                .unwrap()
                .dependencies
                .declaration_digest()
        );
    }
}
