//! Debian metadata wraps the shared admitted payload; it owns no runtime rules.
use crate::{
    PackageError, Result, archive,
    filesystem::{Output, open_path},
    native::with_payload,
    validation,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Seek, SeekFrom},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, Clone)]
pub struct NativeDebRequest {
    pub archive: PathBuf,
    pub sha256: String,
    pub cli: PathBuf,
    pub revision: String,
    pub dpkg_deb: PathBuf,
    pub output: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeDebReport {
    pub output: PathBuf,
    pub sha256: String,
    pub platform_archive_sha256: String,
    pub cli_sha256: String,
    pub version: String,
    pub architecture: String,
}

pub fn build_native_deb(request: &NativeDebRequest) -> Result<NativeDebReport> {
    let output = Output::with_extension(&request.output, ".deb")?;
    let version = package_version(&request.revision)?;
    with_payload(&request.archive, &request.sha256, &request.cli, |payload| {
        write_metadata(&payload.payload, &version)?;
        let package = payload.root.join("anchor.deb");
        package_payload(&request.dpkg_deb, &payload.payload, &package)?;
        let mut package = open_path(&package)?;
        if !package.metadata()?.is_file() {
            return Err(PackageError::Invalid(
                "dpkg-deb did not produce an ordinary DEB".into(),
            ));
        }
        let sha256 = archive::sha256(&mut package)?;
        package.seek(SeekFrom::Start(0))?;
        payload.verify()?;
        output.publish(&mut package)?;
        Ok(NativeDebReport {
            output: output.path,
            sha256,
            platform_archive_sha256: payload.archive_sha256.clone(),
            cli_sha256: payload.cli_sha256().into(),
            version,
            architecture: "amd64".into(),
        })
    })
}

fn package_version(revision: &str) -> Result<String> {
    validation::version(env!("CARGO_PKG_VERSION"))?;
    validation::safe_component(revision)?;
    if !revision.as_bytes()[0].is_ascii_alphanumeric()
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
    {
        return Err(PackageError::Invalid(
            "Debian revision must start with an alphanumeric character and contain only alphanumerics or dots".into(),
        ));
    }
    Ok(format!("{}-{revision}", env!("CARGO_PKG_VERSION")))
}

fn control(version: &str) -> String {
    format!(
        r#"Package: anchor
Version: {version}
Section: utils
Priority: optional
Architecture: amd64
Maintainer: Anchor maintainers
Depends: libc6 (>= 2.28), libc6-dev, libc-bin, libgcc-s1, bubblewrap, slirp4netns, git, ca-certificates, gcc, systemd, passwd, coreutils, util-linux
Description: Anchor local Graph and Plugin runtime with Pilot and WebUI
 Source-free Anchor platform with fixed Goose and an isolated runtime.
 Configure model services in the WebUI after a fresh installation.
 Business Graphs and third-party Plugins are distributed separately.
 Existing installations and in-place upgrades are not supported.
"#
    )
}

fn write_metadata(payload: &Path, version: &str) -> Result<()> {
    let metadata = payload.join("DEBIAN");
    fs::create_dir(&metadata)?;
    fs::set_permissions(&metadata, fs::Permissions::from_mode(0o755))?;
    let path = metadata.join("control");
    fs::write(&path, control(version))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    for (name, prefix, body) in MAINTAINER_SCRIPTS {
        let path = metadata.join(name);
        fs::write(&path, format!("{prefix}\n{body}"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

const MAINTAINER_SCRIPTS: &[(&str, &str, &str)] = &[
    (
        "preinst",
        include_str!("../../../deploy/deb/preinst.sh"),
        include_str!("../../../deploy/native/pre.sh"),
    ),
    (
        "postinst",
        include_str!("../../../deploy/deb/postinst.sh"),
        include_str!("../../../deploy/native/post.sh"),
    ),
    (
        "prerm",
        include_str!("../../../deploy/deb/prerm.sh"),
        include_str!("../../../deploy/native/preun.sh"),
    ),
    (
        "postrm",
        include_str!("../../../deploy/deb/postrm.sh"),
        include_str!("../../../deploy/native/postun.sh"),
    ),
];

fn package_payload(builder: &Path, payload: &Path, output: &Path) -> Result<()> {
    let status = Command::new(builder)
        .args(["--root-owner-group", "--build"])
        .arg(payload)
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        return Err(PackageError::Invalid(format!(
            "dpkg-deb failed with {status}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
