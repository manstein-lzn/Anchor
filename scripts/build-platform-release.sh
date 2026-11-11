#!/usr/bin/env bash
# Build-time tooling only, with pinned uv, Rustup and pixi bootstrap tools.
# Never reads the developer .env or platform state.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
  cat <<'HELP'
Usage: scripts/build-platform-release.sh --goose PINNED_ELF --output NEW.tar.gz
       [--build-root DIRECTORY] [--skip-build]

Build Host, network relay, sandbox networking, native platform launcher,
network/Git/WeCom MCPs and Feishu/WeCom gateways plus WebUI, prepare fixed uv/Rustup/pixi tools, and create a format-3
platform archive without a business Graph. Does not run or deploy the Host.
--skip-build packages existing <build-root>/target/release binaries and WebUI.
Operator build requirements: Rust stable, Node/npm, curl, tar, coreutils.
HELP
}

goose=
output=
build_root="$repo/.local/platform-release"
skip_build=0
while (($#)); do
  case "$1" in
    --goose) goose=${2:?--goose needs an ELF}; shift 2 ;;
    --output) output=${2:?--output needs an archive}; shift 2 ;;
    --build-root) build_root=${2:?--build-root needs a directory}; shift 2 ;;
    --skip-build) skip_build=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ -n "$goose" && -n "$output" ]] || { usage >&2; exit 2; }
build_root=$(realpath -m -- "$build_root")
mkdir -p -- "$build_root"
target="$build_root/target"
if (( !skip_build )); then
  cargo +stable build --manifest-path "$repo/rust/Cargo.toml" --locked --release \
    --target-dir "$target" -p anchor-runner-host -p anchor-sandbox-bwrap \
    -p anchor-network-tools -p anchor-git-tools -p anchor-wecom-tools \
    -p anchor-feishu-gateway -p anchor-wecom-gateway -p anchor-distribution --bins
  npm --prefix "$repo/apps/web" ci
  npm --prefix "$repo/apps/web" run build
fi
if [[ ! -d "$build_root/bootstrap" ]]; then
  "$repo/scripts/prepare-platform-bootstrap.sh" --output "$build_root/bootstrap"
fi
inputs=$(mktemp -d "$build_root/package-inputs.XXXXXXXX")
trap 'rm -rf -- "$inputs"' EXIT
for binary in anchor-distribution anchor-runner-host anchor-net-relay anchor-sandbox-network anchor-platform anchor-network-tools anchor-git-tools anchor-wecom-tools anchor-feishu-gateway anchor-wecom-gateway; do
  # Cargo output may be hardlinked to target/deps; packaging admits independent
  # ordinary copies and records precisely these bytes.
  cp -- "$target/release/$binary" "$inputs/$binary"
done
"$target/release/anchor-distribution" --platform \
  --host "$inputs/anchor-runner-host" --goose "$goose" \
  --net-relay "$inputs/anchor-net-relay" \
  --sandbox-network "$inputs/anchor-sandbox-network" \
  --platform-launcher "$inputs/anchor-platform" \
  --web "$repo/apps/web/dist" --network-tools "$inputs/anchor-network-tools" \
  --git-tools "$inputs/anchor-git-tools" \
  --wecom-tools "$inputs/anchor-wecom-tools" \
  --feishu-gateway "$inputs/anchor-feishu-gateway" \
  --wecom-gateway "$inputs/anchor-wecom-gateway" \
  --uv "$build_root/bootstrap/uv" --rustup-init "$build_root/bootstrap/rustup-init" \
  --pixi "$build_root/bootstrap/pixi" \
  --output "$output"
