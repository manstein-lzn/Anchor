//! Fences: the hub depends only on neutral read/projection primitives,
//! nothing depends on it, it never writes a data root, and its layers depend
//! only downwards with all IO at the edges.

use std::path::{Path, PathBuf};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sources(dir: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let name = path
                    .strip_prefix(crate_dir().join("src"))
                    .unwrap()
                    .display()
                    .to_string();
                found.push((name, std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    found.sort();
    found
}

/// The dependency names of a `[dependencies]` table.
fn dependencies(manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[dependencies]";
        } else if inside && let Some((name, _)) = line.split_once('=') {
            names.push(name.trim().split('.').next().unwrap().to_owned());
        }
    }
    names.sort();
    names
}

#[test]
fn the_hub_depends_only_on_neutral_reading_primitives_and_nothing_depends_on_it() {
    let manifest = std::fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    assert_eq!(
        dependencies(&manifest),
        vec![
            "anchor-facts",
            "anchor-file-lease",
            "anchor-kernel",
            "serde",
            "serde_json",
            "sha2"
        ]
    );
    let workspace = crate_dir().parent().unwrap().to_path_buf();
    let mut dependents = Vec::new();
    for entry in std::fs::read_dir(&workspace).unwrap() {
        let manifest = entry.unwrap().path().join("Cargo.toml");
        if manifest.starts_with(crate_dir()) || !manifest.is_file() {
            continue;
        }
        if std::fs::read_to_string(&manifest)
            .unwrap()
            .contains("anchor-hub")
        {
            dependents.push(manifest.display().to_string());
        }
    }
    assert!(dependents.is_empty(), "{dependents:?}");
}

#[test]
fn the_hub_has_no_path_that_writes_a_data_root() {
    let writes = [
        "fs::write",
        "File::create",
        "OpenOptions",
        "create_dir",
        "remove_file",
        "remove_dir",
        "fs::rename",
        "fs::copy",
        "set_permissions",
        "FileStore",
        "Kernel<",
        "Kernel::",
        ".submit(",
        ".commit(",
        ".create(",
    ];
    for (name, text) in sources(&crate_dir().join("src")) {
        for needle in writes {
            assert!(!text.contains(needle), "{name} contains `{needle}`");
        }
    }
}

#[test]
fn layers_depend_only_downwards_and_only_the_edges_do_io() {
    let io = [
        "std::fs",
        "std::net",
        "std::process",
        "std::env",
        "std::io",
        "std::thread",
        "SystemTime",
        "println!",
        "eprintln!",
    ];
    // What each module may not name. L0 knows no layer above it; L1 knows
    // only L0; L2 knows only L1; L3 reaches the data through L2 only.
    let rules: [(&str, &[&str], bool); 4] = [
        (
            "source.rs",
            &[
                "crate::table",
                "crate::query",
                "crate::analyzers",
                "crate::adapters",
            ],
            false,
        ),
        (
            "table.rs",
            &["crate::query", "crate::analyzers", "crate::adapters"],
            true,
        ),
        (
            "query.rs",
            &[
                "crate::source",
                "crate::analyzers",
                "crate::adapters",
                "Projector",
                "FactTable",
            ],
            true,
        ),
        (
            "analyzers",
            &["crate::source", "crate::table", "crate::adapters"],
            true,
        ),
    ];
    let files = sources(&crate_dir().join("src"));
    for (module, forbidden, pure) in rules {
        let matching: Vec<_> = files
            .iter()
            .filter(|(name, _)| name.starts_with(module))
            .collect();
        assert!(!matching.is_empty(), "no files for {module}");
        for (name, text) in matching {
            for needle in forbidden
                .iter()
                .chain(if pure { io.as_slice() } else { &[] })
            {
                assert!(!text.contains(needle), "{name} contains `{needle}`");
            }
        }
    }
}
