#!/usr/bin/env bash
# Native delivery wraps the existing admitted platform, not another Runtime.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
  cat <<'HELP'
Usage: scripts/build-native-package.sh --goose PINNED_ELF --output NEW.rpm|NEW.deb
       [--build-root DIRECTORY]
       [--release RPM_RELEASE] [--rpmbuild EXECUTABLE]
       [--revision DEB_REVISION] [--dpkg-deb EXECUTABLE]
       [--skip-build]

Build the source-free platform and its native CLI, then create a fresh-install
Linux x86_64 package, selected by the output suffix. Uses the existing platform
admission and systemd service. Requires rpmbuild (RPM) or dpkg-deb (DEB), plus
the platform builder's prerequisites; never installs packages or starts the Host
on the build machine. Build on Rocky 8 for its libc compatibility baseline.
--skip-build reuses reviewed binaries and compiled WebUI.
HELP
}

goose=
output=
build_root="$repo/.local/platform-release"
release=1
rpmbuild=rpmbuild
revision=1
dpkg_deb=dpkg-deb
skip_build=()
while (($#)); do
  case "$1" in
    --goose) goose=${2:?--goose needs an ELF}; shift 2 ;;
    --output) output=${2:?--output needs a package}; shift 2 ;;
    --build-root) build_root=${2:?--build-root needs a directory}; shift 2 ;;
    --release) release=${2:?--release needs an identifier}; shift 2 ;;
    --rpmbuild) rpmbuild=${2:?--rpmbuild needs an executable}; shift 2 ;;
    --revision) revision=${2:?--revision needs an identifier}; shift 2 ;;
    --dpkg-deb) dpkg_deb=${2:?--dpkg-deb needs an executable}; shift 2 ;;
    --skip-build) skip_build=(--skip-build); shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ -n "$goose" && -n "$output" ]] || { usage >&2; exit 2; }
case "$output" in
  *.rpm) builder=anchor-build-rpm; package_options=(--release "$release" --rpmbuild "$rpmbuild") ;;
  *.deb) builder=anchor-build-deb; package_options=(--revision "$revision" --dpkg-deb "$dpkg_deb") ;;
  *) printf 'Output must end in .rpm or .deb.\n' >&2; exit 2 ;;
esac
[[ ! -e "$output" && ! -L "$output" ]] || { printf 'Output already exists.\n' >&2; exit 1; }
build_root=$(realpath -m -- "$build_root")
mkdir -p -- "$build_root"
inputs=$(mktemp -d "$build_root/native-inputs.XXXXXXXX")
trap 'rm -rf -- "$inputs"' EXIT
"$repo/scripts/build-platform-release.sh" --goose "$goose" \
  --output "$inputs/platform.tar.gz" --build-root "$build_root" "${skip_build[@]}"
cp -- "$build_root/target/release/anchor" "$inputs/anchor"
sha256=$(sha256sum -- "$inputs/platform.tar.gz")
"$build_root/target/release/$builder" \
  --archive "$inputs/platform.tar.gz" --sha256 "${sha256%% *}" \
  --cli "$inputs/anchor" "${package_options[@]}" --output "$output"
