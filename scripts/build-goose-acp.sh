#!/usr/bin/env bash
# Build the pinned lean Goose ACP server that Anchor ships as its AgentNode/Pilot runtime.
#
# Why this exists
# ---------------
# Anchor talks to Goose over ACP on stdio. Upstream v1.53.0 offers two entry points that
# both call `goose::acp::server::run`:
#
#   * the full `goose` CLI:      `goose acp`   (release artifact, ~142 MiB, ships the whole CLI)
#   * the lean ACP-only binary:  `goose-acp`   (not published; built from source)
#
# The lean build drops everything Anchor never uses (interactive CLI surface, bundled MCP
# servers, scheduler, ACP HTTP transport, platform/desktop extensions, local inference,
# telemetry, keyring). The shipped package therefore carries a much smaller binary:
#
#   full CLI    148_842_792 B raw / 50_703_638 B gzip -9
#   lean ACP     23_840_248 B raw / 10_655_773 B gzip -9     (measured, x86_64 musl)
#
# The packaged file keeps the name `bin/goose`: preflight, the runtime manifest, the
# `ANCHOR_GOOSE_BINARY*` environment variables and the sandbox allowlist are unchanged.
#
# Pinned inputs - update all of them together when bumping Goose
# -------------------------------------------------------------
#   goose source : https://codeload.github.com/aaif-goose/goose/tar.gz/refs/tags/v1.53.0
#                  sha256 85cc5e76a12e364032df4bec0ed0738a910c3e12ec12731b4405d1fdb18d96ae
#   toolchain    : whatever the source tree's rust-toolchain.toml pins (1.96.1 for v1.53.0)
#   musl cc      : https://musl.cc/x86_64-linux-musl-cross.tgz
#                  sha256 c5d410d9f82a4f24c549fe5d24f988f85b2679b452413a9f7e5f7b956f2fe7ea
#   features     : --no-default-features --features rustls-tls,online-model-meta
#
# Reproducibility
# ---------------
# rustc embeds source paths, so the same inputs built from a different directory produce a
# different digest. Keep ANCHOR_GOOSE_BUILD_ROOT stable and use `--check-reproducible` to
# prove a clean rebuild at that root yields identical bytes before you pin a digest.
#
# The output binary must be a fully static ELF: Anchor's production preflight rejects a
# `bin/goose` that has a PT_INTERP or DT_NEEDED entry. The script verifies this itself.
#
# Usage
# -----
#   scripts/build-goose-acp.sh [--check-reproducible] [--output PATH]
#
# Environment:
#   ANCHOR_GOOSE_BUILD_ROOT   build/cache root (default /var/tmp/anchor-goose-build)
#   ANCHOR_GOOSE_MUSL_CC      existing x86_64-linux-musl-gcc to use instead of downloading
#   CARGO_BUILD_JOBS          cargo parallelism (default 12)
#
# The resulting `goose-acp` binary and its digest are supplied to
# `anchor-distribution --goose <path>`; the printed sha256 is the value to pin in
# anchor-distribution / anchor-devtools / the Goose fixture and in
# ANCHOR_GOOSE_BINARY_SHA256.

set -euo pipefail

GOOSE_VERSION="1.53.0"
SOURCE_URL="https://codeload.github.com/aaif-goose/goose/tar.gz/refs/tags/v${GOOSE_VERSION}"
SOURCE_SHA256="85cc5e76a12e364032df4bec0ed0738a910c3e12ec12731b4405d1fdb18d96ae"
MUSL_URL="https://musl.cc/x86_64-linux-musl-cross.tgz"
MUSL_SHA256="c5d410d9f82a4f24c549fe5d24f988f85b2679b452413a9f7e5f7b956f2fe7ea"
TARGET="x86_64-unknown-linux-musl"
FEATURES="rustls-tls,online-model-meta"
# The digest the rest of the repository pins. It is reproducible only at the default
# build root, because rustc embeds source paths; see the check before publication.
PINNED_SHA256="71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7"

ROOT="${ANCHOR_GOOSE_BUILD_ROOT:-/var/tmp/anchor-goose-build}"
JOBS="${CARGO_BUILD_JOBS:-12}"
CHECK_REPRODUCIBLE=0
OUTPUT=""

while [ $# -gt 0 ]; do
    case "$1" in
        --check-reproducible) CHECK_REPRODUCIBLE=1 ;;
        --output) OUTPUT="${2:?--output requires a path}"; shift ;;
        -h | --help)
            sed -n '2,50p' "$0"
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 2
            ;;
    esac
    shift
done

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }
die() {
    printf 'build-goose-acp: %s\n' "$*" >&2
    exit 1
}

sha256_of() { sha256sum "$1" | cut -d' ' -f1; }

fetch_pinned() {
    local url="$1" path="$2" want="$3"
    if [ -f "$path" ] && [ "$(sha256_of "$path")" = "$want" ]; then
        return 0
    fi
    log "fetching $(basename "$path")"
    curl -fsSL --max-time 1800 -o "$path.part" "$url" || die "download failed: $url"
    mv "$path.part" "$path"
    local got
    got="$(sha256_of "$path")"
    [ "$got" = "$want" ] || die "digest mismatch for $url: got $got want $want"
}

mkdir -p "$ROOT/cache" "$ROOT/toolchain" "$ROOT/artifacts" "$ROOT/src"

# --- pinned source -------------------------------------------------------------------
SOURCE_ARCHIVE="$ROOT/cache/goose-source-v${GOOSE_VERSION}.tar.gz"
fetch_pinned "$SOURCE_URL" "$SOURCE_ARCHIVE" "$SOURCE_SHA256"
log "source sha256 ok"

# --- pinned musl cross compiler (only needed because aws-lc-rs compiles C) ------------
if [ -z "${ANCHOR_GOOSE_MUSL_CC:-}" ]; then
    MUSL_ARCHIVE="$ROOT/cache/x86_64-linux-musl-cross.tgz"
    if [ ! -x "$ROOT/toolchain/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc" ]; then
        fetch_pinned "$MUSL_URL" "$MUSL_ARCHIVE" "$MUSL_SHA256"
        tar xzf "$MUSL_ARCHIVE" -C "$ROOT/toolchain"
    fi
    ANCHOR_GOOSE_MUSL_CC="$ROOT/toolchain/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc"
fi
[ -x "$ANCHOR_GOOSE_MUSL_CC" ] || die "musl cross compiler is not executable: $ANCHOR_GOOSE_MUSL_CC"
export CC_x86_64_unknown_linux_musl="$ANCHOR_GOOSE_MUSL_CC"
export AR_x86_64_unknown_linux_musl="$(dirname "$ANCHOR_GOOSE_MUSL_CC")/x86_64-linux-musl-ar"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$ANCHOR_GOOSE_MUSL_CC"
export CARGO_BUILD_JOBS="$JOBS"

# --- unpack at a stable path ---------------------------------------------------------
rm -rf "$ROOT/src/goose-${GOOSE_VERSION}"
tar xzf "$SOURCE_ARCHIVE" -C "$ROOT/src"
SRC="$ROOT/src/goose-${GOOSE_VERSION}"
[ -f "$SRC/Cargo.toml" ] || die "unexpected source layout under $SRC"
cd "$SRC"

command -v rustup >/dev/null || die "rustup is required (the source pins its toolchain)"
rustup target add "$TARGET" >&2
log "toolchain: $(rustc -V)"

BUILD=(cargo build -p goose --bin goose-acp --profile lean --locked
    --target "$TARGET" --no-default-features --features "$FEATURES")

build_once() {
    "${BUILD[@]}" >&2
}
ARTIFACT="$SRC/target/$TARGET/lean/goose-acp"

log "building lean goose-acp (this takes a few minutes)"
build_once
[ -x "$ARTIFACT" ] || die "cargo did not produce $ARTIFACT"

if [ "$CHECK_REPRODUCIBLE" = 1 ]; then
    cp "$ARTIFACT" "$ROOT/artifacts/goose-acp.first"
    log "clean rebuild at the same root for the reproducibility check"
    cargo clean >&2
    build_once
    if cmp -s "$ROOT/artifacts/goose-acp.first" "$ARTIFACT"; then
        log "reproducible: identical bytes on rebuild"
    else
        die "rebuild at $ROOT produced a different binary; the digest cannot be pinned as-is"
    fi
fi

# --- static ELF gate (production preflight requires this) ----------------------------
interp="$(readelf -l "$ARTIFACT" | grep -ci 'interp' || true)"
needed="$(readelf -d "$ARTIFACT" | grep -ci 'needed' || true)"
[ "$interp" = 0 ] || die "artifact has a PT_INTERP entry; it would be rejected by preflight"
[ "$needed" = 0 ] || die "artifact has DT_NEEDED entries; it would be rejected by preflight"

DIGEST="$(sha256_of "$ARTIFACT")"
BYTES="$(stat -c%s "$ARTIFACT")"
if [ "$DIGEST" != "$PINNED_SHA256" ]; then
    die "built digest $DIGEST does not match the pinned $PINNED_SHA256.
  Anchor pins this digest in anchor-distribution, anchor-devtools (goose/preflight/candidate),
  the Goose fixture, the Web e2e fixture, the .env templates and this script, so a different
  build cannot be shipped. The digest depends on the build root path because rustc embeds
  source paths: rerun with ANCHOR_GOOSE_BUILD_ROOT=/var/tmp/anchor-goose-build (the default the
  repository pin was produced with), or update every pin together."
fi
DEST="${OUTPUT:-$ROOT/artifacts/goose-acp}"
if [ "$DEST" != "$ARTIFACT" ]; then
    install -m 0755 "$ARTIFACT" "$DEST"
fi
printf '%s  goose-acp\n' "$DIGEST" >"$DEST.sha256"

cat >"$DEST.build-record.json" <<JSON
{
  "goose_version": "$GOOSE_VERSION",
  "source_url": "$SOURCE_URL",
  "source_sha256": "$SOURCE_SHA256",
  "recipe": "cargo build -p goose --bin goose-acp --profile lean --locked --target $TARGET --no-default-features --features $FEATURES",
  "toolchain": "$(rustc -V)",
  "musl_cc": "$($ANCHOR_GOOSE_MUSL_CC --version | head -1)",
  "build_root": "$ROOT",
  "artifact": "$(basename "$DEST")",
  "artifact_sha256": "$DIGEST",
  "artifact_bytes": $BYTES,
  "static_elf": true,
  "reproducibility_checked": $([ "$CHECK_REPRODUCIBLE" = 1 ] && echo true || echo false)
}
JSON

log "artifact: $DEST"
log "bytes:    $BYTES"
log "sha256:   $DIGEST"
printf '%s\n' "$DIGEST"
