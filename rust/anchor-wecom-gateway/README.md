# Rust WeCom Text Transport

`anchor-wecom-gateway` is an operator-configured channel transport. It does not own Session/Turn, create Graph Runs, run an Agent, supervise the platform, or provide attachment extraction and upload.

## Configuration

Configure `WECOM_CHANNEL_STATE`, `WECOM_BOT_ID`, `WECOM_BOT_SECRET`, `ANCHOR_CHANNEL_CONTROL_TOKEN`, and the inbound/send user allowlists. The default platform endpoint is `wss://openws.work.weixin.qq.com`; plain `ws` is allowed only for literal loopback IPs.

For inbound callbacks, configure `ANCHOR_CHANNEL_WEBHOOK_URL` and `ANCHOR_API_KEY`. The gateway POSTs `{"event": <ChannelEvent>}` with bearer authorization using a client with no proxy, redirects, or implicit retries. Only HTTP 200 is accepted. Successful replies may include `text` or `reply`, `superseded: true`, and an optional receipt:

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

Disconnect, rejection, timeout, process kill, and late ACK remain unconfirmed; restart never automatically resends an unknown platform request. This is conservative at-most-once dispatch, not exactly-once external delivery. Callback processing interrupted before its result is durably recorded is not replayed.

## Verification

Run the crate's unit, local WebSocket/HTTP transport, and process tests with:

```sh
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-wecom-gateway --all-targets --locked
```

These tests do not demonstrate public WeCom delivery, attachments, Session supervision, or production cutover.
