# Agent Surface: MCP and CLI

Anchor is operable by an agent, not only by a human at the web console. This
document defines that surface. It is a product capability, not a wrapper.

## Why

Users want to describe a goal to their agent ("run a literature review on X and
give me the markdown") and have the agent author, publish, run, observe and
intervene — with the same durability, audit trail and approval guarantees the
web console gets. The graph kernel already provides those guarantees; the agent
surface makes them reachable from an agent.

## Principle: the agent surface is not a privileged backdoor

MCP and CLI are clients of the authenticated HTTP API. They never touch the
database, never bypass a lease, never skip an approval gate and never write an
artifact directly. This is what keeps the evidence chain, the operation ledger
and the audit trail meaningful when an agent is driving.

```
                 ┌──────────────┐
   human ───────→│   Web UI     │
                 └──────┬───────┘
                        │ HTTP + bearer token
   agent (MCP) ──┐      │
                 ├──────┴──→  anchor-api  (auth, domain, state, guards)
   agent (CLI) ──┘               ↑
                           workers / services

   anchor.client  ← one typed operation layer (semantics live here)
        ↑                    ↑
   anchor.cli          anchor.mcp   (MCP server, S2)
```

The CLI and the MCP server are thin adapters over `anchor.client.AnchorClient`,
so their behavior cannot drift.

## Operation surface

Grouped by intent, not by REST resource.

| Intent | Operations |
|---|---|
| Discover | `health`, `capabilities`, `ir`, `list_graphs`, `get_draft`, `list_versions` |
| Author | `validate`, `validate_capabilities`, `save_draft`, `publish`, `install`, `export_bundle`, `import_bundle` |
| Execute | `register_trigger`, `set_trigger_enabled`, `start_run`, `pause_run`, `resume_run`, `stop_run` |
| Observe | `run_digest`, `run_events`, `iter_events`, `run_nodes`, `run_decisions`, `run_verifications`, `run_operations`, `run_diagnostics`, `run_progress`, `list_waits`, `read_artifact`, `wait_for_run` |
| Reconcile | `reconcile_operation` |
| Storage | `storage_report`, `get_budget`, `set_budget`, `retention_preview`, `retention_sweep`, `retention_audit` |
| Human-in-the-loop | `approve_wait`, `reject_wait`, `resume_wait` |

`install(definition)` is the agent-friendly composite: validate structurally,
validate capabilities, save the draft at the current revision, and optionally
publish — in one call.

## Safety policy: which operations an agent may call

The approval gate exists so a human, not the author, signs off. If the agent
could call `approve_wait`, the gate would be theatre. Therefore:

| Class | Operations | Agent (MCP) | Human (CLI/web) |
|---|---|---|---|
| Read-only | discover, observe, storage report | ✅ | ✅ |
| Authoring | validate, save draft, publish | ✅ | ✅ |
| Execution | start, pause, resume, stop, archive | ✅ | ✅ |
| Reconciliation | `reconcile_operation` (requires external evidence) | ✅ | ✅ |
| **Human decision** | `approve_wait`, `reject_wait`, `resume_wait` | **❌ by default** | ✅ |
| **Operator-only** | `recover_lease`, `retention_sweep`, `set_budget` | **❌ by default** | ✅ |

The MCP server refuses human-only operations with a structured error unless
explicitly configured otherwise (`agent_can_approve`, default `false`). The CLI
allows them because a human runs it.

## Observation model for long runs

A research run can take minutes to hours. An agent must observe without burning
its context window.

- `run_digest(run_id)` returns a **compact** view: status, phase, node-status
  counts, waiting nodes, failed nodes, last event sequence. No raw history.
- `run_events(run_id, after=sequence)` and `iter_events` page the append-only
  log incrementally; the agent resumes from the last sequence it saw.
- `wait_for_run(run_id, timeout, interval)` is a bounded long-poll. It returns a
  digest with `timed_out: true` when the deadline passes, so the agent loops.
- Drill-down is explicit: decisions, verifications, operations, diagnostics,
  progress, artifacts. Each is bounded and separate.

## Authoring: the agent must not guess the IR

`GET /api/graphs/ir` (and `anchor ir`) returns the machine-readable authoring
reference for the running instance:

- all 12 node types, with the required capability reference per type and notes
- common node fields, edge fields, metadata conventions
- the condition language: JMESPath over `{output, inputs}`, must return a boolean
- input mapping syntax: `{"<target_key>": "outputs.<node_id>.<path>"}`
- the six-step authoring flow
- a template that validates as-is

This is the single highest-leverage piece: it turns "the agent must reverse
engineer the DSL" into "the agent reads the contract".

## MCP server

`anchor-mcp` speaks newline-delimited JSON-RPC 2.0 over stdio and exposes the
whole operation surface as MCP tools. It has no third-party dependency and no
privileged path: every tool calls `AnchorClient`, which calls the authenticated
HTTP API.

```jsonc
// e.g. Claude Code / any MCP client config
{
  "mcpServers": {
    "anchor": {
      "command": "/path/to/Anchor/.venv/bin/anchor-mcp",
      "env": {
        "ANCHOR_API_URL": "http://127.0.0.1:8090",
        "ANCHOR_API_TOKEN": "<token>"          // or ANCHOR_TOKEN_FILE
      }
    }
  }
}
```

- **Tools**: one per client operation, grouped by intent, each with a closed
  JSON Schema (`additionalProperties: false`) so an agent cannot invent fields.
- **Refusal**: `approve_wait`, `reject_wait`, `resume_wait` (human decision) and
  `retention_sweep`, `set_budget` (operator-only) return
  `isError: true` with `error: "human_only_operation"` unless the server starts
  with `ANCHOR_MCP_AGENT_CAN_APPROVE=1`.
- **Errors**: every failure becomes an `isError: true` result carrying the
  stable code, HTTP status, path and `retryable`, so an agent branches on data
  rather than prose.
- **Context discipline**: `run_digest` and `wait_for_run` are the intended entry
  points; drill-down tools are separate and bounded.

## Error model

Every API failure becomes `AnchorApiError` with a stable `code`, the HTTP
`status`, the `path`, and the server `detail` (for example validation issues).
`retryable` is true for 5xx and 429. The CLI prints the same structure to stderr
and exits 1 for API errors, 2 for client/usage errors, 0 for success — so an
agent can branch on exit code without parsing prose.

## CLI

```bash
anchor ir                                   # the IR contract
anchor capabilities                         # available refs
anchor graph validate --file graph.json     # structural + capability checks
anchor graph install  --file graph.json     # validate, save, publish
anchor trigger add --version <id>           # manual trigger
anchor run start --trigger <id> --objective "..." --idempotency-key k1
anchor run watch <run_id> --timeout 300     # compact digest, bounded
anchor run events <run_id> --after 42
anchor waits                                # what needs a human
anchor wait approve <node_run_id> --reason "..."   # human action
anchor operation reconcile <op_id> --status succeeded --reconciliation-ref provider://...
anchor storage / anchor budget / anchor retention preview|sweep|audit
```

All output is JSON by default.

## Delivery status

| Slice | Content | Status |
|---|---|---|
| S1 | `anchor.client`, `anchor.cli`, `GET /api/graphs/ir`, tests | **done** |
| S2 | MCP server over stdio, human-only refusal, tool schemas | **done** |
| S3 | observation ergonomics (resources, richer digests), authoring help | partial (`ir`, `run_digest`, `wait_for_run`) |
| S4 | docs, PRODUCT_VISION alignment, MCP client e2e | **done** (`tests/test_mcp.py` drives the live API) |

## Non-goals (for now)

- Multi-user identity, per-tenant isolation, remote MCP transport.
- Letting an agent register new capabilities: `agent_ref`/`tool_ref` remain
  file-configured and read-only over the API.
- Letting an agent approve its own work.
