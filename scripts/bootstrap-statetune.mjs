#!/usr/bin/env node
// Bootstrap an Anchor State for /root/statetune by capturing its accumulated
// cognition (doctrines, findings, negative results, authority boundaries) from
// the hand-maintained truth entry points into a versioned State file.
//
// Usage: node scripts/bootstrap-statetune.mjs [statePath]

import { StateStore } from "../src/state.js";

const ROOT = "/root/statetune";
const statePath = process.argv[2] ?? `${ROOT}/.anchor/state.json`;

const seed = {
  goal:
    "Align StateTune with published baselines: full TGraph paper Table 2 alignment (layout + tile), and TenSet official Table-3 Model #3 through projects/statetune_core.",
  acceptance: [
    "layout:xla:random reaches Tau = 0.6840 +/- 0.0110 under a protocol-aligned local evaluation",
    "four layout collections match TGraph paper Table 2 under the same metric/protocol family",
    "tile:xla reaches M_tile = 0.9694 +/- 0.0021",
    "statetune_core consumer holds the official Table-3 M#3 triple: pw >= 0.89, P@1 >= 0.88, P@5 >= 0.96",
    "every claimed number states its metric basis (fold count, aggregation, seeds, checkpoint choice, TTA/ensemble)",
  ],
  constraints: [
    "Never claim 'TGraph paper alignment' from layout:xla:random alone.",
    "Never claim Kaggle hidden-test parity without a valid Kaggle submission score.",
    "Official TenSet 164D features are oracle-only: never feed them as model input.",
    "Do not train or optimize beyond the currently approved authority (T.5: feature enrichment).",
    "New core work lives under projects/statetune_core/; universal_value_tokenizer is sealed legacy evidence.",
  ],
  beliefs: [
    // ---- workstream:tgraph_alignment (top-level AGENTS.md) ----
    {
      id: "tg:acceptance-ladder",
      kind: "protocol",
      text: "Alignment claims use a three-tier ladder: local method milestone < layout paper alignment < full alignment (layout + tile). Tier must be named in every claim.",
      scope: "workstream:tgraph_alignment", confidence: "high", established_rev: 0,
      source: "AGENTS.md",
    },
    {
      id: "tg:folds-6-9-not-global",
      kind: "doctrine",
      text: "Strong results on folds 6-9 are not evidence of global stability; blind folds outside prior decision paths are required.",
      scope: "workstream:tgraph_alignment", confidence: "high", established_rev: 0,
      source: "handover.md",
    },
    // ---- workstream:stage_h_blind (handover.md, 2026-06-16 — stale, verify before acting) ----
    {
      id: "sh:blind-11-13-shape",
      kind: "finding",
      text: "As of 2026-06-16 blind folds 11-13: fold12 near paper/Kaggle reference range; fold11 shows clear best-final drift; fold13 looked weak. STALE - two months old, re-check summary.json before relying on it.",
      scope: "workstream:stage_h_blind", confidence: "low", established_rev: 0, as_of: "2026-06-16",
      source: "handover.md",
    },
    // ---- workstream:core_tenset (NOW.md / PROJECT_STATUS.md, fresh to 2026-08-11) ----
    {
      id: "ct:canonical-position",
      kind: "finding",
      text: "Canonical line u1_schedule_repr_peak1_improved at phase official_table3_alignment_a_orderscale; HEAD eb59df7 (A2), baseline 98540cc, branch research/unified-compile-cost-consumer-v1.",
      scope: "workstream:core_tenset", confidence: "high", established_rev: 0, as_of: "2026-08-11",
      source: "projects/statetune_core/PROJECT_STATUS.md",
    },
    {
      id: "ct:a-protocol-final",
      kind: "finding",
      text: "A-protocol final (OrderScale + warmup/cosine LR/val-early/test-once): pw 0.8776, P@1 0.8903 (PASS >= 0.88), P@5 0.9312 (0.029 short). First strict order-preserving result; residual gap is pw and P@5.",
      scope: "workstream:core_tenset", confidence: "high", established_rev: 0, as_of: "2026-08-11",
      source: "projects/statetune_core/NOW.md",
      evidence: ["ev:t20b-orderscale"],
    },
    {
      id: "ct:table3-is-model3",
      kind: "doctrine",
      text: "Table 3 is three separate models; the target is Model #3 RANKING row (official SegmentSumMLP + probabilistic LambdaRank; official sacrifices regression R2=-1818). Comparing against the wrong row produces false gaps.",
      scope: "workstream:core_tenset", confidence: "high", established_rev: 0,
      source: "projects/statetune_core/NOW.md",
    },
    {
      id: "ct:metric-basis-correction",
      kind: "negative_result",
      text: "rankhead_v1/v2 'clears Peak@1 0.93' was a sign-inversion artifact: raw log-latency y fed into a higher=better metric. Honest throughput basis: rankhead_v1 = pw 0.8126 / P@1 0.5962 / P@5 0.7028, strictly WORSE than schedule shared head. Check metric basis before trusting any rank-head number.",
      scope: "workstream:core_tenset", confidence: "high", established_rev: 0,
      source: "projects/statetune_core/NOW.md",
    },
    {
      id: "ct:d-ensemble-closed",
      kind: "negative_result",
      text: "Cross-tower fusion / checkpoint blending CLOSED: no blend reaches the M#3 triple; all below the schedule baseline's own numbers (2026-08-10).",
      scope: "workstream:core_tenset", confidence: "high", established_rev: 0, as_of: "2026-08-10",
      source: "projects/statetune_core/NOW.md",
      evidence: ["ev:t20b-d-ensemble"],
    },
    {
      id: "ct:levers-exhausted",
      kind: "negative_result",
      text: "Exhausted levers on clean-27: head/objective variants, B0 numeric channel (within noise), margin hypothesis (pairwise coverage is 98.8% dense - falsified for pw), official recipe alone (pw 0.62-0.65; official 0.89 needs per-store 164D feature granularity, not just the LR recipe).",
      scope: "workstream:core_tenset", confidence: "high", established_rev: 0,
      source: "projects/statetune_core/NOW.md",
    },
    {
      id: "ct:dual-tower-invalidated",
      kind: "invalidated_claim",
      text: "The dual-tower '> Table-3 (0.892)' donor claim was invalidated: comparison scale mismatch.",
      scope: "workstream:core_tenset", confidence: "high", established_rev: 0,
      source: "projects/statetune_core/PROJECT_STATUS.md",
    },
    {
      id: "ct:t5-authority",
      kind: "constraint",
      text: "T.5 is the approved frontier: enrich generic per-store roles toward the missing official families (computation_loop / arithmetic_intensity / GPU / allocation) from the same lowering, shared schema, then retrain once. Do not optimize beyond this authority.",
      scope: "workstream:core_tenset", confidence: "high", established_rev: 0,
      source: "projects/statetune_core/PROJECT_STATUS.md",
    },
    // ---- global ----
    {
      id: "gl:naming-debt",
      kind: "finding",
      text: "Public project name is statetune but most implementation modules still live under execution_state_costmodel; keep legacy stable, add new APIs under statetune.",
      scope: "global", confidence: "high", established_rev: 0,
      source: "docs/KNOWN_ISSUES.md",
    },
  ],
  open_questions: [
    "[stage_h_blind] Has fold13 produced summary.json? Did the watcher stop the outer wrapper? Aggregate folds 11-13 blind results (mean tau + drift).",
    "[core_tenset] Which missing official feature families give the largest pw/P@5 gain first under T.5?",
  ],
  next_action:
    "[core_tenset] Execute T.5 step 1: design per-store role enrichment toward computation_loop / arithmetic_intensity / GPU / allocation families per GENERIC_COMPILE_STATE_DESIGN_V1.md.",
  artifacts: [],
  evidence: [
    { id: "ev:t20b-orderscale", ref: `${ROOT}/projects/statetune_core/evidence/tenset_generic_state_t1/T20B_ORDERSCALE_WARMUP_COSINE_V4_RESULT.md`, type: "experiment_result" },
    { id: "ev:t20b-d-ensemble", ref: `${ROOT}/projects/statetune_core/evidence/tenset_generic_state_t1/T20B_DENS_CROSSTOWER_RESULT.md`, type: "experiment_result" },
    { id: "ev:now-md", path: `${ROOT}/projects/statetune_core/NOW.md`, type: "canonical_status" },
    { id: "ev:project-status", path: `${ROOT}/projects/statetune_core/PROJECT_STATUS.md`, type: "canonical_status" },
    { id: "ev:agents-md", path: `${ROOT}/AGENTS.md`, type: "contract" },
    { id: "ev:handover", path: `${ROOT}/handover.md`, type: "stale_handover", as_of: "2026-06-16" },
  ],
};

const store = new StateStore(statePath);
if (await store.exists()) {
  console.error(`state already exists: ${statePath} (delete it first to re-bootstrap)`);
  process.exit(1);
}
const state = await store.init(seed);
console.log(`bootstrapped ${state.beliefs.length} beliefs, ${state.evidence.length} evidence refs -> ${store.path}`);
