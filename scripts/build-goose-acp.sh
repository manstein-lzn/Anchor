#!/usr/bin/env bash
# Build the versioned lean Goose ACP server that Anchor ships as its AgentNode/Pilot runtime.
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
#   patch        : patches/goose-1.53.0-responses-reasoning.patch
#                  Retains Responses reasoning in Goose native history and requests.
#   patch        : patches/goose-1.53.0-external-completion.patch
#                  Requires opted-in host completion, with bounded draft reminders.
#   patch        : patches/goose-1.53.0-provider-errors.patch
#                  Persists structured provider failures and returns ACP prompt errors.
#
# Source builds fix the upstream version, reviewed patches and Cargo.lock. Each
# machine may produce a different binary digest. Record the actual digest for
# runtime identity and packaging; it is not a cross-machine build prerequisite.
# `--check-reproducible` is an optional maintainer diagnostic on one build host.
#
# The output binary must be a fully static ELF: Anchor's production preflight rejects a
# `bin/goose` that has a PT_INTERP or DT_NEEDED entry. The script verifies this itself.
#
# Usage
# -----
#   scripts/build-goose-acp.sh [--ensure] [--check-reproducible] [--output PATH]
#   --ensure reuses this recipe's output, or builds it when missing/outdated.
#
# Environment:
#   ANCHOR_GOOSE_BUILD_ROOT   build/cache root (default /var/tmp/anchor-goose-build)
#   ANCHOR_GOOSE_MUSL_CC      existing x86_64-linux-musl-gcc to use instead of downloading
#   CARGO_BUILD_JOBS          cargo parallelism (default 12)
#
# Source startup passes the resulting digest to the Host automatically. Release
# packaging retains its own artifact verification policy.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REASONING_PATCH="$SCRIPT_DIR/patches/goose-1.53.0-responses-reasoning.patch"
COMPLETION_PATCH="$SCRIPT_DIR/patches/goose-1.53.0-external-completion.patch"
PROVIDER_ERRORS_PATCH="$SCRIPT_DIR/patches/goose-1.53.0-provider-errors.patch"
GOOSE_VERSION="1.53.0"
SOURCE_URL="https://codeload.github.com/aaif-goose/goose/tar.gz/refs/tags/v${GOOSE_VERSION}"
SOURCE_SHA256="85cc5e76a12e364032df4bec0ed0738a910c3e12ec12731b4405d1fdb18d96ae"
MUSL_URL="https://musl.cc/x86_64-linux-musl-cross.tgz"
MUSL_SHA256="c5d410d9f82a4f24c549fe5d24f988f85b2679b452413a9f7e5f7b956f2fe7ea"
TARGET="x86_64-unknown-linux-musl"
FEATURES="rustls-tls,online-model-meta"

ROOT="${ANCHOR_GOOSE_BUILD_ROOT:-/var/tmp/anchor-goose-build}"
JOBS="${CARGO_BUILD_JOBS:-12}"
CHECK_REPRODUCIBLE=0
ENSURE=0
OUTPUT=""

while [ $# -gt 0 ]; do
    case "$1" in
        --ensure) ENSURE=1 ;;
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

if [ -n "$OUTPUT" ]; then
    OUTPUT="$(realpath -m -- "$OUTPUT")"
fi

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

# The input marker is a rebuildable cache key, not an executable policy.
# Keep a checksum of the actual output so truncated files are rebuilt as well.
command -v sha256sum >/dev/null || die "sha256sum is required (install coreutils)"
INPUT_DIGEST="$({
    sha256_of "$0"
    sha256_of "$REASONING_PATCH"
    sha256_of "$COMPLETION_PATCH"
    sha256_of "$PROVIDER_ERRORS_PATCH"
} | sha256sum | cut -d' ' -f1)"
if [ "$ENSURE" = 1 ]; then
    [ -n "$OUTPUT" ] || die "--ensure requires --output PATH"
    [ "$CHECK_REPRODUCIBLE" = 0 ] || die "--ensure cannot be combined with --check-reproducible"
    if [ -x "$OUTPUT" ] && [ -r "$OUTPUT.sha256" ]; then
        saved_digest= saved_label= saved_inputs=
        { read -r saved_digest _ || true; read -r saved_label saved_inputs || true; } < "$OUTPUT.sha256"
        if [ "$saved_label" = "#source-inputs" ] && [ "$saved_inputs" = "$INPUT_DIGEST" ] &&
            [ "$saved_digest" = "$(sha256_of "$OUTPUT")" ]; then
            log "Goose source build ready: $OUTPUT"
            printf '%s\n' "$saved_digest"
            exit 0
        fi
    fi
    log "preparing Goose ${GOOSE_VERSION} with repository patches (first build can take several minutes)"
fi

[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] || die "this recipe requires Linux x86_64"
for tool in rustup cargo rustc cc curl tar gzip patch rsync readelf sha256sum install stat cmp flock realpath; do
    command -v "$tool" >/dev/null || die "missing build prerequisite: $tool (install Rust/rustup, C build tools, curl, patch, rsync, binutils and util-linux)"
done

mkdir -p "$ROOT/cache" "$ROOT/toolchain" "$ROOT/artifacts" "$ROOT/src"
# The stable source/cache path is shared by source checkouts. Serialize its mutation.
exec 8>"$ROOT/build.lock"
flock 8

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

# --- prepare verified source without discarding the Cargo cache ----------------------
# Stage fresh pinned inputs, then synchronize source at the stable build path.
# Existing target artifacts remain available; source from older patches is removed.
command -v rsync >/dev/null || die "rsync is required to preserve the build cache"
STAGING="$(mktemp -d "$ROOT/src/.goose-${GOOSE_VERSION}.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT
tar xzf "$SOURCE_ARCHIVE" -C "$STAGING"
STAGED_SRC="$STAGING/goose-${GOOSE_VERSION}"
SRC="$ROOT/src/goose-${GOOSE_VERSION}"
[ -f "$STAGED_SRC/Cargo.toml" ] || die "unexpected source layout under $STAGED_SRC"
[ -f "$REASONING_PATCH" ] || die "missing reviewed Responses reasoning patch"
[ -f "$COMPLETION_PATCH" ] || die "missing reviewed external completion patch"
[ -f "$PROVIDER_ERRORS_PATCH" ] || die "missing reviewed provider errors patch"
patch --batch --fuzz=0 -d "$STAGED_SRC" -p1 < "$REASONING_PATCH"
patch --batch --fuzz=0 -d "$STAGED_SRC" -p1 < "$COMPLETION_PATCH"
patch --batch --fuzz=0 -d "$STAGED_SRC" -p1 < "$PROVIDER_ERRORS_PATCH"
mkdir -p "$SRC"
rsync -rlc --delete --exclude=/target/ "$STAGED_SRC/" "$SRC/"
PATCH_SHA256="$(sha256_of "$REASONING_PATCH")"
COMPLETION_PATCH_SHA256="$(sha256_of "$COMPLETION_PATCH")"
PROVIDER_ERRORS_PATCH_SHA256="$(sha256_of "$PROVIDER_ERRORS_PATCH")"
cd "$SRC"
# Keep the recipe output independent of a caller's Host target directory.
export CARGO_TARGET_DIR="$SRC/target"
# A caller's toolchain override must not change the reviewed recipe.
RUSTUP_TOOLCHAIN=$(sed -n 's/^channel = "\([^"]*\)"$/\1/p' rust-toolchain.toml)
[ -n "$RUSTUP_TOOLCHAIN" ] || die "verified source is missing its Rust toolchain pin"
export RUSTUP_TOOLCHAIN

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
        die "rebuild at $ROOT produced a different binary; inspect this build host's inputs"
    fi
fi

# --- static ELF gate (production preflight requires this) ----------------------------
interp="$(readelf -l "$ARTIFACT" | grep -ci 'interp' || true)"
needed="$(readelf -d "$ARTIFACT" | grep -ci 'needed' || true)"
[ "$interp" = 0 ] || die "artifact has a PT_INTERP entry; it would be rejected by preflight"
[ "$needed" = 0 ] || die "artifact has DT_NEEDED entries; it would be rejected by preflight"

DIGEST="$(sha256_of "$ARTIFACT")"
BYTES="$(stat -c%s "$ARTIFACT")"
DEST="${OUTPUT:-$ROOT/artifacts/goose-acp}"
if [ "$DEST" != "$ARTIFACT" ]; then
    mkdir -p "$(dirname -- "$DEST")"
    install -m 0755 "$ARTIFACT" "$DEST.part"
    mv -f -- "$DEST.part" "$DEST"
fi
printf '%s  %s\n#source-inputs %s\n' "$DIGEST" "$(basename -- "$DEST")" "$INPUT_DIGEST" >"$DEST.sha256"

cat >"$DEST.build-record.json" <<JSON
{
  "goose_version": "$GOOSE_VERSION",
  "source_url": "$SOURCE_URL",
  "source_sha256": "$SOURCE_SHA256",
  "build_inputs_sha256": "$INPUT_DIGEST",
  "patch": "goose-1.53.0-responses-reasoning.patch",
  "patch_sha256": "$PATCH_SHA256",
  "patches": [
    {"path": "goose-1.53.0-responses-reasoning.patch", "sha256": "$PATCH_SHA256"},
    {"path": "goose-1.53.0-external-completion.patch", "sha256": "$COMPLETION_PATCH_SHA256"},
    {"path": "goose-1.53.0-provider-errors.patch", "sha256": "$PROVIDER_ERRORS_PATCH_SHA256"}
  ],
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
