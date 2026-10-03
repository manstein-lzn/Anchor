# Anchor RSI evidence Plugin

An independent, read-only Rust Streamable HTTP MCP service. It owns business
review evidence, not Graph scheduling, model execution, or project modifications.
`anchor-runner-host` runs the ordinary JSON Graph in `examples/rust-rsi/`.

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

From `rust/`, build `cargo build -p anchor-rsi -p anchor-runner-host`. With a model
configured in the repository `.env`, the optional operator launcher is:

```sh
./.venv/bin/python scripts/rust_rsi_run.py --data /path/to/anchor-data \
  --rust-state /path/to/rust-state --previous /path/to/previous-published-report
```

Python here only launches the Rust binaries and verifies output; it is not part
of either runtime or MCP service. The same binaries can be launched directly
using the environment and host protocol documented in `anchor-runner-host`.
No production scheduler, Graph definition, code or message destination is changed.
The result is a review report and proposed changes, never automatic code edits.

The Graph uses five independent audit branches, a Coordinator join, synthesis,
independent review with a revision edge, then a deterministic publish check.
The publish check verifies required output structure and review approval; it
cannot prove the truth of model claims. Source checks and human assessment remain
necessary before implementing proposals. Long-term improvement is a separate
acceptance criterion from one completed report.

Run projections include `source_field_presence` and `projection_omitted`: a field
omitted from review evidence may still exist in the original record. Presence
never authorizes exposing its private contents.

To review an existing report with operator feedback without repeating the five
audits, use a fresh proof directory and:

```sh
./.venv/bin/python scripts/rust_rsi_run.py \
  --revision-report /path/to/previous-report-files \
  --feedback-file /path/to/acceptance-feedback.md
```

The launcher creates a separate analyze/review/publish Run. The original Run and
immutable report remain unchanged. `acceptance.json` distinguishes mechanical
Graph completion from the separately recorded owner assessment; model approval
is not owner acceptance. Real acceptance results and known missed claims are in
the development ledger A61.
