# io-harness NodeExecutionPort spike

This compatibility package exposes the next integration boundary after A70.
The canonical implementation maps Anchor's graph-level
`NodeExecutionRequest` to the A70 io-harness NodeRequest adapter while keeping
workspace and ToolPort resolution in a host-owned resolver.

The spike proves only the safe subset:

- Agent nodes with no exact cumulative `max_provider_requests` budget;
- durable Anchor started/completed facts alongside a per-invocation io-harness
  SQLite store;
- completed nodes publish an Anchor `NodeCompletion` only after io-harness
  reports `Finished` and the strict A70 summary/route contract passes;
- io-harness's non-`Send` Store future stays inside a dedicated blocking thread
  and current-thread Tokio runtime, so the graph `NodeExecutionPort` contract
  remains `Future + Send`;
- provider failure is fail-closed: the started fact remains uncertain and the
  per-invocation io-harness run id is retained for a future explicit recovery
  path; the current GraphRunner has no `Uncertain` recovery decision API.

The budget and cancellation mappings are inherited from the A70 executor and
remain explicit when io-harness emits them: `StepCapReached`/time/cost budgets
become `BudgetExhausted`, cancellation becomes `Cancelled`, and neither
publishes a completion fact. This slice does not translate Anchor
`max_provider_requests` into an io step budget, and A70 has not yet injected
Graph wall/cost budgets into the `TaskContract`.
Known invalid final output and tool-registration errors write a durable failed
fact. Recovery pauses and provider/store errors are not converted to ordinary
node failure. A repeated Graph execution remains blocked by Anchor
`CompletionFact::Uncertain`; retaining an io-harness run id alone does not make
that Graph invocation resumable.

It does not modify production `HostNodes`, and it does not claim
process-crash-window, Sandbox/MCP, or exact provider-budget support. Those paths
remain fail-closed until the runtime contract has a durable way to represent an
io-harness run that is paused for recovery.

This wrapper has no local test target. Run the canonical implementation and
provider-free tests with:

```text
CARGO_TARGET_DIR=/tmp/anchor-io-harness-runtime-target \
  cargo test --manifest-path rust/anchor-io-harness-runtime/Cargo.toml -- --nocapture
```
