# Anchor Rust Host

The platform and standalone entry points use the same `anchor-runtime`
GraphRunner. AgentNode and Pilot execute through pinned Goose ACP; OpNode uses
the authorized Bubblewrap sandbox. The Host owns admission, Graph/Run control,
Library bindings, Artifacts, platform Sessions/Turns, scheduling and channels.

## Build and start

From the repository root:

```sh
cargo +stable build --manifest-path rust/Cargo.toml -p anchor-runner-host --locked
npm --prefix apps/web ci
cp .env.example .env
# Configure the pinned Goose binary and model in .env.
./scripts/dev.sh start
```

The development script uses `.local/rust`, seeds an Op-only `dev` Graph,
and submits no model work at startup. Vite serves development UI at port 5173;
the Host API and compiled UI use port 8077. Rebuild changed backend code before
restarting. See [usage](../../docs/usage.md) for root and model configuration.

For a prepared deployment, configure separate bundle, catalog, Library, state,
workspace and Web roots, then run `anchor-runner-host serve`. Startup validates
the configuration and acquires a state-root writer lease before listening.
The protected JSONL standalone protocol also uses the same execution kernel.
CLI syntax is available through `anchor-runner-host --help`.

## Product contracts

- Graph editing/validation, trigger, pause, stop and continuation share admission.
- Run details freeze the Graph and per-node Plugin identities; later Library
  changes do not replace historical bindings.
- Each invocation retains its workspace, native Goose history, observed tool
  outcomes and immutable Artifact. Continuation checks interrupted effects;
  startup never replays uncompleted work automatically.
- Pilot Sessions use durable Turn/request identities and SSE event cursors.
  Native form answers and precise deletion confirmations stay on the same Turn.
- Graph conversations bind trusted owner/source, Graph and reply node. Per-node
  history and read-only previous workspaces stay isolated by that identity.
- Manual triggers, UTC schedules, Webhook and the documented Responses subset
  use the same Runner. Health/readiness requests do not call a model.
- The Host supervises the declared native WeCom Gateway. Unknown sends are
  persisted without automatic resend; public media integration remains separate.

Detailed contracts and limitations live in the [current architecture](../../docs/architecture.md),
[Web API contract](../../docs/rust-frontend-api-contract.md),
[Graph composition](../../docs/graph-composition-design.md),
[Plugin guide](../../docs/plugins.md) and [WeCom guide](../../docs/wecom-assistant.md).
No existing production catalog, records or credentials are migrated by startup.

## Verification and distribution

```sh
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host --locked
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
  cargo +stable run --manifest-path rust/Cargo.toml -p anchor-devtools --locked -- \
  regression fixture
```

The deterministic fixtures execute the actual Host, Goose, MCP and Sandbox
against local scripted providers. Tests require Linux, Git, Bubblewrap and usable
namespaces. Conditional Goose tests require the explicit pinned binary; missing
configuration is not a passing acceptance result. Evidence checks final results,
node histories, workspace/Artifact contents, identity and recovery.

Use [native development tools](../anchor-devtools/README.md) for regression,
preflight and read-only cutover preparation. The
[distribution builder](../anchor-distribution/README.md) creates source-free
packages; the [candidate regression](../../docs/rust-production-candidate.md)
checks release Host, Web, Goose and packaged official tools together.

Real providers, full business content, public services, target-system deployment
and production migration are distinct acceptance items in the
[development ledger](../../docs/pilot-development-plan.md).
