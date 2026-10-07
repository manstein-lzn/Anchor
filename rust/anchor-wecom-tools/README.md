# WeCom application API tools

Rust-native stdio MCP implementation of `wecom_send_text`, `wecom_send_markdown`
and `wecom_get_user`, using the fixed rmcp 2.2 server and stdio transport. Tool
names, descriptions and input schemas match `plugins/wecom/server.py`.

The binary defaults to stdio MCP. Configuration comes only from
`WECOM_CORP_ID`, `WECOM_AGENT_ID`, `WECOM_SECRET` and `WECOM_API_BASE_URL`
(default `https://qyapi.weixin.qq.com`). It does not read `.env`, channel
credentials or proxy environment variables. Credentials are checked when a
tool needs them; member queries do not require an agent ID. HTTP requests have
a 20-second timeout, with neither automatic retries nor redirects.

Token refresh uses a shared async lock and a 60-second expiry margin. API
failures report numeric error codes, not remote error text or request URLs.
Transport, HTTP and invalid-response failures after a message POST report
unknown delivery; they do not imply either success or safe resending. Results
are MCP text content containing JSON, with `isError`; unknown tools return the
MCP invalid-params protocol error.

## Independent Plugin package

```sh
anchor-wecom-tools package-plugin /existing/parent/new-plugin
```

The destination must be new and its parent must already exist without symlink
components. Packaging preserves the original Plugin metadata and environment
declarations and sets the MCP command to `bin/anchor-wecom-tools`, with empty
arguments and `cwd="."`. The only files are:

- `plugin.json`
- `skills/wecom/SKILL.md`, unchanged from the original Plugin
- `bin/anchor-wecom-tools`, a verified copy of the running executable

The package contains no Python entrypoints, `.env`, runtime credentials or
`channel.json`. The unchanged skill also describes the existing Python channel;
that channel is deliberately not part of this application-API package.

Packaging stages the files alongside the destination, checks binary content
and source identity, then publishes with a Linux atomic no-replace rename.
Existing files, directories and symlinks are never overwritten. Pre-publication
failure removes staging rather than leaving a partial destination.

## Validation

Tests use only loopback HTTP fixtures and real stdio subprocesses. They cover
schemas, arguments/configuration, token caching/expiry/concurrent refresh,
HTTP/API/transport errors, single-effect POST failure, native package contents,
source identity changes and concurrent no-replace publication. No real WeCom
service, business messaging, model or long-lived WebSocket gateway is exercised.

After the shared workspace lockfile is integrated by the main track:

```sh
CARGO_TARGET_DIR=/tmp/anchor-rust-platform-HEW1JK/target-wecom CARGO_BUILD_JOBS=2 cargo +stable test --offline --manifest-path rust/Cargo.toml -p anchor-wecom-tools
CARGO_TARGET_DIR=/tmp/anchor-rust-platform-HEW1JK/target-wecom CARGO_BUILD_JOBS=2 cargo +stable clippy --offline --manifest-path rust/Cargo.toml -p anchor-wecom-tools --all-targets -- -D warnings
cargo +1.92.0 fmt --manifest-path rust/Cargo.toml -p anchor-wecom-tools -- --check
```
