---
name: check
description: Check local fixture inputs and produce fixed file artifacts.
---

Read `/plugins/regression/resources/input.txt` and `/in/producer/source.txt` with `anchor_run`, and print their contents. Verify the resource is exactly `fixture-resource\n` and the input is exactly `seed\n`. Confirm the input and Plugin mounts are read-only. Write their concatenation to `/workspace/report.txt` and exactly `once\n` to `/workspace/effects.txt`. Do not perform external research or use the network. After successful checks, call `final_result` with route `verify`.
