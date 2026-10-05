# anchor-io-harness-runtime

This workspace crate is the canonical Rust-native Anchor boundary for
`io-harness = 0.86.0` and `rig-core = 0.43.0`.

The layering is deliberate:

- `adapter`: converts io-harness provider requests/responses to Rig;
- `completion`: adapts native `final_result` calls into Harness completion values;
- `node`: exposes Anchor `ToolPort` and owns the io-harness SQLite backend;
- `node_exec`: freezes an Anchor `NodeRequest` into a TaskContract and only
  validates a completed `{summary, route?}` result;
- `node_port`: resolves host-owned workspaces/tools asynchronously and maps
  execution facts to Graph `NodeExecutionPort`.
- `recording`: publishes private per-call observations using native `Record` files.

io-harness owns the only Agent loop. Rig is only the provider transport. Anchor
continues to own Graph/Run/Artifact/Plugin/Sandbox facts. Provider or recovery
states whose external outcome is unknown remain fail-closed with a resumable
io-harness run id.

AgentNode completion uses a native `final_result` output tool with `summary` and
optional `route`. The provider adapter projects that call into an internal JSON
value for Harness validation; plain text, including JSON text, cannot complete a
new model turn. Extra arguments are ignored, null routes mean no route, and invalid
summaries/routes use Harness's existing correction turns. The output-tool name is
reserved. This adapter's output-schema path is for Anchor node completion, not
arbitrary structured result types.

A turn mixing completion with business calls keeps all business calls in order
and records the premature completion as deferred. After checking their results,
the model must submit a fresh completion alone. Duplicate or truncated completion
calls cannot finish. There is no pending completion latch or additional Agent loop.

The existing Harness `step_turns.text` retains an adapter-owned `_anchor_completion`
projection of received calls alongside canonical summary/route. It is evidence of
the protocol conversion, not a second completion authority. Legacy persisted
completed values remain readable without that optional projection. Finished runs
reopen without another model call; interrupted execution still follows Harness
checkpoints and tool recovery. Cancellation remains subject to existing Harness
step boundaries; this change does not promise interruption of the final turn.

Completion-mode streaming is buffered until the full response arrives and emits
the same canonical value returned to Harness. Incremental user-facing summary
streaming is not implemented by this bridge. Requests without output schemas keep
their existing text streaming behavior.

Production NodePort records every model attempt under the existing io-store:
`np1-<InvocationKey hash>.recordings/<20-digit sequence>/`. Reopening an invocation
reserves the next directory and never overwrites an earlier attempt. The host can
use `recording::directory(io_store_root, key)` when deleting a Run's data.

Each attempt contains `request.json` (io-harness request) and `rig-request.json`
(typed request after Anchor injects the native completion tool). A successful
response adds `rig-response.json`, excluding Rig's provider-specific `raw` document,
and `recording.json` written by io-harness 0.86 `provider::Record::save`. The latter
retains the native response before Anchor's completion conversion, including
`final_result` arguments, tool results in subsequent requests, and usage. These
are typed Provider/Rig observations, not an HTTP byte recording. A native `Replay`
can read the exchange; running its native completion response requires the same
Anchor completion projection, so these files are not a replacement Node backend.

Requests are synced before transport starts and successful exchanges are synced
immediately after each call. `outcome.json` records only `succeeded`, `failed`, or
`recording_incomplete`, with an error category and no raw error/endpoint/header.
A directory with a request but no outcome has no recorded terminal result: it
may be in flight, interrupted, or have lost a diagnostic write. It does not prove
whether the provider received or answered the request. Partial stream content is
not recorded as a successful response. Post-response recording errors do not
change a returned turn or trigger retries; storage errors before sending prevent
the call. Recordings do not authorize replay or modify Harness recovery facts.

Unix files are created 0600 and attempt directories 0700. Transport credentials
are never copied into the recording. Prompts, model content, and tool results
remain intact and may themselves contain sensitive business data. Existing Runs
have no backfilled request history. External-provider, process-kill, and browser
acceptance are separate from the controlled fixture tests.

The former `experiments/io-harness-{adapter,node,node-exec,node-port}` packages
are compatibility wrappers around these modules. They remain useful as stable
historical entry paths and do not contain a second implementation.

### Native conversations

An optional `NodeHostResolver::conversation_hint` supplies a trusted stable key.
The Host derives it from canonical bundle identity, Session and full node ID.
The bridge uses io-harness 0.86 `Session`/`Store`, with a stable framework root and
an independent current-Run sandbox workspace. There is no second history engine.
`with_conversational_turns(false)` preserves Anchor's bounded completion contract.

Each invocation records its exact native Session/run/turn locator and admission.
Reopening repairs interrupted admission publication before continuing; it does
not select the newest native row. `trace_messages` supports both ordinary and
conversation invocations, including unrecorded open tool attempts explicitly marked
as having an unknown external outcome. Per-call provider recordings stay separate
from the native Store and retain their existing limitations.

`node_port::remove_conversation(io_store_root, hint)` acquires the same conversation
lock and deletes that native scope, locators and recordings. The Host calls it only
for a whole Graph cascade after checking execution and ownership. It preserves
other scopes and the lock inode; a later conversation with the same key starts
without the deleted history. Anchor Run/Artifact cleanup remains the Host's job.

### Host-frozen images

`NodeHostResolver::prompt_images` supplies validated immutable `NodeImage` bytes.
The execution bridge attaches native `Media` to the same TaskContract for start,
resume, recovery and Session turns. Host admission owns decoding, order and size
limits; the bridge preserves bytes without transcoding. It uses the public Media
value because `Media::image` applies a vendor-agnostic 5 MiB convenience limit,
while this Host supports 10 MiB over Rig's OpenAI wires. The native request-wide
20 MiB image limit still applies.

Model registries default to text-only; explicit image capability is frozen with
the invocation model binding and checked on resume. Images are rejected before
any model request if the selected model has no declared image capability.
