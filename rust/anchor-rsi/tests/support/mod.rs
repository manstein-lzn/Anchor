use anchor_runtime::artifact::ARTIFACT_DESCRIPTOR_FILE;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};

pub fn artifact(root: &Path, node: &str, invocation: u64, selected: &[&str]) -> Value {
    let mut files = BTreeMap::new();
    let mut directories = Vec::new();
    for entry in selected
        .iter()
        .flat_map(|name| walkdir::WalkDir::new(root.join(name)).sort_by_file_name())
    {
        let entry = entry.unwrap();
        let name = entry.path().strip_prefix(root).unwrap().to_str().unwrap();
        if entry.file_type().is_dir() {
            directories.push(name.to_owned());
        } else {
            let bytes = fs::read(entry.path()).unwrap();
            files.insert(name.to_owned(), json!({"sha256":format!("{:x}", Sha256::digest(&bytes)),"bytes":bytes.len(),"executable":false}));
        }
    }
    directories.sort();
    directories.dedup();
    let manifest = serde_json::to_vec(
        &json!({"node":node,"invocation":invocation,"files":files,"directories":directories}),
    )
    .unwrap();
    let digest = format!("{:x}", Sha256::digest(&manifest));
    let descriptor = json!({"format":1,"artifact":{"id":format!("fs2-{digest}"),"node_id":node,"invocation":invocation},"manifest_sha256":digest,"files":files,"directories":directories});
    fs::write(
        root.join(ARTIFACT_DESCRIPTOR_FILE),
        serde_json::to_vec_pretty(&descriptor).unwrap(),
    )
    .unwrap();
    descriptor
}
