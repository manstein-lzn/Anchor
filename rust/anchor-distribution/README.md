# Source-free Goose Runtime packaging

`anchor-distribution` is an operator-facing Rust builder, not a new Runner or an
installer. It does not execute supplied binaries, call providers, install system
dependencies, or change deployment permissions.

```sh
anchor-distribution --host /reviewed/anchor-runner-host \
  --goose /reviewed/goose --bundle /reviewed/format-1-bundle \
  --web /reviewed/web-dist --scholarly /reviewed/anchor-scholarly \
  --docmost-tools /reviewed/anchor-docmost-tools \
  --wecom-tools /reviewed/anchor-wecom-tools \
  --wecom-gateway /reviewed/anchor-wecom-gateway \
  --output /existing/output/runtime.tar.gz
```

`--web`, the first-party binary flags and repeated `--tool NAME=ELF` inputs are
optional unless their Plugin is referenced by the admitted Graph. If the Graph
references `academic-research`, `--scholarly` becomes required. If it references
`docmost`, `--docmost-tools` becomes required. If it references `wecom`, both
`--wecom-tools` and `--wecom-gateway` become required. Each first-party binary
must be supplied separately; passing one without its Plugin is rejected. The
builder places the scholarly ELF at
`bundle/plugins/academic-research/bin/anchor-scholarly`, updates the frozen
Plugin summary and validates the resulting bundle. Native first-party MCP and
channel entrypoints are placed under their declared Plugin `bin/` paths and do
not use an interpreter or host `PATH`. The builder consumes reviewed,
already-built target ELFs; it does not build workspace crates or resolve a
cross-crate install manifest.

Output must be a new
`.tar.gz` file under an existing, non-symlink directory. Success prints a JSON
`PackageReport` with the output path, archive SHA256, and inventory.

## Public API

`ToolBinary { name: String, path: PathBuf }` and
`PackageRequest` carries `host`, `goose`, `bundle`, optional `web`, `tools`,
optional `scholarly`, `docmost_tools`, `wecom_tools`, `wecom_gateway`, and
`output`; pass it to
`build_package(&PackageRequest) -> Result<PackageReport>`.
`PackageReport { output, sha256, inventory }` and the inventory are serializable.

Goose is fixed to the Anchor-built lean ACP binary from pinned upstream v1.53.0
sources (`../scripts/build-goose-acp.sh`) SHA256
`71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7`.
The builder does not build Goose itself; supply the binary produced by that
script. There is no environment or CLI option to bypass this check. Host and tools must
be executable ELF files with an executable entry point and matching platform.
The supplied Host's version, features and dependency closure are not attested by
ELF acceptance. Arbitrary ELF is not proof of the expected production Host.

## Layout And Reproducibility

```text
anchor-runtime/
  bin/anchor-runner-host
  bin/goose
  bin/<explicit-tool>
  bundle/graph.json
  bundle/manifest.json
  bundle/plugins/<declared-plugin>/<declared-resource>
  bundle/plugins/academic-research/bin/anchor-scholarly # when referenced
  bundle/plugins/docmost/bin/anchor-docmost-tools       # when referenced
  bundle/plugins/wecom/bin/anchor-wecom-tools           # when referenced
  bundle/plugins/wecom/bin/anchor-wecom-gateway         # when referenced
  web/                         # optional compiled assets
  README.md
  runtime-manifest.json
```

Input resources are copied into a private snapshot using component-wise
`NOFOLLOW` opens. Shared `FileGraphBundleLoader` admission runs on that snapshot.
Bundle files must exactly match graph/manifest and the admitted Plugin resource
pins; only their parent directories are allowed, so unknown empty directories
are rejected as well. Extensionless native Plugin executables are supported only
under `plugins/<id>/bin/`, validated as ELF, and retain normalized execute mode.
Scripts and source files are not supported; compiled JS/Wasm are accepted only
in the explicitly supplied Web tree. Source maps are rejected.

All input files, including Host, Goose and resources, must be ordinary files with
`nlink=1`; symlinks, ancestor symlinks, hardlinks and special files are rejected.
Use `fs::copy` or a normal independent copy for build/fixture binaries that are
hardlinked. The builder rechecks input content and filesystem identities before
publication. It does not lock writers; the admitted private snapshot determines
the payload, and drift checks do not promise detection of changes after the last
check.

Files and directories have stable ordering, UID/GID zero, empty owner names,
mtime zero, and normalized modes (ELF 0755, resources 0644, directories 0755).
Gzip omits filenames and timestamps. The manifest records builder version,
pinned Goose identity, platform, ELF identities plus each executable's
interpreter and `DT_NEEDED` runtime library names, admitted Plugin pins, JSON
environment references and sorted file sizes/hashes/modes. It does not include
absolute input paths, output names or mutable metadata. `runtime-manifest.json`
does not hash itself; the build report hashes the whole archive. Neither is a
signature.

The complete archive is fsynced privately and published with atomic
`renameat(NOREPLACE)`; a concurrent winner is never overwritten. An unsupported
filesystem fails closed. Post-publication directory-fsync failure is reported as
`PublicationUncertain`, not as success or proof that no output exists. This
implementation targets Linux and uses `/proc/self/fd` to anchor publication.

## Boundaries

Known source/repository, credential and mutable-state paths are denied. JSON,
YAML and TOML are parsed to reject known literal credential fields and URL
userinfo; environment references such as `${API_KEY}` are retained unexpanded.
Text assignments and PEM private-key markers are also checked. This is a
conservative admission filter, not generic secret discovery: arbitrary prose,
binary contents, images, generated JS and custom credential field names require
operator review. No claim is made that every possible secret can be detected.

Per-file size is limited to 1 GiB, textual resource parsing to 64 MiB, and paths
to deterministic USTAR limits. There is no total-size/file-count quota, binary
provenance verifier, ELF shared-library resolver, automatic external-service
installation, or cross-platform package builder. Use reviewed local inputs.
Packaging alone is not runtime execution, real-provider acceptance, a test on an
production-cutover evidence. Model/API credentials,
host-path grants and runtime state must be configured outside the archive.

## Focused Verification

Run `cargo +stable test -p anchor-distribution` and
`cargo +stable clippy -p anchor-distribution --all-targets -- -D warnings` from the
Rust workspace, then a crate-scoped rustfmt check. Small local ELF fixtures test
archive construction; only private test helpers substitute the expected Goose
digest. Public API and CLI tests verify that the fixed pin cannot be bypassed.

The separate ignored local-binary smoke can be selected explicitly:

```sh
ANCHOR_DISTRIBUTION_GOOSE=/absolute/pinned/goose cargo +stable test \
  -p anchor-distribution --test cli \
  cli_packages_pinned_goose_and_reports_archive_hash_without_executing_inputs \
  -- --ignored --exact
```

That smoke packages real fixed Goose with a small ELF Host stand-in; it does not
execute Goose, an actual Anchor Host, or any model. Full extracted-runtime
acceptance belongs to the Host's distribution integration fixture.
