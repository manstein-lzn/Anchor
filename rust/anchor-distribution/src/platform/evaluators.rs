//! Protected suites share the release installer and cannot register caller paths.
use crate::{
    PackageError, ResourceFile, Result, filesystem::open_path, generated_file, validation,
};
use anchor_facts::evaluation::ProtectedSuite;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_SUITE_BYTES: usize = 256 * 1024;
const PACKAGES: &[(&str, &str, &[u8])] = &[(
    "input-discovery",
    "evaluators/input-discovery/manifest.json",
    include_bytes!("../../../../resources/evaluations/input-discovery/manifest.json"),
)];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformEvaluatorInventory {
    pub id: String,
    pub package_path: String,
    pub manifest_sha256: String,
    pub protocol_version: u32,
    pub suite_version: String,
}

/// Exact official bytes selected only after complete installed-release validation.
/// This capability is not constructed from protocol JSON or a supplied suite path.
#[derive(Clone)]
pub struct RegisteredEvaluationPackage {
    inventory: PlatformEvaluatorInventory,
    suite_path: PathBuf,
    suite_bytes: Vec<u8>,
}

impl std::fmt::Debug for RegisteredEvaluationPackage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredEvaluationPackage")
            .field("inventory", &self.inventory)
            .field("suite_path", &self.suite_path)
            .finish_non_exhaustive()
    }
}

impl RegisteredEvaluationPackage {
    pub fn inventory(&self) -> &PlatformEvaluatorInventory {
        &self.inventory
    }

    pub fn suite_path(&self) -> &Path {
        &self.suite_path
    }

    pub fn suite_bytes(&self) -> &[u8] {
        &self.suite_bytes
    }

    pub fn digest(&self) -> &str {
        &self.inventory.manifest_sha256
    }
}

pub(crate) fn release_evaluator_resources() -> impl Iterator<Item = (String, u32)> {
    PACKAGES.iter().map(|(_, path, _)| ((*path).into(), 0o644))
}

pub(crate) fn release_evaluator_resource_mode(path: &str) -> Option<u32> {
    release_evaluator_resources().find_map(|(file, mode)| (file == path).then_some(mode))
}

pub(crate) fn write_release_evaluators(
    root: &Path,
    entries: &mut BTreeMap<String, Option<ResourceFile>>,
) -> Result<Vec<PlatformEvaluatorInventory>> {
    for (_, path, bytes) in PACKAGES {
        let mut relative = *path;
        while let Some((parent, _)) = relative.rsplit_once('/') {
            fs::create_dir_all(root.join(parent))?;
            entries.insert(parent.into(), None);
            relative = parent;
        }
        generated_file(root, path, bytes, entries)?;
        validation::resource_content(&root.join(path), path)?;
    }
    release_evaluator_inventory(root)
}

fn official_inventory(id: &str, path: &str, bytes: &[u8]) -> Result<PlatformEvaluatorInventory> {
    if bytes.len() > MAX_SUITE_BYTES {
        return Err(PackageError::Invalid(
            "protected evaluator suite exceeds 256 KiB".into(),
        ));
    }
    let suite: ProtectedSuite = serde_json::from_slice(bytes)?;
    suite
        .validate()
        .map_err(|error| PackageError::Invalid(format!("protected evaluator suite: {error}")))?;
    if suite.id != id {
        return Err(PackageError::Invalid(
            "protected evaluator package and suite identity differ".into(),
        ));
    }
    let parent = Path::new(path)
        .parent()
        .and_then(Path::to_str)
        .ok_or_else(|| PackageError::Invalid("protected evaluator has no package root".into()))?;
    Ok(PlatformEvaluatorInventory {
        id: id.into(),
        package_path: parent.into(),
        manifest_sha256: format!("{:x}", Sha256::digest(bytes)),
        protocol_version: 1,
        suite_version: suite.version.to_string(),
    })
}

pub(crate) fn validate_evaluator_inventory(
    evaluators: &[PlatformEvaluatorInventory],
) -> Result<()> {
    let official = PACKAGES
        .iter()
        .map(|(id, path, bytes)| official_inventory(id, path, bytes))
        .collect::<Result<Vec<_>>>()?;
    if evaluators != official {
        return Err(PackageError::Invalid(
            "platform evaluators differ from the official protected packages".into(),
        ));
    }
    Ok(())
}

pub(crate) fn release_evaluator_inventory(root: &Path) -> Result<Vec<PlatformEvaluatorInventory>> {
    PACKAGES
        .iter()
        .map(|(id, path, expected)| {
            // Installer staging roots are already enumerated and pinned below
            // /proc/self/fd; only the public selector accepts an external path.
            read_official_suite(fs::File::open(root.join(path))?, expected)?;
            official_inventory(id, path, expected)
        })
        .collect()
}

fn read_official_suite(file: fs::File, expected: &[u8]) -> Result<Vec<u8>> {
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_SUITE_BYTES as u64 {
        return Err(PackageError::Invalid(
            "protected evaluator suite must be an ordinary file no larger than 256 KiB".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_SUITE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes != expected {
        return Err(PackageError::Invalid(
            "platform evaluator resource differs from official bytes".into(),
        ));
    }
    Ok(bytes)
}

pub(crate) fn registered_evaluation_package(
    runtime: &Path,
    evaluators: &[PlatformEvaluatorInventory],
    id: &str,
) -> Result<RegisteredEvaluationPackage> {
    let (official_id, path, expected) = PACKAGES
        .iter()
        .find(|(official_id, _, _)| *official_id == id)
        .ok_or_else(|| PackageError::Invalid("unregistered protected evaluator package".into()))?;
    let official = official_inventory(official_id, path, expected)?;
    if !evaluators.iter().any(|entry| entry == &official) {
        return Err(PackageError::Invalid(
            "protected evaluator is absent from the admitted release".into(),
        ));
    }
    let suite_path = runtime.join(path);
    let suite_bytes = read_official_suite(open_path(&suite_path)?, expected)?;
    Ok(RegisteredEvaluationPackage {
        inventory: official,
        suite_path,
        suite_bytes,
    })
}
