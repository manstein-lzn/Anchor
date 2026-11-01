# Rust platform deployment

The format-3 platform archive contains the Host, fixed Goose, compiled WebUI,
network Plugin, network relay and fixed uv/Rustup/pixi bootstrap tools. The installed
`anchor-platform start` command derives release and private data paths from its
own location and clears inherited deployment and credential settings. Neither
installation nor startup loads `.env`, `anchor.env` or `model.env`.

Model services, Plugin credentials, Graphs, channels, schedules and Python/Rust
initialization use the existing Host-managed WebUI flows. Missing model settings
do not prevent opening the WebUI; Pilot directs the user to Settings when needed.
The installer supports fresh roots only. The legacy Python deployment was
removed; no legacy inventory or migration tool is provided. This guide does not
establish an in-place upgrade or migration contract.

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

Keep existing installations, state and workspace roots intact. Do not replace an
existing installation with the fresh installer, copy an older release over live
state, bypass runtime pins, weaken sandbox controls or relax the writer lock.
Before an upgrade, stop the writer and make a consistent backup of the mutable
roots. Inspect saved Run status and external effects before
resuming; deployment rollback does not make external actions exactly-once.

Offline fixtures establish only their tested startup, inventory, configuration
and recovery behavior. They do not establish a production migration, public
WeCom delivery or real-provider business acceptance.
