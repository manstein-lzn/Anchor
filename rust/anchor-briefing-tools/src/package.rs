use std::{
    fs::{self, File},
    io,
    os::unix::fs::PermissionsExt,
    path::Path,
};

pub fn package_plugin(destination: &Path, typst: &Path) -> io::Result<()> {
    if destination.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "candidate already exists",
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("destination needs a parent"))?;
    let staging = tempfile::Builder::new()
        .prefix(".briefing-candidate-")
        .tempdir_in(parent)?;
    for path in ["bin", "skills/ai-morning-briefing", "templates"] {
        fs::create_dir_all(staging.path().join(path))?;
    }
    for (path, bytes) in [
        (
            "plugin.json",
            include_bytes!("../../../plugins/ai-morning-briefing/plugin.json").as_slice(),
        ),
        (
            "skills/ai-morning-briefing/SKILL.md",
            include_bytes!(
                "../../../plugins/ai-morning-briefing/skills/ai-morning-briefing/SKILL.md"
            )
            .as_slice(),
        ),
        (
            "skills/ai-morning-briefing/sources.json",
            include_bytes!(
                "../../../plugins/ai-morning-briefing/skills/ai-morning-briefing/sources.json"
            )
            .as_slice(),
        ),
        (
            "templates/morning.typ",
            include_bytes!("../../../plugins/ai-morning-briefing/templates/morning.typ").as_slice(),
        ),
    ] {
        fs::write(staging.path().join(path), bytes)?;
    }
    fs::copy(
        std::env::current_exe()?,
        staging.path().join("bin/anchor-briefing-tools"),
    )?;
    fs::copy(typst, staging.path().join("bin/typst"))?;
    for path in ["bin/anchor-briefing-tools", "bin/typst"] {
        fs::set_permissions(staging.path().join(path), fs::Permissions::from_mode(0o755))?;
    }
    File::open(staging.path())?.sync_all()?;
    fs::rename(staging.path(), destination)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
