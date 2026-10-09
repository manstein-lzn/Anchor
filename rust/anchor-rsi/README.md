# Anchor RSI evidence Plugin

An independent, read-only Rust Streamable HTTP MCP service. It owns business
review evidence, not Graph scheduling, model execution, or project modifications.
The ordinary JSON Graph in `examples/rust-rsi/` is retained as a business
acceptance asset; its full Goose execution has not been revalidated.

At startup it freezes a view of the operator-authorized source and deployment:

- source code, documentation, manifests and dependency declarations;
- dynamically discovered `workspaces/*/graph.json`;
- deployed public Plugin/Skill resources;
- all discovered Run metadata, with a separate last-seven-days flag based on
  declared timestamps (missing timestamps remain unknown);
- optionally previous review reports and one Rust state root.

Prompts, conversations, checkpoints and model traces are not Run evidence.
Credential/cache/runtime paths and symlinks are excluded. Text/JSON redaction is
heuristic and explicitly reported; evidence is not executable source. Files over
8 MiB are skipped with a coverage issue. A read page contains at most 200 lines
and 64 KiB; larger individual lines return an explicit incomplete-read error.

## Configuration

Required environment:

```text
ANCHOR_RSI_SOURCE_ROOT=/absolute/path/to/source
ANCHOR_RSI_DATA_ROOT=/absolute/path/to/anchor-data
ANCHOR_RSI_EVIDENCE_ROOT=/absolute/path/to/new-empty-evidence-directory
```

Optional: `ANCHOR_RSI_RUST_STATE_ROOT`, `ANCHOR_RSI_PREVIOUS_ROOT`. Missing optional
roots are limitations, not empty-success claims. Use a report-only directory as
the previous root. Source and deployment data remain read-only; output is stored
only in the separate evidence directory. Local same-user processes are trusted.
The server binds an ephemeral loopback port and prints its `/mcp` endpoint as the
first stdout line. It is not intended as a remotely exposed unprotected service.

## Model tools

- `rsi_index({domain,offset?,limit?})`: domains `code`, `graphs`, `plugins`, `runs`,
  `dependencies`, `previous`; default 20 entries and a continuation offset.
- `rsi_read({path,offset?,limit?})`: an indexed virtual evidence path; offsets are
  zero based, returned line numbers are one based frozen-projection locators.
  Absolute paths, traversal, unindexed files and mutable host files are refused.
- `rsi_ecosystem({offset?,limit?})`: public metadata for a page of dynamically
  discovered direct dependencies. HTTPS requests are limited to crates.io,
  PyPI, npm and GitHub; no redirects, proxy environment or deployment credentials.
  Timeouts, missing repositories, response size limits and HTTP errors remain
  evidence. Version/release metadata is a signal, not a feature or upgrade-benefit
  guarantee. The returned `evidence_path` can be read and cited later.

`index.json`, frozen files, ecosystem page indexes and `tool-calls.jsonl` allow an
operator to check what was collected, what the model actually requested, and
which requests failed. Inventory coverage is distinct from model review coverage.
All model-facing evidence is untrusted task data, not an instruction source.

## Run the Graph

Build with `cargo build -p anchor-rsi --locked` from `rust/`. The same binary
provides the read-only evidence MCP service and deterministic business commands:
`collect`, `research`, `rsi-review`, `rsi-gate`, `rsi-publish`, `native-gate`,
`native-publish`, `weekly-collect`, `weekly-gate` and `weekly-publish`.
Use `anchor-rsi --help` for arguments. Gates call the existing Host
`anchor-route`; publishing rechecks review, commit and evidence bindings.

The Graph uses the existing Host/Runner with operator-authorized local inputs
and a canonical Plugin MCP endpoint. Install and schedule it through the normal
Graph APIs, following [RSI setup](../../docs/rsi.md) or
[weekly report setup](../../docs/weekly-work-report.md). There is no separate
business Runner. For routine execution regression, use the
[native deterministic Goose fixtures](../anchor-devtools/README.md).
Real model/business acceptance and report quality remain separate.

The Graph uses five independent audit branches, a Coordinator join, synthesis,
independent review with a revision edge, then a deterministic publish check.
The publish check verifies required output structure and review approval; it
cannot prove the truth of model claims. Source checks and human assessment remain
necessary before implementing proposals. Long-term improvement is a separate
acceptance criterion from one completed report.

Run projections carry the record's own graph identity (`graph`, `graph_entry`,
`graph_objective`, `graph_nodes`) instead of a constant label, plus `format`,
`started`/`updated` and `duration_ms` when the producer recorded timestamps.
Real values are projected for `error`, `cursor`, `recovery` (pending and
submission counts), the parallel activation (`parallel`, `fanout`, `join` with
`branches[].status`) and a per-node `results` summary. `source_field_presence`
is kept only where absence is real (`started`, `updated`, `reason`); it never
stands in for a value, and presence never authorizes exposing private contents.
`projection_omitted` lists only the bodies left out entirely (`snapshot`,
`input`, `decided`, `graph_calls`, `plugin_bindings`). Deployment graph
definitions are collected from `catalog/*/graph.json`; the schedule snapshot is
resolved from the granted Rust state root (`schedules.json` or
`state/schedules.json`).

Historical report and correction acceptance in A61 remains in the development
ledger. The former launch and revision commands can be inspected at commit
`45d4bbfa189aafb5a41bd8a8b05295e98749ca6b`; they are not current instructions.
Mechanical Graph completion, model review and owner report assessment remain
different acceptance states.
