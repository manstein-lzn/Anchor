# Feishu Gateway

Rust transport for enterprise self-built Feishu application bots. The Host
supervises one process per enabled Graph; the process does not run an Agent.
The gateway accepts `im.message.receive_v1` schema `2.0` events from users in
private `p2p` chats, with text, file or image content and the configured
application ID. It does not accept group messages, address-book access or
model-selected recipients. The Host can explicitly notify a task result's
original private conversation and send an authorized file there.

## Transport Contract

The gateway bootstraps at `/callback/ws/endpoint`, opens the returned outbound
WebSocket, and uses the protobuf frame defined by the official Go SDK's
`ws/pbbp2.pb.go`, `ws/model.go` and `ws/client_message.go`. It acknowledges after
durable admission, not after the Host or model finishes. Bounded split-frame
assembly, binary ping/pong and server-provided connection timing are supported.
Application credentials and event bodies are never logged.

Admission runs on a bounded blocking writer pool with an 800 ms wait budget;
admission plus ACK writing shares a two-second deadline. A timed-out admission
returns a failure ACK, and a late commit remains eligible for worker recovery.
Shutdown retains the directory lease until those writers have actually exited.

Message deduplication uses the incoming Feishu `message_id`, not the envelope
`event_id`. The Host receives `source:feishu`, `event_id:message_id`,
`sender_id:open_id`, `conversation_id:chat_id`, `reply_target:chat_id`,
`message_type:text|file|image`, metadata `chat_type:single` and
`request_id:message_id`. Both the receive list and the reply list must admit the
sender; empty lists grant no authority.

Media admission writes only the platform resource key, kind and safe filename
into the original event's attachment descriptor. It acknowledges before the
worker downloads anything. The worker fetches the authenticated
`/open-apis/im/v1/messages/{message_id}/resources/{key}?type=file|image` endpoint;
neither event URLs nor model URLs are accepted, and redirects are disabled.
Files are bounded at 20 MiB, images at 10 MiB (PNG, JPEG or WebP), and the Host
attachment envelope at 72 MiB. Downloaded bytes become
`{name,data_base64,media_type?}` only in the Host request, never in the transport
ledger. Retries download the original descriptor again; settlement retains the
original event identity and strips attachments as before.

Replies use the HTTPS message-reply API and an application tenant access token.
Final answers and text notifications are interactive JSON 2.0 cards with one
Markdown component, so headings, lists, code blocks, quotes and supported tables
remain rich text. Platform mention tags are neutralized rather than allowing
model-selected mentions; ordinary Markdown and code are not globally escaped.
Source text stays bounded at 20,480 UTF-8 bytes, and the serialized request,
including the nested card string, must fit a conservative 30,000-byte bound.
Messages are not silently split. Formatting is only a transport projection:
the logical digest of an existing text receipt is unchanged.
Only HTTP 200 and API `code:0` confirm an effect; message creation/reply also
requires a valid returned `data.message_id`. A final PATCH uses the card ID
already returned by its progress create, never a model-selected message.
A durable claim precedes final delivery. Unknown outcomes are settled as unknown
and never sent automatically again, including after restart. Host callback and
settlement retries reuse the neutral transport ledger extracted from WeCom;
there is no second delivery database or effect protocol.

The state directory is owner-only, protected against links, and exclusively
leased. The private `control.json` descriptor contains `{socket,token}` and
points to `control.sock`. The bounded, authenticated NDJSON control protocol
accepts `{"operation":"status","token":"..."}` and returns one of
`connecting`, `authenticating`, `authenticated`, `reconnecting`, or `stopped`.
The Host-only `send` operation takes `token`, `request_id`, `open_id`, `chat_id`
and Markdown `content`. The Host derives the bot,
recipient and conversation from the trusted active task result; the gateway
rechecks the send list and uses the message-create API with `receive_id_type=open_id`.
Both conversation identity and content bind the same existing ledger claim.
Confirmed reentry needs no platform/token request; changed identity/content
conflicts and unknown sends never replay. Control shutdown cancels network
waits but joins blocking ledger writes before releasing the state lease.

`send_file` takes the same trusted identity fields plus `name`, `data_base64`
and `md5`, never a filesystem path or external URL. The neutral transport file
validator checks nonempty bytes, the 20 MiB bound, filename and digest. The
gateway uploads official multipart fields `file_type:stream`, `file_name` and
`file` to `/open-apis/im/v1/files` before claiming a durable message send. An
upload failure has no message-send claim and can be retried. Its fixed, safe
error points to application-identity upload permission, app-version publication
and file constraints without echoing upstream error bodies or assuming every
failure is a permission problem. After a send claim,
an unknown result never replays. Confirmed reentry performs no upload or send.
Filename, SHA-256 of the bytes and original private recipient bind the existing
ledger claim. Images can be sent as downloadable files; inline outbound image
messages are not part of this slice.

## Live Activity

When `ANCHOR_CHANNEL_PROGRESS_URL` is configured, the shared read-only progress
subscriber runs beside the Host callback. One best-effort card per inbound
updates no faster than once per second (at most 60 tool updates during a
five-minute observation window). It renders the Host's bounded safe status
line, category and step, not Goose arguments, titles or thoughts. Status has
its own token cache and task. Duplicate admission and in-process worker retries
do not create another card.

The card explicitly sets `config.update_multi:true` at creation and every
update. Host/Goose settlement ends tool observation, not delivery. The final
reply worker cancels the subscription, joins the last bounded status request
under the per-card lock and takes exclusive ownership of its known message ID.
It replaces the entire card with the final Markdown answer, removing the
progress heading. There is no second answer message and no recall.
PATCH `/open-apis/im/v1/messages/{message_id}` carries only the serialized full
card in `content`; HTTP 200 plus `code:0` confirms the final update even though
`data` is empty. The same existing reply claim, logical content digest and Host
settlement cover this update, not the earlier best-effort progress creation.
No second delivery identity or store is introduced.

Handoff can wait for at most the current two-second status request. A missing
card ID or any failed/uncertain prior progress write uses the ordinary reply
API instead: an externally delayed status must never overwrite the answer.
If the final PATCH itself receives an explicit nonzero API code with HTTP 200
or 4xx, the same claim may fall back to one ordinary reply. A timeout, server
error, disconnect, malformed response or cancellation is unknown and never
falls back or automatically replays, including after restart. Once handed to
final delivery, the card cannot receive another status or shutdown update.
Only explicitly refused, unmodified cards can return to diagnostic formatting.
File and introduction messages are independent of this card's final content.

Stream closure or the tool-update bound stops observation without discarding
a safely known card. Supersession, callback failure, shutdown or expiry attempts
diagnostic terminal formatting only on cards not owned by final delivery.
Platform confirmation says the answer was accepted, not that its business
operation succeeded. Status mapping remains deliberately in-memory: a crash,
unknown create or platform outage can leave an old card, and recovery can show
another. There is no historical sweep or recall API. An unavailable platform
can also prevent diagnostic formatting. Existing unknown/no-replay and durable
settlement rules are unchanged.

## Application Permissions

Private event receipt requires `im:message.p2p_msg:readonly`; sending replies,
cards and files uses `im:message:send_as_bot`. Media resource download additionally
requires an application permission listed by that endpoint: `im:message`,
`im:message:readonly` or `im:message.history:readonly`. The receive-only private
event scope does not by itself authorize resource download. File upload requires
`im:resource` or `im:resource:upload`. Progress and final card PATCH accept the
existing send-as-bot scope (or the separate update scope); no new permission is
required for same-card final answers. Operators must explicitly select and
publish any additional permissions; the gateway does not grant or enable them.

## Configuration

Normally the Host supplies these values from the Graph's owner-bound channel
configuration. Never put application credentials in a Graph or plugin bundle.

| Variable | Purpose |
| --- | --- |
| `FEISHU_CHANNEL_STATE` | Absolute private state directory |
| `FEISHU_APP_ID`, `FEISHU_APP_SECRET` | Application identity and credential |
| `ANCHOR_FEISHU_USERS` | Explicit incoming `open_id` allowlist, comma-separated |
| `ANCHOR_FEISHU_SEND_USERS` | Explicit reply `open_id` allowlist, comma-separated |
| `ANCHOR_CHANNEL_CONTROL_TOKEN` | At least 32 bytes for the private control API |
| `ANCHOR_CHANNEL_WEBHOOK_URL`, `ANCHOR_API_KEY` | This Graph's authenticated Host callback |
| `ANCHOR_CHANNEL_PROGRESS_URL` | Optional authenticated read-only Host progress base URL; parsed once at startup |
| `ANCHOR_CHANNEL_CONTROL_SOCKET`, `ANCHOR_CHANNEL_CONTROL_DESCRIPTOR` | Optional assertions of the canonical control paths |
| `ANCHOR_FEISHU_API_BASE_URL` | Operator/fixture override; HTTPS, or HTTP on a literal loopback address only |
| `ANCHOR_FEISHU_CONNECT_MS`, `ANCHOR_FEISHU_HTTP_MS` | Connection and platform HTTP timeouts |
| `ANCHOR_FEISHU_HEARTBEAT_MS` | Initial binary-ping interval; server config may update it |
| `ANCHOR_FEISHU_RECONNECT_MS`, `ANCHOR_FEISHU_RECONNECT_MAX_MS` | Initial retry timing; server config may update it |
| `ANCHOR_FEISHU_RECOVERY_WINDOW_MS` | Undelivered inbound recovery window, default 15 minutes |
| `ANCHOR_CHANNEL_WEBHOOK_TIMEOUT_MS` | Optional Host callback timeout; omitted permits long model work |

No application credentials, real Feishu service or real model provider have
been exercised by this crate's deterministic fixtures. A connected listener is
not evidence that application publication, user availability, event subscription
or an actual private-chat reply has succeeded.
