# Source-free Anchor packaging

`anchor-distribution` is an operator-facing Rust builder, not a new Runner or an
installer. It does not execute supplied binaries, call providers, install system
dependencies, or change deployment permissions.

## Complete platform

A platform release has no business Graph. The same Host and Kernel serve Pilot
against an initially empty catalog. The required native network Plugin ships as
an importable candidate; Host seeds it into the Library without enabling a
Session's network permission. Fixed uv, Rustup and pixi bootstrap tools are native
static executables; pixi is mounted read-only on agent and Pilot command PATH
without registration or installation. Python versions, Rust toolchains and packages are prepared
on demand under the mutable state root, one shared installation per version.

```sh
anchor-distribution --platform --host /reviewed/anchor-runner-host \
  --goose /reviewed/goose --net-relay /reviewed/anchor-net-relay \
  --sandbox-network /reviewed/anchor-sandbox-network \
  --platform-launcher /reviewed/anchor-platform \
  --network-tools /reviewed/anchor-network-tools --web /reviewed/web-dist \
  --uv /reviewed/uv --rustup-init /reviewed/rustup-init --pixi /reviewed/pixi \
  --output /existing/output/platform.tar.gz
```

All platform inputs are required. The public API is
`build_platform_package(&PlatformPackageRequest) -> Result<PlatformPackageReport>`.
The archive has `runtime-manifest.json` format 3 with `kind: "platform"`, a
top-level admitted `plugins` inventory, and
`bootstrap.uv`/`bootstrap.rustup_init`/`bootstrap.pixi` identities containing package-relative
`path`, fixed `version`, `source` and executable `sha256`. There is no `bundle`
field or `bundle/` tree. The original Graph package below keeps format 1 and its
strict admission checks.

```text
anchor-runtime/
  bin/anchor-runner-host
  bin/goose
  bin/anchor-net-relay
  bin/anchor-sandbox-network
  bin/anchor-platform
  bin/uv
  bin/rustup-init
  bin/pixi
  plugins/network/plugin.json
  plugins/network/skills/network/SKILL.md
  plugins/network/bin/anchor-network-tools
  web/index.html
  README.md
  runtime-manifest.json
```

The fixed bootstrap is uv 0.11.6 x86_64 Linux musl, binary SHA256
`61c5aaf37153f7b2b4c8d11ff5174fa30e930dc3dc2825f54593a2dd319eb8af`, fetched
from its official versioned GitHub release (archive SHA256
`aa342a53abe42364093506d7704214d2cdca30b916843e520bc67759a5d20132`), and
Rustup 1.29.0 x86_64 Linux musl, SHA256
`9cd3fda5fd293890e36ab271af6a786ee22084b5f6c2b83fd8323cec6f0992c1`, from
`https://static.rust-lang.org/rustup/archive/1.29.0/x86_64-unknown-linux-musl/rustup-init`.
The third pin is pixi 0.83.0 x86_64 Linux musl, SHA256
`50571451e96f4a82cf675cd83baaf66fff9b718f0aaf153900b9faf4c29bd401`, from
`https://github.com/prefix-dev/pixi/releases/download/v0.83.0/pixi-x86_64-unknown-linux-musl`.
It is a self-contained executable, not a managed language installation; its
caches remain under the writable workspace HOME. Development deployments without
a release can authorize an executable with `ANCHOR_PILOT_PIXI=/absolute/path`;
no version probe is run for that override.
There is no pin bypass. `scripts/prepare-platform-bootstrap.sh --output NEW_DIR`
fetches and verifies these three native build inputs without installing a language
environment. `scripts/build-platform-release.sh --goose PINNED_ELF --output NEW.tar.gz`
builds the required Rust binaries and WebUI and packages them. It never loads the
developer `.env` or copies platform state into the release.

## Fresh platform installation

The target machine needs Linux x86_64, Bubblewrap, slirp4netns, Git, shell, a C
linker, CA certificates and the shared libraries recorded in the executable
inventory. The installation shell probes the executable prerequisites and gives
a system-package command when they are absent; it never falls back to sharing
the Host network. Rust/Node are build-machine requirements; Python/Rust
development installations are unnecessary on the target machine.

Model configuration is optional at installation. Start the service and configure
the provider in WebUI **Settings**; the Host stores it privately under
`data/state/managed/`. Python and Rust initialization is available on the same page.

The launcher derives all resource and mutable roots from its installed location,
clears inherited configuration, and does not read environment files. Model, Plugin,
channel and language settings belong to the existing Host-managed WebUI flows.
The retired `--model-config` option is rejected; old files are left untouched.

```sh
scripts/install-platform.sh --archive /reviewed/platform.tar.gz \
  --sha256 TRUSTED_BUILD_REPORT_SHA256 --install-root /existing/parent/anchor
/existing/parent/anchor/release/anchor-runtime/bin/anchor-platform start
```

The script verifies the archive hash before extracting its native installer into
a private temporary directory; alternatively pass a separately reviewed ELF
with `--installer`. The native install API verifies its own private archive
snapshot, rejects links/special files/traversal/duplicates, validates fixed
Goose/bootstrap pins and the exact admitted inventory, then atomically publishes
an entirely new installation. Existing roots are refused. File count is capped
at 100000, individual files at 1 GiB, and compressed/uncompressed totals at 8 GiB.
It does not start a service or update an existing installation.

```text
<install-root>/
  release/anchor-runtime/     read-only release, directories/ELFs 0555, resources 0444
  config/model.env           optional deployment-managed model data, 0600
  data/home/                 private runtime HOME
  data/state/                Host facts, including managed/runtime-config.json
  data/catalog/              empty business Graph catalog
  data/library/              Library, populated by Host's native network seed
  data/workspaces/           Session and node workspaces
```

The native `anchor-platform start` clears inherited environment settings and
selects installation-owned roots, fixed Goose, isolated model transport and the
optional private model data. Without that file it injects no model environment
variables, allowing WebUI settings to take effect. It binds `127.0.0.1:8077`; open
that address to configure the service and use Pilot. The
initial command grant is `sh,git`; Python/Rust execution is granted through the
Host's managed initialization. It does not inherit the developer machine's
Python/Rust paths, dependency cache, Graph or secrets. The Host reads the release
bootstrap identities and its managed configuration; users do not register
internal directories. Startup and initialization behavior require separate Host
and real-provider acceptance, not merely a successful package build.
Use `anchor-platform start --listen 127.0.0.1:PORT` to choose another local port;
the native launcher accepts only loopback IP addresses with a nonzero port.

## Standalone Graph package

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
`plugins/academic-research/bin/anchor-scholarly` (beside the bundle, in the
shipped Plugin sources), writes the frozen manifest's Plugin *references*
(`{"id": …}` — a bundle never carries Plugin bytes) and validates the resulting
bundle. Native first-party MCP and channel entrypoints are placed under their
declared Plugin `bin/` paths and do not use an interpreter or host `PATH`. The builder consumes reviewed,
already-built target ELFs; it does not build workspace crates or resolve a
cross-crate install manifest.

The archive ships a Graph bundle that *references* its Plugins and the Plugin
sources beside it. A deployment that runs it must therefore point the host at the
one Library root those Plugins are imported into (`ANCHOR_RUNNER_LIBRARY_ROOT`,
or `ANCHOR_RUNNER_CATALOG_ROOT` when the deployment keeps one root); the bundle
directory is a Graph, not a Plugin home, and the host never infers a Library root
from it.

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
`17e065a79cb0508413af714724aed51d6b76972dda9feebdda701330134fb640`.
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
  bin/anchor-net-relay
  bin/anchor-sandbox-network
  bundle/graph.json
  bundle/manifest.json          # Plugin *references* only: {"id": …}
  plugins/<declared-plugin>/<declared-resource>          # imported into the Library
  plugins/academic-research/bin/anchor-scholarly # when referenced
  plugins/docmost/bin/anchor-docmost-tools       # when referenced
  plugins/wecom/bin/anchor-wecom-tools           # when referenced
  plugins/wecom/bin/anchor-wecom-gateway         # when referenced
  web/                         # optional compiled assets
  README.md
  runtime-manifest.json
```

Input resources are copied into a private snapshot using component-wise
`NOFOLLOW` opens. Shared `FileGraphBundleLoader` admission runs on that snapshot:
the input bundle directory holds exactly `graph.json`, `manifest.json` and a
`plugins/` tree of Plugin *sources* (a Plugin's own files are its code), and the
shipped `bundle/` holds exactly `graph.json` and `manifest.json`. Only those
parents' directories are allowed, so unknown empty directories are rejected as
well. Extensionless native Plugin executables are supported only
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
Only a Plugin's root `plugins/<id>/plugin.json` treats `configuration.fields`
keys as field identifiers and `configuration.environment` values as references
to those declared fields, using the public Host configuration types. Declaration
metadata is still scanned; secret defaults (including empty or null defaults),
embedded values and literal credentials elsewhere remain forbidden. Non-secret
defaults under known credential field or mapped environment names retain the
credential checks, even when the field is not typed as secret. Full
Plugin/MCP binding validation remains owned by shared catalog admission.
Native Docmost admission requires its declared `--endpoint` argument with
`${DOCMOST_BASE_URL}/api/files/upload`; arbitrary endpoint or argument overrides
are rejected, and WeCom's native MCP entrypoint still requires no arguments.
Text assignments and PEM private-key markers are also checked. This is a
conservative admission filter, not generic secret discovery: arbitrary prose,
binary contents, images, generated JS and custom credential field names require
operator review. No claim is made that every possible secret can be detected.

Per-file size is limited to 1 GiB, textual resource parsing to 64 MiB, and paths
to deterministic USTAR limits. There is no total-size/file-count quota, binary
provenance verifier, ELF shared-library resolver, automatic external-service
installation, or cross-platform package builder. Use reviewed local inputs.
Packaging alone does not prove runtime execution, real-provider acceptance,
compatibility with a target distribution, or production readiness. Model/API credentials,
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
