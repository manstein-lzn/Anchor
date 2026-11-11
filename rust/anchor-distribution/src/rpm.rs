//! Native packaging reuses platform admission; RPM owns release bytes, not state.
use crate::{
    PackageError, Result, archive,
    filesystem::{Output, open_path},
    native::{NativePayload, with_payload},
    validation,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, Clone)]
pub struct NativeRpmRequest {
    pub archive: PathBuf,
    pub sha256: String,
    pub cli: PathBuf,
    pub release: String,
    pub rpmbuild: PathBuf,
    pub output: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeRpmReport {
    pub output: PathBuf,
    pub sha256: String,
    pub platform_archive_sha256: String,
    pub cli_sha256: String,
    pub version: String,
    pub release: String,
    pub architecture: String,
}

pub fn build_native_rpm(request: &NativeRpmRequest) -> Result<NativeRpmReport> {
    let output = Output::with_extension(&request.output, ".rpm")?;
    let spec = specification(&request.release)?;
    with_payload(&request.archive, &request.sha256, &request.cli, |payload| {
        build(request, output, &spec, payload)
    })
}

fn build(
    request: &NativeRpmRequest,
    output: Output,
    spec: &str,
    payload: &NativePayload,
) -> Result<NativeRpmReport> {
    let root = &payload.root;
    for directory in ["BUILD", "BUILDROOT", "RPMS", "SOURCES", "SPECS", "SRPMS"] {
        fs::create_dir(root.join(directory))?;
    }
    let spec_path = root.join("SPECS/anchor.spec");
    fs::write(&spec_path, spec)?;
    // rpmbuild interprets macro definitions; a build path must not inject one.
    let topdir = rpm_build_path(root)?;
    let status = Command::new(&request.rpmbuild)
        .args([
            "--define",
            &format!("_topdir {topdir}"),
            "--define",
            &format!("_tmppath {topdir}"),
        ])
        .arg("-bb")
        .arg(&spec_path)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        return Err(PackageError::Invalid(format!(
            "rpmbuild failed with {status}"
        )));
    }
    payload.verify()?;
    let rpm_path = root.join(format!(
        "RPMS/x86_64/anchor-{}-{}.x86_64.rpm",
        env!("CARGO_PKG_VERSION"),
        request.release,
    ));
    let mut rpm = open_path(&rpm_path)?;
    if !rpm.metadata()?.is_file() {
        return Err(PackageError::Invalid(
            "rpmbuild did not produce an ordinary RPM".into(),
        ));
    }
    let sha256 = archive::sha256(&mut rpm)?;
    use std::io::{Seek, SeekFrom};
    rpm.seek(SeekFrom::Start(0))?;
    output.publish(&mut rpm)?;
    Ok(NativeRpmReport {
        output: output.path,
        sha256,
        platform_archive_sha256: payload.archive_sha256.clone(),
        cli_sha256: payload.cli_sha256().into(),
        version: env!("CARGO_PKG_VERSION").into(),
        release: request.release.clone(),
        architecture: "x86_64".into(),
    })
}

fn rpm_build_path(path: &Path) -> Result<&str> {
    path.to_str()
        .filter(|path| {
            path.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
        })
        .ok_or_else(|| {
            PackageError::Invalid("RPM build path contains shell or macro syntax".into())
        })
}

fn specification(release: &str) -> Result<String> {
    validation::version(env!("CARGO_PKG_VERSION"))?;
    validation::safe_component(release)?;
    if !release.as_bytes()[0].is_ascii_alphanumeric() || release.contains('-') {
        return Err(PackageError::Invalid(
            "RPM release must be alphanumeric with dots or underscores".into(),
        ));
    }
    let mut spec = format!(
        r#"%global debug_package %{{nil}}
%global _build_id_links none
# Stripping or other byte rewriting would invalidate the admitted inventory.
%global __os_install_post %{{nil}}
Name: anchor
Version: {version}
Release: {release}
Summary: Anchor local Graph and Plugin runtime with Pilot and WebUI
License: MIT
BuildArch: x86_64
Requires: bubblewrap
Requires: slirp4netns
Requires: git
Requires: ca-certificates
Requires: gcc
Requires: systemd
Requires(pre): glibc-common
Requires(post): shadow-utils
Requires(post): coreutils
Requires(post): util-linux
Requires(post): systemd
Requires(preun): systemd
Requires(postun): systemd

%description
Source-free Anchor platform with fixed Goose, local WebUI and isolated runtime.
Configure model services in the WebUI after a fresh installation.
Business Graphs and third-party Plugins are distributed separately.
Existing installations and in-place upgrades are not supported by this package.

%prep
%build
%install
mkdir -p "%{{buildroot}}"
cp -a "%{{_topdir}}/payload/." "%{{buildroot}}/"

%clean
chmod -R u+w "%{{buildroot}}"
rm -rf "%{{buildroot}}"

%files
%defattr(-,root,root,-)
%attr(0755,root,root) %dir /var/lib/anchor
%attr(0755,root,root) %dir /var/lib/anchor/platform
/var/lib/anchor/platform/release
/usr/bin/anchor
/usr/lib/systemd/system/anchor.service
/usr/share/applications/anchor.desktop
"#,
        version = env!("CARGO_PKG_VERSION"),
    );
    for (section, body) in [
        ("pre", include_str!("../../../deploy/native/pre.sh")),
        ("post", include_str!("../../../deploy/native/post.sh")),
        ("preun", include_str!("../../../deploy/native/preun.sh")),
        ("postun", include_str!("../../../deploy/native/postun.sh")),
    ] {
        spec.push_str(&format!("\n%{section}\n{}", body.replace('%', "%%")));
    }
    Ok(spec)
}

#[cfg(test)]
mod tests;
