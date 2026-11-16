#!/usr/bin/env bash
# Build-time tooling only, with pinned uv, Rustup, pixi and official lark-cli inputs.
# Never reads the developer .env or platform state.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
  cat <<'HELP'
Usage: scripts/build-platform-release.sh --goose PINNED_ELF --output NEW.tar.gz
       [--build-root DIRECTORY] [--skip-build]
       [--lark-cli REVIEWED_1.0.100_ELF]

Build Host, Hub/Eval, network relay, sandbox networking, native platform launcher,
network/Git/WeCom MCPs and Feishu/WeCom gateways plus WebUI, prepare fixed
uv/Rustup/pixi and official lark-cli 1.0.100 inputs, and create a format-5
platform archive with protected input-discovery evaluation and guidance resources,
and no Graph bundles or factory Graph registry.
Does not run or deploy the Host.
--skip-build packages existing <build-root>/target/release binaries and WebUI.
--lark-cli supplies a reviewed binary instead of bootstrap/lark-cli; the fixed
SHA256 and static ELF checks still apply.
Operator build requirements: Rust stable, Node/npm, curl, tar, coreutils.
HELP
}

goose=
output=
build_root="$repo/.local/platform-release"
skip_build=0
lark_cli=
while (($#)); do
  case "$1" in
    --goose) goose=${2:?--goose needs an ELF}; shift 2 ;;
    --output) output=${2:?--output needs an archive}; shift 2 ;;
    --build-root) build_root=${2:?--build-root needs a directory}; shift 2 ;;
    --skip-build) skip_build=1; shift ;;
    --lark-cli) lark_cli=${2:?--lark-cli needs an ELF}; shift 2 ;;
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
    -p anchor-feishu-gateway -p anchor-wecom-gateway -p anchor-distribution \
    -p anchor-hub -p anchor-eval --bins
  npm --prefix "$repo/apps/web" ci
  npm --prefix "$repo/apps/web" run build
fi
if [[ ! -d "$build_root/bootstrap" ]]; then
  "$repo/scripts/prepare-platform-bootstrap.sh" --output "$build_root/bootstrap"
fi
lark_cli=${lark_cli:-"$build_root/bootstrap/lark-cli"}
[[ -f "$lark_cli" ]] || { printf 'Pinned lark-cli is missing; prepare a new bootstrap directory or supply --lark-cli.\n' >&2; exit 1; }
inputs=$(mktemp -d "$build_root/package-inputs.XXXXXXXX")
trap 'rm -rf -- "$inputs"' EXIT
for binary in anchor-distribution anchor-runner-host anchor-hub anchor-eval anchor-net-relay anchor-sandbox-network anchor-platform anchor-network-tools anchor-git-tools anchor-wecom-tools anchor-feishu-gateway anchor-wecom-gateway; do
  # Cargo output may be hardlinked to target/deps; packaging admits independent
  # ordinary copies and records precisely these bytes.
  cp -- "$target/release/$binary" "$inputs/$binary"
done
"$target/release/anchor-distribution" --platform \
  --host "$inputs/anchor-runner-host" --goose "$goose" \
  --hub "$inputs/anchor-hub" --eval "$inputs/anchor-eval" \
  --net-relay "$inputs/anchor-net-relay" \
  --sandbox-network "$inputs/anchor-sandbox-network" \
  --platform-launcher "$inputs/anchor-platform" \
  --web "$repo/apps/web/dist" --network-tools "$inputs/anchor-network-tools" \
  --git-tools "$inputs/anchor-git-tools" \
  --wecom-tools "$inputs/anchor-wecom-tools" \
  --feishu-gateway "$inputs/anchor-feishu-gateway" \
  --lark-cli "$lark_cli" \
  --wecom-gateway "$inputs/anchor-wecom-gateway" \
  --uv "$build_root/bootstrap/uv" --rustup-init "$build_root/bootstrap/rustup-init" \
  --pixi "$build_root/bootstrap/pixi" \
  --output "$output"
