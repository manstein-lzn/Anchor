# Rust platform deployment

The format-3 platform archive contains the Host, fixed Goose, compiled WebUI,
network Plugin, network relay and fixed uv/Rustup/pixi bootstrap tools. The installed
`anchor-platform start` command derives release and private data paths from its
own location and clears inherited deployment and credential settings. Neither
installation nor startup loads `.env`, `anchor.env` or `model.env`.

Model services, Plugin credentials, Graphs, channels, schedules and Python/Rust
initialization use the existing Host-managed WebUI flows. Once the required
sandbox baseline is available, missing model settings or an unprepared
Python/Rust environment do not prevent opening the WebUI for first configuration;
Pilot directs the user to Settings when needed.
The installer supports fresh roots only. The legacy Python deployment was
removed; no legacy inventory or migration tool is provided. This guide does not
establish an in-place upgrade or migration contract.

Every Host `serve` checks Bubblewrap's isolated namespaces and a fresh `/proc`
mount in its private PID namespace before the Host creates mutable directories,
cleans startup state, imports bundled Plugins, recovers Runs or binds HTTP.
Failure reports the cause and exits nonzero; it does not bind the host's proc,
share host namespaces or change Docker or host security policies as a fallback.
Passing this check establishes only the namespace/private-proc baseline, not the
network helper path or every Sandbox capability.
Source-build regression and ordinary Ubuntu Docker failure-path evidence are in
the [startup verification record](../.local/sandbox-startup-20261104/verification.md).

## Install a reviewed platform

Build with `scripts/build-platform-release.sh --goose PINNED_ELF --output NEW.tar.gz`.
The target needs Linux x86_64, Bubblewrap, slirp4netns, Git, shell, CA certificates,
a system linker for Rust development, and the libraries in the executable inventory.
Rust and Node are build-machine requirements. Verify the archive against the
trusted build report; the manifest is an inventory, not a signature.

For a private local installation:

```sh
scripts/install-platform.sh --archive /reviewed/platform.tar.gz \
  --sha256 TRUSTED_BUILD_REPORT_SHA256 --install-root /existing/parent/anchor
/existing/parent/anchor/release/anchor-runtime/bin/anchor-platform start
```

Open `http://127.0.0.1:8077`. Configure the default model in **Settings → Model
services**, then talk to Pilot. Model settings are private under
`data/state/managed/model-config.json`; language installations and other mutable
facts also remain below `data/`. The platform supplies fixed bootstrap tools;
the user authorizes downloads through the WebUI when preparing a language.
The listener is loopback-only; `start --listen 127.0.0.1:PORT` selects another
local port. Old environment files are ignored and left untouched.

## Run under systemd

The standard unit uses the same installed launcher and requires no environment
file. Prepare a fresh installation and give the service user its mutable data:

```sh
sudo useradd --system --home-dir /var/lib/anchor --shell /usr/sbin/nologin anchor
sudo install -d -o root -g root -m 0755 /var/lib/anchor
sudo scripts/install-platform.sh --archive /reviewed/platform.tar.gz \
  --sha256 TRUSTED_BUILD_REPORT_SHA256 --install-root /var/lib/anchor/platform
sudo chown anchor:anchor /var/lib/anchor/platform
sudo chown -R anchor:anchor /var/lib/anchor/platform/data
sudo install -o root -g root -m 0644 deploy/systemd/anchor.service /etc/systemd/system/anchor.service
sudo systemctl daemon-reload
sudo systemctl enable --now anchor.service
sudo systemctl status anchor.service
```

The unit protects the root-owned release and allows writes under the private
`data/` directory. It uses control-group shutdown so Host-managed children stop
with the Host. A custom local port can be set in an operator-owned systemd
drop-in by replacing `ExecStart` with the same launcher plus `start --listen`.
This does not change model or Plugin configuration.

An optional operator preflight reuses the launcher's exact command configuration.
It checks the fixed format-3 runtime inventory, selected executables, private
mutable roots, available system tools and writer lease. It does not start the
service, call a model or inspect credential values. Run it while the Host is
stopped; an active writer is a failure:

```sh
cargo build --manifest-path rust/Cargo.toml --release -p anchor-devtools --locked
sudo install -d -o root -g root -m 0755 /opt/anchor/bin
sudo install -o root -g root -m 0755 rust/target/release/anchor-devtools /opt/anchor/bin/anchor-devtools
sudo -u anchor /opt/anchor/bin/anchor-devtools preflight \
  --launcher /var/lib/anchor/platform/release/anchor-runtime/bin/anchor-platform
```

Preflight is optional and is not an `ExecStartPre` hook. The Host performs its
own required startup and writer-lock checks. Do not configure two Hosts to write
one state root. The flagless preflight remains available for explicit operator
process settings used by standalone Graph deployments; it requires no env file.

## Operator process settings

These settings belong to explicitly configured source/standalone Host processes.
The installed platform launcher supplies its own reviewed paths and controls,
and does not inherit arbitrary deployment or credential settings. This table is
not an environment-file template for the installed platform service. Source
startup and its Goose build/cache are documented in the
[development guide](development.md).

| Setting | Ownership and behavior |
| --- | --- |
| `ANCHOR_RUNNER_BUNDLE_ROOT`, `ANCHOR_RUNNER_GRAPH_NAME` | Optional deployment-managed entry bundle and Graph identity; HTTP cannot rewrite that Graph. Do not set them for ordinary editable business Graphs. |
| `ANCHOR_RUNNER_CATALOG_ROOT` | Editable Graph catalog. |
| `ANCHOR_RUNNER_STATE_ROOT` | Kernel Run facts, Sessions/Turns, schedules, channels and writer lock. |
| `ANCHOR_RUNNER_WORKSPACE_ROOT` | Node workspaces, inputs, Artifacts and native sessions. |
| `ANCHOR_RUNNER_LIBRARY_ROOT` | Canonical installed Plugin/tool Library; falls back to the catalog root, never to a guessed directory beside the bundle. Without a configured Library or catalog root, Plugin operations fail explicitly. |
| `ANCHOR_RUNNER_WEB_ROOT` | Compiled WebUI, such as `apps/web/dist`. |
| `ANCHOR_RUNNER_LISTEN` | Loopback only, default `127.0.0.1:8077`; remote access is described below. |
| `ANCHOR_RUNNER_ALLOWED_COMMANDS` | Explicitly authorized commands, such as `sh,git`; a model-supplied path does not grant access. |
| `ANCHOR_DEPENDENCIES_PYTHON`, `ANCHOR_PILOT_PYTHON_ENABLED` | Existing Python interpreter and optional Pilot authorization for a development deployment; full-platform users initialize through the WebUI. |
| `ANCHOR_DEPENDENCIES_UV`, `ANCHOR_DEPENDENCIES_UV_SHA256`, `ANCHOR_DEPENDENCIES_SOURCES` | Register a fixed dependency tool, digest and allowed sources together. The full platform supplies these through its release inventory. |
| `ANCHOR_MODEL_CONTEXT_WINDOW` | Optional positive context limit, passed to Goose as `GOOSE_CONTEXT_LIMIT` instead of model-name inference, which may be unreliable for aliases. |
| `ANCHOR_NODE_TOOL_DISCLOSURE`, `ANCHOR_NODE_ALWAYS_VISIBLE` | Force disclosure (`1`), disable it (`0`), or retain the automatic default; the second setting is a JSON array narrowing the always-visible set. See [Plugin disclosure](plugins.md#发现与读取). |
| `ANCHOR_PLUGIN_EXTERNAL_CONFIGURATION` | Explicitly deployment-managed Plugin fields; see [connection configuration](plugins.md#连接配置与凭据). |
| `ANCHOR_RETENTION_DAYS`, `ANCHOR_RETENTION_MAX_GB` | Defaults 30 days / 20 GB; `0` or `off` disables the respective limit. Startup and hourly reclamation use the existing protected deletion path; see [retention](design-cards/003-data-retention.md#8-批准与实现2026-10-23). |

Keep mutable roots outside the release and separate by responsibility. Before
listening, the Host validates resources, writable roots and the deployment-writer
lease; only one Host may write a state root. External tool environments and local
inputs require explicit registration/authorization. Existing interpreters do not
imply that all dependency tools are available; the WebUI reports missing tools
and keeps deployment-managed versions read-only.

Goose uses its own network namespace by default when `anchor-net-relay` is beside
the configured Goose binary, or at the absolute `ANCHOR_GOOSE_RELAY_BINARY` path.
The reviewed platform supplies the relay in `bin/`. The relay connects through a
UNIX socket to the Host bridge/model proxy; provider credentials stay on the Host.
Setting `ANCHOR_GOOSE_LOCAL_NETWORK=0`, or omitting the relay, selects shared host
networking and requires explicit `ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1`. This is
not loopback-only isolation and does not authorize business tools. Preflight
checks that the relay exists when isolated mode is selected; do not weaken
isolation to fix environment failures.

Model calls use the Host proxy by default; Goose receives a bridge address and
one-time token, not the provider endpoint/key. `ANCHOR_MODEL_DIRECT=1` is an
explicit temporary rollback for proxy diagnosis: it puts the endpoint and
credentials back in the Sandbox process. Model settings and credentials still
belong to Host-managed storage, never to a Graph/Plugin archive or repository.

## Channels and remote access

Install and configure the channel Plugin in the WebUI, then bind and enable its
message trigger for a saved Graph. The Host owns the gateway process, its private
control state and credential injection. Manage Sessions in the Run view. No
separate gateway systemd unit or environment template is required. Existing
separately managed gateways must be stopped before enabling the Host-owned path;
this guide does not switch a running legacy deployment automatically.

Anchor has no login: the Host refuses to start on a non-loopback listener, and a
leftover `ANCHOR_API_KEYS` is reported instead of being read. For remote access,
tunnel to the loopback listener or put an authenticated proxy in front of it and
apply the deployment's authentication and network boundary there. For Responses
streaming, disable proxy buffering and permit long-lived SSE connections.

## Health, failures and existing data

`GET /health` returns `{"status":"ok"}`. `GET /ready` must return HTTP 200 and
`{"status":"ready"}`; a missing or invalid selected Graph returns HTTP 503.
These checks do not call a model or establish external service availability.
Inspect `journalctl -u anchor.service` for Host and gateway failures.

Model endpoints, protocols, names and API keys come from Host-managed storage;
`ANCHOR_MODEL_URL/API_KEY/NAME/WIRE_API/ALIASES` are not configuration or fallback
sources. Existing managed configuration can prefill default address/protocol/Flash
before its first save, leaving Pro empty; saving at the original address can retain
the existing key. After saving, missing fields or keys fail explicitly instead of
falling back to the old service. Historical provider credentials for unfinished
tasks can be updated or cleared in the WebUI's collapsed credential section.
Old `.env` and `config/model.env` files are neither imported nor overwritten or
deleted; verify and save the current service in the WebUI. New installations do
not accept `--model-config`.

Keep existing installations, state and workspace roots intact. Do not replace an
existing installation with the fresh installer, copy an older release over live
state, bypass runtime pins, weaken sandbox controls or relax the writer lock.
Before an upgrade, stop the writer and make a consistent backup of the mutable
roots. Inspect saved Run status and external effects before
resuming; deployment rollback does not make external actions exactly-once.

Offline fixtures establish only their tested startup, inventory, configuration
and recovery behavior. They do not establish a production migration, public
WeCom delivery or real-provider business acceptance.
