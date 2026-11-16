//! A finite official advisory slot, delivered and checked like other release data.
use crate::{PackageError, ResourceFile, Result, generated_file};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};

pub(crate) const PATH: &str = "harness/input-discovery.md";
const CONTENT: &[u8] = include_bytes!("../../../../resources/harness/input-discovery.md");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformGuidanceInventory {
    pub slot: String,
    pub path: String,
    pub sha256: String,
    pub schema_version: u32,
}

pub(crate) fn resources() -> impl Iterator<Item = (String, u32)> {
    [(PATH.into(), 0o644)].into_iter()
}

pub(crate) fn resource_mode(path: &str) -> Option<u32> {
    (path == PATH).then_some(0o644)
}

pub(crate) fn write(
    root: &Path,
    entries: &mut BTreeMap<String, Option<ResourceFile>>,
) -> Result<Vec<PlatformGuidanceInventory>> {
    fs::create_dir(root.join("harness"))?;
    entries.insert("harness".into(), None);
    generated_file(root, PATH, CONTENT, entries)?;
    inventory(root)
}

pub(crate) fn inventory(root: &Path) -> Result<Vec<PlatformGuidanceInventory>> {
    if fs::read(root.join(PATH))? != CONTENT {
        return Err(PackageError::Invalid(
            "platform guidance differs from official bytes".into(),
        ));
    }
    Ok(vec![PlatformGuidanceInventory {
        slot: "input-discovery".into(),
        path: PATH.into(),
        sha256: crate::archive::sha256(&mut fs::File::open(root.join(PATH))?)?,
        schema_version: 1,
    }])
}

pub(crate) fn validate(entries: &[PlatformGuidanceInventory]) -> Result<()> {
    if !matches!(entries, [entry] if entry.slot == "input-discovery" && entry.path == PATH && entry.schema_version == 1)
    {
        return Err(PackageError::Invalid(
            "platform guidance inventory requires the input-discovery slot".into(),
        ));
    }
    Ok(())
}
