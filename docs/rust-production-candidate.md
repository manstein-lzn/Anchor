# Rust Production Candidate Harness

`anchor-devtools regression candidate` builds the WebUI and performs one locked,
offline release build of the Rust Host, distribution builder, scholarly tool and
WeCom Gateway. It then selects the existing `goose_distribution` recovery test
in the release profile. The test uses the actual Host, GraphRunner, Goose ACP,
authorized tools and Bubblewrap, replacing only model transport with the existing
deterministic loopback OpenAI-compatible Provider fixture.

The package contains the built WebUI, a small format-1 Graph, the official
academic-research Plugin with its Rust scholarly binary, the Gateway binary and
the caller-supplied pinned Goose binary. The fixture extracts the archive and
runs its Host and Goose binaries with the extracted bundle and Web root. It
checks readiness and the actual HTTP page and JavaScript/CSS bytes, source
absence, manifest file hashes, Run/node history, native Goose history, workspace
and Artifact bytes, the same native Session across stop/resume, a single
workspace write and a second restart after completion. Official integration
binaries are packaged and hashed; no business tool is called.

This scenario makes **five local model requests**: two before stop and three
after resume. The completed-run restart adds no requests. The previous candidate
scenario made three requests and restarted only after completion; these are
different scenarios. The native entry deliberately reuses the existing recovery
fixture instead of claiming the old three-request scenario is unchanged.

The command does not load `.env`, inherit application/provider credentials,
change production configuration or data, or call Docmost, WeCom, public academic
services or other external business systems. It refuses Web dotenv files that
Vite would load for a production build. Build processes inherit only tool
locations and use a private home. Cargo runs offline with an isolated Cargo home
that shares registry/git caches without inheriting credentials or Cargo config.
Dependencies must already be available locally, including installed Web
dependencies and the stable Rust toolchain.

## Run

Provide the Anchor-built lean Goose ACP binary (`scripts/build-goose-acp.sh`,
upstream v1.53.0 sources) required by `anchor-distribution`:

```sh
anchor-devtools regression candidate \
  --workspace-root /absolute/path/to/Anchor \
  --goose /absolute/path/to/goose \
  --target-dir /tmp/anchor-production-candidate-target
```

Alternatively set `ANCHOR_GOOSE_BINARY` or `ANCHOR_DISTRIBUTION_GOOSE` to the local
binary; `--goose` takes precedence, followed by `ANCHOR_GOOSE_BINARY`. The default
evidence directory is a new temporary directory. To choose one, pass a path
that does not already exist:

```sh
anchor-devtools regression candidate \
  --goose /absolute/path/to/goose \
  --evidence-root /tmp/my-rust-candidate-evidence
```

The default target directory is `/tmp/anchor-production-candidate-target`, or
`CARGO_TARGET_DIR` when set. Reusing it permits Cargo to reuse existing release
outputs. The candidate invokes the following bounded sequence; the final command
may compile its test harness and reuses the same target directory:

```sh
npm --prefix apps/web run build
cargo +stable build --release --locked --offline --manifest-path rust/Cargo.toml \
  -p anchor-runner-host -p anchor-distribution -p anchor-scholarly \
  -p anchor-wecom-gateway --bins
cargo +stable test --release --locked --offline --manifest-path rust/Cargo.toml \
  -p anchor-runner-host --test goose_distribution -- \
  --ignored --exact extracted_goose_runtime_resumes_without_sources_or_replaying_workspace_effects \
  --test-threads=1 --nocapture
```

The candidate passes `ANCHOR_TEST_WEB_DIST`, `ANCHOR_TEST_ACADEMIC_PLUGIN`,
`ANCHOR_TEST_SCHOLARLY_BINARY` and `ANCHOR_TEST_WECOM_GATEWAY_BINARY` explicitly to
the test. Direct callers of the existing distribution fixture can omit these
inputs for its original smaller package; such evidence cannot satisfy candidate
acceptance.

Stdout is a JSON report, also retained as `report.json` in the private evidence
directory. It contains actual argv, working directory, exit code (null if a
process could not start or was signaled), per-command logs and SHA256 hashes,
test counts, validated scenario count and archive/manifest/Provider hashes.
Observed scenario/Provider file counts and actual local request counts are
reported separately, including partial evidence from failed fixtures.
The scenario retains the archive, Runtime manifest, Provider transcript and
Host logs. One executed, passed distribution test and exactly one complete
scenario report are required; unrelated tests may be filtered by the exact selector.
Failed builds/tests, absent or mismatched evidence,
unfinished Provider scripts, extra requests or repeated effects return failure.
Configuration errors before execution return a nonzero exit without fabricating
a successful report. No workspace-wide build or regression suite is run.

Focused development checks, once the CLI modules are exported:

```sh
cargo +stable test --locked --offline --manifest-path rust/Cargo.toml \
  -p anchor-devtools candidate::
cargo +stable test --locked --offline --manifest-path rust/Cargo.toml \
  -p anchor-devtools --test cli
```

This harness is a deterministic production-candidate regression, not real
provider/business acceptance, target-machine deployment validation, migration,
or authorization to switch production traffic and data.

`anchor-devtools preflight` prints the deployment preflight JSON without flags.
`anchor-devtools cutover ...` forwards options to the native inventory module;
a JSON `decision` of `blocked` exits 2. `regression fixture` and
`regression goose-fixture` retain their existing bounded regression behavior.
