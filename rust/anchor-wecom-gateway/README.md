# Rust WeCom Text Transport

`anchor-wecom-gateway` is an operator-configured channel transport. It does not own Session/Turn, create Graph Runs, run an Agent, supervise its own process, or provide attachment extraction and upload.

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

For inbound private-message callbacks, configure `ANCHOR_CHANNEL_WEBHOOK_URL` and `ANCHOR_API_KEY`. Group callbacks and media are rejected before Host admission. The gateway POSTs `{"event": <ChannelEvent>}` with bearer authorization using a client with no proxy, redirects, or implicit retries. Only HTTP 200 is accepted. Successful replies may include `text` or `reply`, `superseded: true`, and an optional receipt:

Official AI-bot image/file callbacks carry short-lived download URLs whose payloads require callback AES-key decryption; this gateway does not fetch or decrypt them. Mixed messages containing media are also rejected. The Host's normalized attachment contract is separate and does not make these upstream callbacks compatible. See the [official callback message format](https://developer.work.weixin.qq.com/document/path/100719).

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

## Verification

Run the crate's unit, local WebSocket/HTTP transport, and process tests with:

```sh
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-wecom-gateway --all-targets --locked
```

These tests do not demonstrate public WeCom delivery, attachment handling, Host process supervision, or production cutover. The Host owns Session/Run replacement and cancellation; the Gateway suppresses stale replies and recovers durable callback/delivery facts but does not supervise its own process.
