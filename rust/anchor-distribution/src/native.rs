//! Shared admitted payload for native package formats; mutable facts never ship.
use crate::{
    PackageError, PlatformInstallRequest, Result,
    filesystem::{Snapshot, make_directories_writable},
    install_platform, validation,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const SERVICE: &str = include_str!("../../../deploy/systemd/anchor.service");
const LAUNCHER: &str = "/var/lib/anchor/platform/release/anchor-runtime/bin/anchor-platform";

pub(crate) struct NativePayload {
    pub(crate) root: PathBuf,
    pub(crate) payload: PathBuf,
    pub(crate) archive_sha256: String,
    cli: Snapshot,
}

impl NativePayload {
    pub(crate) fn verify(&self) -> Result<()> {
        self.cli.verify()
    }

    pub(crate) fn cli_sha256(&self) -> &str {
        &self
            .cli
            .resources()
            .next()
            .expect("single-file snapshot")
            .sha256
    }
}

pub(crate) fn with_payload<T>(
    archive: &Path,
    sha256: &str,
    cli: &Path,
    build: impl FnOnce(&NativePayload) -> Result<T>,
) -> Result<T> {
    let staging = tempfile::Builder::new()
        .prefix("anchor-native-")
        .tempdir()?;
    let result = prepare(staging.path(), archive, sha256, cli).and_then(|payload| build(&payload));
    // Admission makes release directories read-only. Restore only private
    // staging directories so TempDir can clean up, including failed builds.
    let _ = make_directories_writable(staging.path());
    result
}

fn prepare(root: &Path, archive: &Path, sha256: &str, cli: &Path) -> Result<NativePayload> {
    let installed = install_platform(&PlatformInstallRequest {
        archive: archive.into(),
        sha256: sha256.into(),
        install_root: root.join("admitted"),
        model_config: None,
    })?;
    let payload = root.join("payload");
    create_payload_directories(&payload)?;
    let platform = payload.join("var/lib/anchor/platform");
    let release = installed.install_root.join("release");
    // Reparenting updates '..' and requires owner write on the outer container;
    // admitted runtime bytes and subtree modes never change.
    fs::set_permissions(&release, fs::Permissions::from_mode(0o755))?;
    fs::rename(release, platform.join("release"))?;
    fs::set_permissions(platform.join("release"), fs::Permissions::from_mode(0o555))?;
    let cli = Snapshot::file(cli, &payload, "usr/bin/anchor")?;
    let identity = validation::elf(&payload.join("usr/bin/anchor"))?;
    if identity.architecture != "X86_64" || identity.bits != 64 || !identity.little_endian {
        return Err(PackageError::Invalid(
            "native packages require Linux x86_64 ELFs".into(),
        ));
    }
    let manifest: crate::PlatformPackageInventory = serde_json::from_reader(fs::File::open(
        platform.join("release/anchor-runtime/runtime-manifest.json"),
    )?)?;
    if manifest.platform != identity {
        return Err(PackageError::Invalid(
            "native CLI and platform architectures differ".into(),
        ));
    }
    write_resource(
        &payload,
        "usr/lib/systemd/system/anchor.service",
        &native_service()?,
    )?;
    write_resource(
        &payload,
        "usr/share/applications/anchor.desktop",
        "[Desktop Entry]\nType=Application\nName=Anchor\nComment=Open the local Anchor WebUI\nExec=/usr/bin/anchor open\nTerminal=false\nCategories=Development;\n",
    )?;
    Ok(NativePayload {
        root: root.into(),
        payload,
        archive_sha256: installed.archive_sha256,
        cli,
    })
}

fn write_resource(root: &Path, relative: &str, content: &str) -> Result<()> {
    let path = root.join(relative);
    fs::write(&path, content)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    Ok(())
}

fn create_payload_directories(payload: &Path) -> Result<()> {
    // Package ownership normalization does not normalize modes or override umask.
    for relative in [
        "",
        "var",
        "var/lib",
        "var/lib/anchor",
        "var/lib/anchor/platform",
        "usr",
        "usr/bin",
        "usr/lib",
        "usr/lib/systemd",
        "usr/lib/systemd/system",
        "usr/share",
        "usr/share/applications",
    ] {
        let path = payload.join(relative);
        fs::create_dir(&path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn native_service() -> Result<String> {
    let original = format!("ExecStart={LAUNCHER} start\n");
    if SERVICE.matches(&original).count() != 1 {
        return Err(PackageError::Invalid(
            "systemd service launcher contract changed".into(),
        ));
    }
    // Standard /usr/bin SELinux labeling admits the service entrypoint; the
    // existing launcher remains the only runtime configuration owner.
    Ok(SERVICE.replacen(&original, "ExecStart=/usr/bin/anchor serve\n", 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_payload_integration_directories_have_fixed_modes() {
        let root = tempfile::tempdir().unwrap();
        let payload = root.path().join("payload");
        create_payload_directories(&payload).unwrap();
        let mut directories = vec![payload];
        while let Some(directory) = directories.pop() {
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o7777,
                0o755
            );
            for entry in fs::read_dir(directory).unwrap() {
                directories.push(entry.unwrap().path());
            }
        }
    }

    #[test]
    fn native_service_changes_only_the_entrypoint_and_keeps_the_service_boundary() {
        let native = native_service().unwrap();
        assert_eq!(
            native.replace(
                "ExecStart=/usr/bin/anchor serve\n",
                &format!("ExecStart={LAUNCHER} start\n")
            ),
            SERVICE
        );
        for setting in [
            "User=anchor\n",
            "Group=anchor\n",
            "NoNewPrivileges=true\n",
            "KillMode=control-group\n",
            "ReadOnlyPaths=/var/lib/anchor/platform/release\n",
            "ReadWritePaths=/var/lib/anchor/platform/data\n",
        ] {
            assert!(native.contains(setting), "{setting}");
        }
    }
}
