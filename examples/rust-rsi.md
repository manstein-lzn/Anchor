# Rust-native RSI Graph

This example is a normal format-1 Graph bundle with one read-only evidence Plugin.
Five paired fanout/join branches examine runs, code, Graphs, Plugins and dependency
signals. Their findings are synthesized, checked by an independent reviewer, and
published only after the review passes. The reviewer can return to analysis for
correction. There is no hard-coded research-round limit.

The five domain roles are Graph design, not a hard-coded list of deployed Graphs
or dependency projects. The evidence service discovers those from operator roots.
Only direct dependencies and public release metadata are covered by the initial
community research tool; broad community discussion and feature applicability
remain limitations, not implied completed research.

The Plugin manifest pins the identity `anchor.rsi`. The launcher writes the
disposable evidence service URL into that Plugin's canonical `mcpServers`
declaration before admission; credentials and host paths are never embedded in
this bundle. MCP network permission is separate from shell-tool network
permission: the nodes authorize the configured HTTP MCP, while `anchor_run`
remains network-disabled.

Build/run instructions are in `rust/anchor-rsi/README.md`. The optional repository
launcher writes a disposable evidence/run directory and does not alter installed
Graphs or production schedules. Historical reports are optional operator input.
Report generation does not automatically implement proposals or prove long-term
recursive improvement.

The directory `examples/rust-rsi/` is the distributable format-1 bundle.
