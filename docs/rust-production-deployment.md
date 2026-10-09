# Rust Production Deployment

This deploys the source-free `anchor-runtime/` archive produced by
`anchor-distribution`, using the Rust `anchor-runner-host serve` binary directly.
The Host unit launches the Rust binary directly. Host startup checks
required roots, loads and validates the configured Graph bundle, probes writable
roots, and acquires the state-root `deployment-writer` lock before listening.
The archive must include the built WebUI at `web/index.html`, selected through
`ANCHOR_RUNNER_WEB_ROOT` in the protected environment file.
The native `anchor-devtools preflight` is optional operator-time tooling, outside
the service lifecycle.

## Install

Use a dedicated `anchor` system user. Verify the archive SHA256 against the
trusted build report before extracting it. Extract each reviewed release into
`/opt/anchor/releases/<release-id>/` so it contains
`anchor-runtime/bin/anchor-runner-host`, `bin/goose`, `bundle/`, `web/index.html`,
and `runtime-manifest.json`. Build the package with `--web apps/web/dist`; the
distribution builder otherwise permits packages without Web assets. Keep the
release tree root-owned and immutable to the service user. For example:

```sh
RELEASE_ID=2026-10-08.1
ARCHIVE=/absolute/path/to/anchor-runtime.tar.gz
EXPECTED_SHA256=<trusted-build-report-sha256>
printf '%s  %s\n' "$EXPECTED_SHA256" "$ARCHIVE" | sha256sum --check -
sudo install -d -o root -g root -m 0755 "/opt/anchor/releases/${RELEASE_ID}"
sudo tar -xzf "$ARCHIVE" -C "/opt/anchor/releases/${RELEASE_ID}"
sudo chown -R root:root "/opt/anchor/releases/${RELEASE_ID}"
sudo chmod -R go-w "/opt/anchor/releases/${RELEASE_ID}"
sudo ln -sfnT "/opt/anchor/releases/${RELEASE_ID}" /opt/anchor/current
```

The unit's `ExecStart` intentionally follows `current`, but the bundle, WebUI,
and Goose paths in `/etc/anchor/anchor.env` must name the selected release's
concrete paths, not paths through `current`. Replace
`REPLACE_WITH_RELEASE_ID` in all three environment values before use. For each
release, stop the Host and optional gateway, point `current` at the new release,
update all three paths in `anchor.env`, run preflight against that same release,
then start the services. Keep the old release and its state untouched for
rollback. Keep the packaged bundle immutable to the Host; Graph and runtime
updates arrive through a reviewed release and an operator-controlled release
switch, never by mutating the live bundle.

Create the service-owned writable roots and protected environment file:

```sh
sudo useradd --system --home-dir /var/lib/anchor --shell /usr/sbin/nologin anchor
sudo install -d -o anchor -g anchor -m 0750 /var/lib/anchor
sudo install -d -o anchor -g anchor -m 0750 /var/lib/anchor/state /var/lib/anchor/workspaces /var/lib/anchor/catalog
sudo install -d -o root -g root -m 0755 /etc/anchor
sudo install -o root -g root -m 0600 deploy/systemd/anchor.env.example /etc/anchor/anchor.env
```

Edit `/etc/anchor/anchor.env`: set the actual model URL, API key and model name;
review the graph's exact `ANCHOR_RUNNER_ALLOWED_COMMANDS`; install Bubblewrap,
Git and any explicitly required Plugin dependencies. Goose is the Anchor-built lean
ACP binary from pinned upstream v1.53.0 sources (`scripts/build-goose-acp.sh`) with
SHA256 `71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7`.
The template binds to loopback and permits an empty API-key list only in that
case. Non-loopback binds require unique bearer secrets of at least 32 bytes and
must be protected by the deployment's network/authentication boundary.

Optionally install and run the Rust operator-time preflight outside the Host
service lifecycle. `systemd-run` supplies the protected EnvironmentFile to a
transient process running as the `anchor` user; this is not an `ExecStartPre`
hook and does not contact Goose, a model, or a provider. Set `RELEASE_ID` to
the exact release currently selected by `/opt/anchor/current`:

```sh
sudo install -d -o root -g root -m 0755 /opt/anchor/bin
sudo install -o root -g root -m 0755 rust/target/release/anchor-devtools /opt/anchor/bin/anchor-devtools
RELEASE_ID=2026-10-08.1
RUNTIME_ROOT="/opt/anchor/releases/${RELEASE_ID}/anchor-runtime"
sudo systemd-run --wait --pipe --collect --uid=anchor \
  --property=EnvironmentFile=/etc/anchor/anchor.env \
  --setenv=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  --setenv=ANCHOR_ENV_FILE=/etc/anchor/anchor.env \
  --setenv="ANCHOR_DEPLOYMENT_ROOT=${RUNTIME_ROOT}" \
  --setenv=ANCHOR_RUNNER_HOST_BINARY=/opt/anchor/current/anchor-runtime/bin/anchor-runner-host \
  /opt/anchor/bin/anchor-devtools preflight
```

Build `anchor-devtools` with `cargo +stable build --manifest-path rust/Cargo.toml
--release -p anchor-devtools --locked` before installing it; if a custom target
directory is configured, use that build's path. It is an operator tool and is not
required in the runtime archive.

The service and transient preflight use the same explicit `PATH`, so `sh` and
`git` availability checks reflect the Host's command environment. The command
passes the same Host binary path used by the unit's `ExecStart`; preflight
resolves that supplied path and requires it to be the Host binary in the
selected concrete release. It does not parse the installed unit, so keep the
unit's `ExecStart` and `ANCHOR_RUNNER_HOST_BINARY` argument aligned. The Rust
Host repeats its required roots, bundle, and writer-lock checks itself.
Enable the standard service:

```sh
sudo install -o root -g root -m 0644 deploy/systemd/anchor.service /etc/systemd/system/anchor.service
sudo systemctl daemon-reload
sudo systemctl enable --now anchor.service
sudo systemctl status anchor.service
```

When run, the optional preflight checks protected env-file mode and secret
presence without opening or printing secret values, endpoint syntax without
resolving DNS, concrete non-symlink runtime/bundle paths, owned/private writable
roots, the non-overlapping state and workspace roots, required local
executables, runtime inventory file hashes, fixed Goose version/hash/static-musl
identity, Host ELF runtime metadata, the selected Host executable identity, and
whether the `deployment-writer` lock is free. It requires writable, service-owned
state, workspace, and catalog roots, with all three outside the runtime package.
If absent, it
creates only the `deployment-locks/.deployment-writer.lock` scaffolding under
the state root while checking the lock; it does not modify Run or Graph data.
The Host remains authoritative for parsing and validating secret values and
API-key structure.
The Host acquires and holds that lock for its lifetime, so concurrent starts
fail closed even if they race after the optional check. Never configure two
Hosts to write one state root.

## WeCom gateway lifecycle

When the reviewed Graph bundle mounts the `wecom` Plugin, the Rust Host
automatically discovers its `channel.json` and supervises the packaged
`anchor-runtime/bin/anchor-wecom-gateway`. It injects the required
`WECOM_BOT_ID` and `WECOM_BOT_SECRET` from the Host environment, creates the
private state directory below `/var/lib/anchor/state/channels/wecom`, supplies the
loopback callback and API key, restarts an unexpectedly exited child after a
delay, and sends `SIGTERM` plus waits for it during Host shutdown. The gateway
publishes `/var/lib/anchor/state/channels/wecom/control.json`; Host channel tools use
that state-root path by default. An explicit
`ANCHOR_CHANNEL_CONTROL_DESCRIPTOR` is accepted only when it names the same
path, so a stale or separately managed endpoint cannot be silently selected.

Configure `ANCHOR_WECOM_GRAPH`, `ANCHOR_WECOM_REPLY_NODE`, `WECOM_BOT_ID`,
`WECOM_BOT_SECRET`, `ANCHOR_WECOM_USERS`, and `ANCHOR_WECOM_SEND_USERS` in
`/etc/anchor/anchor.env`. The callback is
`http://127.0.0.1:8077/channels/wecom/events`; the Host supplies the API key
used by the child from `ANCHOR_API_KEYS` (or a private loopback-only key when
the Host has no API keys). Keep the control token child-only; it is generated
by the Host and never placed in the Host environment.

Do not enable `anchor-wecom-gateway.service` together with a Graph bundle that
mounts `wecom`: that compatibility unit would create a second gateway and
violate the single-owner contract. `deploy/systemd/anchor-wecom-gateway.service`
and its environment template remain only for a separately reviewed special
deployment whose Graph does not declare the channel and whose operator has
explicitly disabled the automatic path. The standard Rust package needs only
`anchor.service`.

The gateway still fails closed on missing credentials, an unsafe state
directory, an active gateway lock, invalid callback configuration, or
unavailable private control state. `systemctl is-active anchor.service`
reports process state, not successful WeCom authentication or callback
delivery; inspect the Host journal. No live WeCom calls are part of the
fixture checks.

## Health and readiness

Health is `GET http://127.0.0.1:8077/health` and returns `{"status":"ok"}`.
Readiness is `GET http://127.0.0.1:8077/ready`: require HTTP 200 and
`{"status":"ready"}`. A valid `/health` response alone is liveness, not
readiness; `/ready` returns HTTP 503 with `{"status":"not_ready"}` when the
configured Graph is unavailable or invalid. If API keys are configured, send
`Authorization: Bearer <key>` to both paths. These checks do not make a model
request or prove external provider health. Place an authenticated reverse proxy
in front if remote access is needed.

For `POST /v1/responses` with `stream: true`, configure the proxy to disable
response buffering and allow long-lived SSE connections; otherwise text deltas
may be delayed until the response completes. The endpoint implements the
documented Responses subset, not the complete OpenAI Responses API.

## Failure and rollback

Inspect `journalctl -u anchor.service` and, when enabled,
`journalctl -u anchor-wecom-gateway.service`. Fix required roots, bundle,
permissions, or lock availability; do not bypass Host startup validation or
relax the single-writer lock, Goose pin check, or sandbox command list. To roll
back, stop the enabled Host and gateway units, point `/opt/anchor/current` to
the previous reviewed release, optionally run the operator preflight, then
start the Host and configured gateway. Keep state and workspace
roots outside release directories and do not restore an older release over
current state. Back up state before upgrades; restore state only from a
consistent backup while the service is stopped. If a Run was active at failure,
inspect its persisted status and effects before resuming: external side effects
are not exactly-once and are never automatically replayed by deployment
rollback. Keep the previous package, archive SHA256, environment backup and
state backup until the replacement passes operational acceptance.

## Checks

The fixture tests are offline and do not invoke Goose, a model, a provider, or
WeCom. They validate optional preflight behavior, unit configuration, and Host
startup checks, not a production deployment.
