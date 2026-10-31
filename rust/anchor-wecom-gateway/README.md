# Rust WeCom Channel Transport

`anchor-wecom-gateway` is an operator-configured channel transport. It does not own Session/Turn, create Graph Runs, run an Agent, supervise its own process, or extract attachment contents.

## Plugin And Runtime Packaging

The Rust WeCom Plugin declares `bin/anchor-wecom-gateway` in
`plugins/wecom/channel.json`; the separate `bin/anchor-wecom-tools` MCP process
serves app API tools and is not the channel transport. When a Graph references
the WeCom Plugin, build a source-free runtime with both reviewed release ELFs:

```sh
anchor-distribution --host /reviewed/anchor-runner-host \
  --goose /reviewed/goose --bundle /reviewed/format-1-bundle \
  --wecom-tools /reviewed/anchor-wecom-tools \
  --wecom-gateway /reviewed/anchor-wecom-gateway \
  --output /existing/output/runtime.tar.gz
```

The builder embeds each binary at its declared Plugin entrypoint, updates the
frozen resource pin, and re-admits the bundle. Both flags are required only when
the Graph references `wecom`; passing either without that Plugin is rejected.
The builder does not start the Gateway or configure service supervision.

## Configuration

Configure `WECOM_CHANNEL_STATE`, `WECOM_BOT_ID`, `WECOM_BOT_SECRET`, `ANCHOR_CHANNEL_CONTROL_TOKEN`, and the inbound/send user allowlists. The default platform endpoint is `wss://openws.work.weixin.qq.com`; plain `ws` is allowed only for literal loopback IPs.

For inbound callbacks, configure `ANCHOR_CHANNEL_WEBHOOK_URL` and `ANCHOR_API_KEY`. Private chats accept text/mixed/image/file/voice; group mentions accept text/mixed and require the platform's `chatid`. The platform delivers group callbacks only when the bot is mentioned; the gateway does not infer mentions from text. Group members share the latest-message scope, while the real sender remains the allowlist and receipt identity. Replies use the original callback `req_id`; proactive group sends are not added. Unsupported callbacks and media the transport cannot fetch are rejected before Host admission. The gateway POSTs `{"event": <ChannelEvent>}` with bearer authorization using a client with no proxy, redirects, or implicit retries. Only HTTP 200 is accepted. Successful replies may include `text` or `reply`, `superseded: true`, and an optional receipt:

With progress enabled, an admitted callback still processing after the acknowledgement delay receives a `finish: false` stream update with the latest Host status, or `⏳ 正在处理` as a fallback. Status text stays on one line and changes in place only when the rendered status changes, at most once per second and 15 updates per turn. The gateway does not animate trailing dots or refresh unchanged text; the WeCom client owns its loading indicator and placement. A final answer replaces that same stream with `finish: true`; a Host or processing failure instead finishes it with a failure notice. Each correlated update is acknowledged before the next one is sent. The [official stream protocol](https://developer.work.weixin.qq.com/document/path/101463) does not allow updates after `finish: true`, so loading cannot be hidden by ending the stream early.

Official AI-bot image/file/voice callbacks carry short-lived download URLs whose payloads are AES-256-CBC encrypted. The gateway downloads and decrypts them itself (HTTPS, or loopback HTTP in tests; 16 items, 20 MiB per item and 50 MiB per event) and hands the Host the decrypted bytes through the Host's own attachment contract (`{"name","data_base64","media_type"}`). The temporary URL and AES key never reach the Host, and the ledger stores only the descriptors, so a recovered event re-downloads and fails honestly when the platform URL has expired. Mixed messages may interleave text with media items. See the [official callback message format](https://developer.work.weixin.qq.com/document/path/100719).

```json
{"text":"reply text","receipt":{"key":"channel-stable-id","content_sha256":"<64 lowercase hex>"}}
```

Webhook URLs are limited to 4096 bytes and require HTTPS, or HTTP with a literal IPv4/IPv6 loopback address (not `localhost`). Requests and responses are limited to 64 KiB; reply text is limited to 20,480 bytes; receipt keys are 9–256 ASCII bytes (`channel-` plus a non-empty stable identifier); bearer keys are limited to 4096 bytes. Receipt objects accept exactly `key` and `content_sha256`. A receipt requires text or `superseded: true`. Consumers that omit receipts retain the original callback/reply flow and receive no settlement callback.

## Durable Settlement

SQLite schema v1 upgrades transactionally to v2 while preserving existing delivery facts. Before platform dispatch, the gateway durably claims the reply as `unknown`. After a correlated ACK, the outcome and settlement outbox are committed together; the `confirmed` ledger fact is durable before settlement HTTP begins.

Settlement is POSTed to the same configured webhook URL as the original event, with the original identity-bearing event:

```json
{"event": <ChannelEvent>, "settlement":{"key":"channel-stable-id","content_sha256":"<64 lowercase hex>","status":"confirmed"}}
```

`status` is exactly `confirmed`, `unknown`, or `suppressed`. ACK rejection, timeout, disconnect, and restart-unknown use `unknown`. A reply known not to have been dispatched because a newer message superseded it uses `suppressed` and is sent through the outbox too.

Only HTTP 200 acknowledges a settlement. Other statuses, transport errors and timeouts leave it in the durable outbox for bounded exponential-backoff retry, including after restart. Retrying settlement never resends the platform message. If the Host accepted a settlement but the gateway crashed before persisting the HTTP success, the same settlement may be POSTed again; the stable receipt key identifies that retry.

Disconnect, rejection, timeout, process kill, and late ACK remain unconfirmed; restart never automatically resends an unknown platform request. This is conservative at-most-once dispatch, not exactly-once external delivery. If callback processing is interrupted before a durable reply claim exists, restart retries the same Host event; Host admission must therefore remain idempotent. If a reply claim already exists, restart does not call the Host or resend that platform reply.

## Proactive Files

The private control socket accepts one file through the same configured bot and
`send_users` allowlist as proactive Markdown:

```json
{"operation":"send_file","token":"<control token>","request_id":"<stable delivery id>","userid":"<recipient>","name":"AI每日晨报_20261031.pdf","data_base64":"<file bytes>","md5":"<lowercase digest>"}
```

Files range from 5 bytes to 20 MiB; filenames are at most 256 UTF-8 bytes. The control request allows the corresponding
base64 body plus a 64 KiB envelope; text/status requests keep their 64 KiB limit.
The gateway checks filename, bytes and digest, then uses its existing
three-stage media upload on the active connection and sends `aibot_send_msg`
with `msgtype=file`. It returns `accepted=true` only after the final message ACK,
not after upload. The stable request ID binds recipient, filename and bytes;
confirmed retries return `duplicate=true` without uploading again.
This envelope matches the official SDK's
[`sendMediaMessage(chatid, 'file', mediaId)`](https://github.com/WecomTeam/aibot-node-sdk)
implementation; upload bounds follow the
[long-connection protocol](https://developer.work.weixin.qq.com/document/path/101463).

Upload failure or disconnect before dispatch creates no delivery claim and can
be retried with the same identity. Once the final send is claimed, rejection,
timeout, disconnect or restart leaves the existing `unknown` fact and prevents
automatic resend. File bytes are not copied into a second durable store; the
Host owns the source Artifact and any conversation handoff. Local transport
tests cover these contracts; they do not establish public WeCom delivery.

## Verification

Run the crate's unit, local WebSocket/HTTP transport, and process tests with:

```sh
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-wecom-gateway --all-targets --locked
```

These tests do not demonstrate public WeCom delivery, Host process supervision, or production deployment. The Host owns Session/Run replacement and cancellation; the Gateway suppresses stale replies and recovers durable callback/delivery facts but does not supervise its own process.
