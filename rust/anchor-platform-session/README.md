# Rust platform Session facts

`SessionStore` owns platform Session/Turn metadata, lifecycle and delivery events.
It does not open Python stores, create conversations in a framework database, own
Agent execution or copy Run state, Agent messages, Harness logs or recovery records.

## Host contract

The crate root exports `SessionStore`, `CreateSession`, `Session`, `SessionStatus`,
`SessionEvent`, `Turn`, `TurnStatus`, `TurnEvent`, `NativeExecution`, `GooseExecution` and
`SessionError`. The store is synchronous, `Clone + Send + Sync`;
clones share a connection, while independent `open` calls use independent SQLite
connections. An async HTTP adapter can call it through its blocking executor.

Every operation takes a trusted host `owner`, never a body-provided identity.
`get`, `rename`, `set_status`, `attach_run` return `Session`; `create` takes
`CreateSession`, `list` returns `Vec<Session>`, `events(owner, id, after)` returns
`Vec<SessionEvent>`, and `delete` returns `()`, all inside `Result<_, SessionError>`.
An inaccessible or absent Session returns the same `Missing` error. IDs are
globally unique: even a different owner receives `Conflict` on duplicate creation,
without overwriting or adopting any existing data. The owner is not in the DTO.

Request fields all default, including an optional ID (generated UUID when absent).
The initial DTO matches the legacy Session shape: conversation ID equals Session
ID; approval/operation are null and approvals/questions/operations are empty.
Status uses `active`, `waiting_user`, `interrupted`, `archived`. Archived Sessions
cannot transition back to any other status. Titles are trimmed and limited to
120 Unicode scalar values; an initial empty title is allowed, rename requires
1–120. Identity strings use 1–256 UTF-8 bytes of alphanumeric characters or
`_-.:@`; `.` and `..` are forbidden.

`list` sorts by last update descending, with ID as tie-breaker. Activity names are
`session.created`, `session.renamed`, `session.<status>` and `run.attached`.
Sequences start at 1, are monotonically increasing **within each Session**, and
`after` is exclusive. Each mutation commits its snapshot and event in one immediate
transaction. Repeated attachments preserve the snapshot and append no event.
Deletion conflicts while any run IDs are retained; otherwise it atomically removes
the Session, Turns and delivery/activity events, unless a Turn is running. Native
Harness files have a separate ownership boundary. This is a snapshot store, not event sourcing.

## Turn contract

`create_turn(owner, session, request_id, prompt)` atomically admits one running
Turn and updates Session activity. The same request ID and exact input returns
the same Turn, even after completion; changed input or another running Turn
conflicts. Request IDs use 1–128 UTF-8 bytes and prompts at most 64 KiB; `None`
represents an explicit continuation request, not an automatically replayed tool.
Only ordinary Pilot Sessions are admitted by this slice.

`get_turn`, `list_turns`, `append_turn_event`, `turn_events` and `finish_turn`
remain owner-aware. Turn status is `running`, `completed`, `failed`, `stopped` or
`interrupted`. Delivery sequences are local to each Turn and `after` is exclusive.
Terminal delivery is immutable, so a terminal SSE reader cannot miss a late append.
Finishing a Turn commits its status, Session projection and lifecycle events
together. Status changes and deletion cannot invalidate a running Turn.

`interrupt_running()` is an explicit deployment-writer startup operation. It
marks leftover business Turns interrupted without rerunning the model, changing
native Harness outcomes or manufacturing a reply. Opening the store does not
perform startup interruption. Accepted prompt metadata and UI chunks are not
an alternative Agent transcript; io-harness owns the latter.

## Native form question facts

`Question`, `QuestionStatus`, `QuestionAnswer` and `QuestionAction` are public DTOs.
`create_question(owner, session, turn, message, requested_schema)` requires an
owned running Turn and admits exactly one pending question per Turn. The Turn
remains `running`, so its SSE stream stays live; the Session becomes `waiting_user`
with the question message as its reason. `get_question` and `list_questions` are
owner-, Session- and Turn-scoped. The legacy `Session.questions` placeholder is
unchanged; consumers use the dedicated question methods rather than another copy
of the facts.

`answer_question` atomically changes `pending` to `answered`, records an
`accept`, `decline` or `cancel` answer and returns the Session to `active` without
creating another Turn. A different answer conflicts. An identical answer retry
returns the saved historical question without new events or mutations, including
after Turn completion, interruption or a later Turn starting. Pending questions
require a live Turn; interrupted questions always reject late answers. Unknown
answer fields are rejected on deserialization; decline and cancel cannot carry
content. Accept requires an object satisfying the requested form schema, and
undeclared fields are always rejected even when `additionalProperties` is absent.

`validate_question_schema(&Value)` is shared by trusted Host adapters. It supports
flat string, integer, number and boolean properties, primitive enum values,
title/description/default annotations, required fields, string length/pattern/
email/URI/date/date-time constraints, numeric range/multiple constraints and
object property-count bounds. Nested schemas, references and unsupported keywords
or types fail closed. Schemas and serialized answers are limited to 64 KiB;
nonblank question messages use the same UTF-8 byte limit. Validation uses pinned
`jsonschema` with default features disabled, an external-resource-denying
retriever and bounded regex execution.

Question mutations, the Session snapshot and delivery/activity events commit in
one immediate transaction. SSE data is `{type:"question", question}`,
`{type:"question-answered", question}` or `{type:"question-interrupted", question}`.
`finish_turn` and `interrupt_running` interrupt pending questions before the
terminal SSE event in the same transaction and preserve answered questions.
Opening the store alone does not claim to restore a live Goose process or its
in-memory pending elicitation. Schema version 5 adds the questions table and
single-pending partial index while preserving version 1–4 facts and events.

## Channel Session admission

`admit_channel_inbound(owner, request)` owns the durable admission fact for a
channel identity `(source, account, conversation_id, sender_id)`. The identity
is scoped by the trusted owner and is bound to one Graph/reply node. It creates
or reuses one channel Session, admits one serialized Turn, and records the
inbound-to-Session/Turn/Run relation. Repeating an inbound with the same
immutable text and attachment manifest is read-only; changing the manifest,
text, Graph or reply node fails closed. `associate_channel_run` can attach the
Run after Host execution starts. Ordinary `create_turn` cannot use channel
Sessions.

Attachment manifests are metadata only: each entry freezes a safe workspace
path, filename, lowercase SHA-256, byte size and optional media type. The store
does not fetch or verify the file bytes; the Host must materialize and validate
the workspace before admission.

`admit_channel_delivery`, `begin_channel_delivery` and
`settle_channel_delivery` persist idempotent outbound facts. A restart converts
`sending` to `unknown` through `recover_channel_deliveries`; replacement
suppresses pending/failed deliveries and marks in-flight delivery outcomes
unknown. `delete_channel_session` refuses running Turns, retained Runs and
unfinished deliveries. These APIs are storage contracts only: no channel
supervisor, send loop, WeCom transport, webhook verification, Graph execution
or Host wiring is implemented in this crate. Schema version 6 adds the channel
tables transactionally on top of versions 1–5.

## Execution associations

Turn JSON includes `native: Option<NativeExecution>`, `goose: Option<GooseExecution>`
and `runs: Vec<String>`;
missing fields deserialize as `None` and an empty vector for older JSON. Every
Turn read, list, admission retry and finish projects the durable relationship
tables rather than guessing associations from Session run IDs.

`bind_native(owner, session, turn, execution)` returns `Result<Turn, SessionError>`.
`NativeExecution` has `scope: String` (64 ASCII hex characters), `session: i64` and
`run: i64` (both positive). First binding requires a running Turn. Identical
bindings are no-op retries even after termination; any changed field conflicts.
The `(scope, run)` pair is exclusive across all Turns and owners. These are
business identity facts only: the store neither opens nor writes native Harness
storage, and a native run does not become a Graph run attachment.

`bind_goose(owner, session, turn, execution)` returns `Result<Turn, SessionError>`.
`GooseExecution` has `scope: String` (64 ASCII hex characters) and `session: String`
(1–1024 UTF-8 bytes, without Unicode control characters). The session ID is opaque:
it is not trimmed, parsed as an integer or rewritten. First binding requires a
running Turn; identical retries preserve timestamps and events even after
termination, and any changed field conflicts. Native and Goose bindings are
mutually exclusive on a Turn, including concurrent binding from independent
connections. Projection rejects inconsistent stored dual bindings. The same
`(scope, session)` can be reused by later Turns in the same platform Session.
A scope belongs to one platform Session and cannot be adopted by another platform
Session or owner, even with a different Goose session ID. The store does not
read or write Goose's native history or convert legacy native associations.

`associate_run(owner, session, turn, run)` returns `Result<Turn, SessionError>`.
It atomically appends a path-safe run ID to both Turn `runs` and Session `run_ids`,
deduplicating each independently and preserving insertion order. Existing
`attach_run` facts can be associated later. Runtime IDs such as `rust-<uuid>` use
the existing identity validation. Both methods check trusted ownership and exact
Session/Turn membership inside an immediate transaction.

New bindings update Session activity with `turn.native_bound` or `turn.goose_bound`; new Turn run links
write `turn.run_associated`, alongside `run.attached` if the Session attachment is
new. Retries append no duplicate events or change timestamps. Associations do
not change Turn execution timestamps or either lifecycle. Terminal run links are
allowed for Host startup reconciliation, including interrupted or archived
Sessions, without appending Turn delivery or reopening terminal Turns. Binding
and association calls never append UI delivery events. Host cross-store
coordination and native-history cleanup remain outside this crate.

## Database boundary

Pass an explicit regular file path whose parent directories already exist.
No directories are created. Symlinks in the database path or its ancestors,
non-regular files, unsafe SQLite sidecars, parent traversal, memory databases and
SQLite URI paths are rejected. SQLite also opens with `SQLITE_OPEN_NOFOLLOW`.

The separate database uses application ID `0x414e5353` and schema version `6`.
Only an empty, unversioned database can be initialized. Existing identity,
version and schema definitions must match exactly. Exact native v1/v2/v3/v4/v5 databases
upgrade transactionally to v6; foreign, altered or future schemas are rejected
before DDL. This does not migrate Python Session/Turn history. Successful stores use
WAL, full synchronous commits, foreign keys and a five-second busy timeout.
The v3 `turn_native` and `turn_runs` tables preserve the existing Turn SQL and
delivery records and cascade with business Turn/Session deletion. Retained Graph
run IDs still prevent Session deletion; native facts alone do not retain it.
The additive v4 `turn_goose` table and lookup index leave native records and
delivery JSON unchanged and cascade with business deletion. Host adapters must
reject incompatible legacy runtime histories rather than reuse them as Goose
sessions; the platform store does not choose a runtime or migrate that history.

The integration tests use only temporary SQLite files and local threads. They
cover DTOs, ownership, lifecycle, cursors, persistence, parallel connections,
path/schema rejection and injected transaction failures. No model or external
service is involved. API/Host assembly and production migration remain separate.
