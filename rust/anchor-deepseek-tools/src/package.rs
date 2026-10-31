use rustix::fs::{Mode, OFlags, RenameFlags, open, renameat_with};
use std::{
    fs::{self, File},
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
};

pub fn package_plugin(destination: impl AsRef<Path>) -> io::Result<()> {
    let destination = std::path::absolute(destination)?;
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("destination has no parent"))?;
    let name = destination
        .file_name()
        .ok_or_else(|| io::Error::other("destination has no name"))?;
    let directory = File::from(open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let pinned = std::path::PathBuf::from(format!(
        "/proc/self/fd/{}",
        std::os::fd::AsRawFd::as_raw_fd(&directory)
    ));
    let staging = tempfile::Builder::new()
        .prefix(".anchor-deepseek-")
        .tempdir_in(&pinned)?;
    fs::create_dir_all(staging.path().join("bin"))?;
    fs::create_dir_all(staging.path().join("skills/deepseek-web"))?;
    for (path, bytes) in [
        (
            "plugin.json",
            include_bytes!("../../../plugins/deepseek-web/plugin.json").as_slice(),
        ),
        (
            "skills/deepseek-web/SKILL.md",
            include_bytes!("../../../plugins/deepseek-web/skills/deepseek-web/SKILL.md").as_slice(),
        ),
    ] {
        let mut file = File::create(staging.path().join(path))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    let mut executable = File::open("/proc/self/exe")?;
    let mut binary = File::create(staging.path().join("bin/anchor-deepseek-tools"))?;
    io::copy(&mut executable, &mut binary)?;
    binary.set_permissions(fs::Permissions::from_mode(0o755))?;
    binary.sync_all()?;
    for path in ["bin", "skills/deepseek-web", "skills", ""] {
        File::open(staging.path().join(path))?.sync_all()?;
    }
    renameat_with(
        &directory,
        staging.path().file_name().unwrap(),
        &directory,
        name,
        RenameFlags::NOREPLACE,
    )?;
    directory.sync_all()?;
    let _ = staging.keep();
    Ok(())
}
