use std::{
    fs,
    path::{Path, PathBuf},
};

use anchor_library::{Checkout, GithubSource, InstallError, InstallRequest};
use serde_json::{Value, json};

pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

pub fn fixture(root: &Path, label: &str, hidden: bool) -> PathBuf {
    let directory = root.join(label);
    let manifest = if hidden {
        ".codex-plugin/plugin.json"
    } else {
        "plugin.json"
    };
    write(
        directory.join(manifest),
        json!({
            "name": label,
            "description": "operator fixture",
            "skills": "skills",
            "mcpServers": {"probe": {"command": "fixture-never-executed", "args": []}}
        })
        .to_string(),
    );
    write(
        directory.join("skills/check/SKILL.md"),
        format!("# {label}\nOnly fixture content.\n"),
    );
    write(directory.join("resources/evidence.txt"), label);
    directory
}

pub fn request(id: Option<&str>, replace_existing: bool) -> InstallRequest {
    InstallRequest {
        source: "https://github.com/owner/repository/tree/main/plugins/demo".to_owned(),
        id: id.map(str::to_owned),
        replace_existing,
    }
}

pub struct FixtureCheckout;

impl Checkout for FixtureCheckout {
    fn checkout(&self, source: &GithubSource, destination: &Path) -> Result<(), InstallError> {
        let source_dir = destination.join(source.relative_path());
        write(
            source_dir.join(".codex-plugin/plugin.json"),
            json!({"name": "Checkout fixture"}).to_string(),
        );
        write(
            source_dir.join("skills/check/SKILL.md"),
            "checkout fixture fact",
        );
        Ok(())
    }
}

pub fn graph() -> Value {
    json!({
        "objective": "installer fixture",
        "entry": "worker",
        "agents": {"worker": {"model": "unused-fixture", "instructions": "read fixture"}},
        "ops": {},
        "nodes": [{"id": "worker", "agent": "worker", "plugins": ["demo"]}],
        "edges": []
    })
}
