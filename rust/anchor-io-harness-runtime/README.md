# anchor-io-harness-runtime

This workspace crate is the canonical Rust-native Anchor boundary for
`io-harness = 0.86.0` and `rig-core = 0.43.0`.

The layering is deliberate:

- `adapter`: converts io-harness provider requests/responses to Rig;
- `node`: exposes Anchor `ToolPort` and owns the io-harness SQLite backend;
- `node_exec`: freezes an Anchor `NodeRequest` into a TaskContract and only
  accepts a strict completed `{summary, route?}` result;
- `node_port`: resolves host-owned workspaces/tools asynchronously and maps
  execution facts to Graph `NodeExecutionPort`.

io-harness owns the only Agent loop. Rig is only the provider transport. Anchor
continues to own Graph/Run/Artifact/Plugin/Sandbox facts. Provider or recovery
states whose external outcome is unknown remain fail-closed with a resumable
io-harness run id.

The former `experiments/io-harness-{adapter,node,node-exec,node-port}` packages
are compatibility wrappers around these modules. They remain useful as stable
historical entry paths and do not contain a second implementation.
