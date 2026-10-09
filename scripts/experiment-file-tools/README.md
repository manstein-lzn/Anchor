# Tool-surface experiment (`anchor_read`/`anchor_edit`)

Paired task-quality experiment for the intervention landed in `306480f`: the
AgentNode tool surface gains `anchor_read`/`anchor_edit`.

One variable changes between arms: **the Host binary**, and therefore whether
those two tools exist. The task, instructions, seed, model alias, provider,
wall-clock budget and checker are identical, and pass/fail comes from an
independent artifact check that never reads the model's own summary.

## Reproduce

```sh
# treatment: any Host built from 306480f or later
python3 run.py --host <path>/anchor-runner-host --arm treatment --out out
# baseline: the same tree at 81a5866 (before the tools), built into its own target dir
python3 run.py --host <baseline-target>/debug/anchor-runner-host --arm baseline --out out
```

`run.py` reads provider settings from `--env-file` (default `/home/mansteinl/Anchor/.env`),
pins `models.worker` to `ANCHOR_MODEL_NAME`, seeds each task through its own op
node, runs the bundle over the Host's stdio protocol, then applies the task's
`check.sh` to the agent node's frozen workspace snapshot. Tokens/cost come from
the node evidence written since `1ef72f3`.

`make_tasks.py` regenerates `tasks/` (five frozen tasks).

## Result (2026-10-09, one repetition per cell, `deepseek-flash`)

| task | baseline pass | treatment pass | baseline req/$/s | treatment req/$/s |
| --- | --- | --- | --- | --- |
| t1-config | yes | yes | 13 / 0.00341 / 25.9 | 8 / 0.00124 / 10.2 |
| t2-script-bug | yes | yes | 10 / 0.00368 / 27.5 | 12 / 0.00252 / 18.7 |
| t3-rename | yes | yes | 5 / 0.00080 / 6.5 | 13 / 0.00178 / 14.6 |
| t4-surgical-edit | yes | yes | 19 / 0.00547 / 43.4 | 14 / 0.00450 / 53.2 |
| t5-version-bump | yes | yes | 7 / 0.00165 / 12.6 | 10 / 0.00177 / 12.7 |
| **total** | **5/5** | **5/5** | **54 / $0.01502 / 115.9s** | **57 / $0.01182 / 109.4s** |

Raw per-run records: `results/baseline.json`, `results/treatment.json`.

**Reading:** at this task size and model, the primary metric (independent
artifact pass rate) shows **no separation** — every task passed in both arms, so
the hypothesis that structured file tools raise task completion quality is *not
supported by this evidence*. The secondary cost/latency signals are suggestive
(treatment −21% in provider cost) but noisy: one repetition per cell, n=5, the
treatment used slightly *more* requests, and per task the winner flips
(t3/t5 favour the baseline). A discriminating experiment needs harder tasks
(adversarial quoting, files large enough that paging matters, many sequential
edits), at least five repetitions per cell with reported uncertainty, and
error-recovery metrics — or the honest conclusion that these tools are a
correctness/consistency improvement (hash-guarded edits, no shell quoting)
rather than a measurable quality lever on small tasks.
