# P7 Historical Fixture

This directory preserves the report-only signal investigation experiment and
its original results. It is not a second supported optimization workflow.
The built-in Dreaming Graph and its dedicated system have been retired, not
replaced by this fixture. The separate `examples/graphs/rsi-signals.json` remains
removed; neither path is a current product entry.

`graph/build.py` owns the historical fixture. The launchers and Op tests consume
it directly; an explicit build writes only `graph/rsi-signals.fixture.json`,
never a product example. The saved trial Graph and reports retain their
original bytes and conclusions. Historical launchers do not establish current
release or provider acceptance.

Historical fixture commands from the repository root (not run by this preparation):

```sh
python3 experiments/harness-hub/p7/graph/build.py
python3 experiments/harness-hub/p7/test_graph.py
```

The build also writes `graph/grants.json`; data-dependent Op tests require the
existing local Hub binary and `.local/harness-hub-p6/exp1/B`, otherwise they skip.
Raw trial logs, native histories and machine-local inputs are not checked in;
see the [local evidence rules](../../../docs/development.md#文档归属). Generated
fixture JSON is not a shipped example or a new acceptance result.
