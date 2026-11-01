#!/usr/bin/env bash
# Fetch only the fixed native bootstrap tools; no Python/Rust installation.
set -euo pipefail
umask 077

usage() {
  cat <<'HELP'
Usage: scripts/prepare-platform-bootstrap.sh --output NEW_DIRECTORY

Download reviewed x86_64 Linux musl uv 0.11.6, rustup-init 1.29.0 and pixi
0.83.0, verify fixed SHA256 values, and publish three executable inputs for
platform packaging.
The parent directory must exist. Existing outputs are never replaced.
HELP
}

destination=
while (($#)); do
  case "$1" in
    --output) destination=${2:?--output needs a directory}; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ -n "$destination" ]] || { usage >&2; exit 2; }
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || {
  printf 'Bootstrap preparation supports Linux x86_64 only.\n' >&2; exit 1;
}
for tool in curl tar sha256sum mktemp realpath mv chmod; do
  command -v "$tool" >/dev/null || { printf 'Missing build prerequisite: %s\n' "$tool" >&2; exit 1; }
done
parent=$(realpath -e -- "$(dirname -- "$destination")")
destination="$parent/$(basename -- "$destination")"
[[ ! -e "$destination" && ! -L "$destination" ]] || { printf 'Output already exists.\n' >&2; exit 1; }
staging=$(mktemp -d "$parent/.anchor-bootstrap.XXXXXXXX")
trap 'rm -rf -- "$staging"' EXIT
download="$staging/uv.tar.gz"
curl --silent --show-error --fail --location --proto '=https' --tlsv1.2 \
  --connect-timeout 15 --max-time 180 \
  https://github.com/astral-sh/uv/releases/download/0.11.6/uv-x86_64-unknown-linux-musl.tar.gz \
  --output "$download"
printf '%s  %s\n' aa342a53abe42364093506d7704214d2cdca30b916843e520bc67759a5d20132 "$download" | sha256sum --check --status
tar -xOf "$download" uv-x86_64-unknown-linux-musl/uv > "$staging/uv"
rm -- "$download"
printf '%s  %s\n' 61c5aaf37153f7b2b4c8d11ff5174fa30e930dc3dc2825f54593a2dd319eb8af "$staging/uv" | sha256sum --check --status
curl --silent --show-error --fail --location --proto '=https' --tlsv1.2 \
  --connect-timeout 15 --max-time 180 \
  https://static.rust-lang.org/rustup/archive/1.29.0/x86_64-unknown-linux-musl/rustup-init \
  --output "$staging/rustup-init"
printf '%s  %s\n' 9cd3fda5fd293890e36ab271af6a786ee22084b5f6c2b83fd8323cec6f0992c1 "$staging/rustup-init" | sha256sum --check --status
curl --silent --show-error --fail --location --proto '=https' --tlsv1.2 \
  --connect-timeout 15 --max-time 180 \
  https://github.com/prefix-dev/pixi/releases/download/v0.83.0/pixi-x86_64-unknown-linux-musl \
  --output "$staging/pixi"
printf '%s  %s\n' 50571451e96f4a82cf675cd83baaf66fff9b718f0aaf153900b9faf4c29bd401 "$staging/pixi" | sha256sum --check --status
chmod 0755 "$staging/uv" "$staging/rustup-init" "$staging/pixi"
mv -T --no-clobber -- "$staging" "$destination"
[[ ! -d "$staging" ]] || { printf 'A concurrent preparation published the output.\n' >&2; exit 1; }
trap - EXIT
printf 'uv: %s/uv\nrustup-init: %s/rustup-init\npixi: %s/pixi\n' "$destination" "$destination" "$destination"
