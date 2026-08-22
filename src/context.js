import { createHash } from "node:crypto";
import { readFile, stat } from "node:fs/promises";

const PURPOSES = new Set(["work", "resume", "review", "verify", "acceptance"]);

export const DEFAULT_FRESHNESS = { staleDays: 30, staleRevisions: 200 };

// Deterministic staleness: a belief is stale when its own timestamp or its
// age in revisions exceeds the thresholds. Stale beliefs are still projected,
// but marked so the model verifies before trusting them.
export function beliefStaleness(belief, revision, freshness = DEFAULT_FRESHNESS) {
  const { staleDays, staleRevisions } = { ...DEFAULT_FRESHNESS, ...freshness };
  if (typeof belief.as_of === "string") {
    const timestamp = Date.parse(belief.as_of);
    if (Number.isFinite(timestamp) && Date.now() - timestamp > staleDays * 86_400_000) {
      return `as_of ${belief.as_of} is older than ${staleDays} days`;
    }
  }
  if (Number.isInteger(belief.established_rev) && Number.isInteger(revision) && revision - belief.established_rev > staleRevisions) {
    return `aged ${revision - belief.established_rev} revisions (established at r${belief.established_rev}, now r${revision})`;
  }
  return null;
}

// Bounded evidence-drift check: only entries that carry both a path and a
// sha256 are verified, at most `cap` per compile. Results are cached by
// path+size+mtime so unchanged files are never re-hashed on the hot path.
export async function verifyEvidence(state, { cap = 8, cache = new Map() } = {}) {
  const candidates = (state.evidence ?? []).filter((entry) => typeof entry.path === "string" && typeof entry.sha256 === "string").slice(0, cap);
  const drifted = [];
  let verified = 0;
  for (const entry of candidates) {
    let size = null;
    let mtimeMs = null;
    try {
      const stats = await stat(entry.path);
      size = stats.size;
      mtimeMs = stats.mtimeMs;
    } catch {
      // missing file: key still deterministic
    }
    const cacheKey = `${entry.path}|${size ?? "missing"}|${mtimeMs ?? "-"}`;
    let actual = cache.get(cacheKey);
    if (actual === undefined) {
      try {
        const buffer = await readFile(entry.path);
        actual = createHash("sha256").update(buffer).digest("hex");
      } catch {
        actual = null;
      }
      cache.set(cacheKey, actual);
    }
    verified += 1;
    if (actual !== entry.sha256.toLowerCase()) {
      drifted.push({ path: entry.path, expected: entry.sha256, actual: actual ?? "unreadable/missing" });
    }
  }
  return { verified, drifted };
}

export function activeBeliefs(state) {
  return state.beliefs.filter((belief) => belief.status === "active" || belief.status === "confirmed");
}

export function compileContext(state, { purpose = "work", capabilities = [], policyVersion = "anchor.context.v1", freshness = DEFAULT_FRESHNESS } = {}) {
  if (!PURPOSES.has(purpose)) throw new TypeError(`unknown context purpose: ${purpose}`);
  const envelope = {
    schema: "anchor.context.v1",
    purpose,
    state_revision: state.revision,
    policy_version: policyVersion,
    goal: state.task.goal,
    acceptance: state.task.acceptance,
    constraints: state.task.constraints,
    phase: state.phase,
    completed_work: state.completed,
    failures_and_blockers: state.failures,
    decisions: state.decisions,
    open_questions: state.open_questions,
    next_action: state.next_action,
    cognition: activeBeliefs(state).map((belief) => {
      const stale = beliefStaleness(belief, state.revision, freshness);
      return stale ? { ...belief, stale: true, stale_reason: stale } : belief;
    }),
    artifacts: state.artifacts,
    evidence: state.evidence,
    allowed_actions: [...capabilities],
    provenance: { state_schema: state.schema, state_revision: state.revision },
  };
  if (purpose === "review" || purpose === "verify" || purpose === "acceptance") {
    envelope.review_focus = {
      acceptance: state.task.acceptance,
      evidence: state.evidence,
      artifacts: state.artifacts,
      open_questions: state.open_questions,
    };
  }
  envelope.belief_stats = {
    total: state.beliefs.length,
    active: envelope.cognition.length,
    stale: envelope.cognition.filter((belief) => belief.stale).length,
    superseded: state.beliefs.filter((belief) => belief.status === "superseded").length,
    refuted: state.beliefs.filter((belief) => belief.status === "refuted").length,
  };
  return envelope;
}

export function renderContext(envelope) {
  const beliefs = Array.isArray(envelope.cognition) ? envelope.cognition : [];
  const byScope = new Map();
  for (const belief of beliefs) {
    if (!byScope.has(belief.scope)) byScope.set(belief.scope, []);
    byScope.get(belief.scope).push(belief);
  }
  const beliefLines = [...byScope.entries()].flatMap(([scope, items]) => [
    `# ${scope}`,
    ...items.map((belief) => `- [${belief.kind}/${belief.status}${belief.stale ? " \u26a0stale" : ""}] ${belief.text}${belief.stale ? ` (stale: ${belief.stale_reason})` : ""}${belief.source ? ` (source: ${belief.source})` : ""}`),
  ]);
  const drift = envelope.evidence_check?.drifted ?? [];
  const driftLines = drift.length
    ? [
        "== EVIDENCE DRIFT WARNINGS ==",
        ...drift.map((item) => `- ${item.path}: content no longer matches recorded sha256 (${String(item.expected).slice(0, 12)}...) - verify before trusting dependent beliefs`),
      ]
    : [];
  return [
    "You are a short-lived invocation operating on authoritative Anchor state.",
    "Treat this context as evidence-backed task state. Do not invent facts or permissions.",
    "The CURRENT COGNITION section is the accumulated, revised understanding of this task;",
    "it outranks any assumption you might infer from scratch. If your plan conflicts with it,",
    "state the conflict explicitly instead of silently ignoring it.",
    "Every invocation must end by appending a fenced cognition declaration:",
    '```anchor-state-delta',
    '{ "learned": "one sentence of what this turn established", "blocked": "omit if nothing", "next_action": "...", "belief_ops": [] }',
    '```',
    "Keep it to 1-3 sentences of interpretation only - what you concluded, decided, or found blocked, and the next action.",
    "Do NOT narrate your work: file changes, commands, and outputs are captured automatically by the harness.",
    "belief_ops are for durable cognition changes, e.g.",
    '{ "op": "add", "belief": { "id": "scope:slug", "text": "...", "kind": "finding|negative_result|doctrine|constraint|protocol|invalidated_claim", "scope": "...", "confidence": "high|medium|low" } },',
    '{ "op": "supersede", "id": "old-belief-id", "by": "new-belief-id" }.',
    "Declarations are validated by the host; invalid or stale ones are rejected.",
    "Return tool calls or a candidate state_delta; durable changes are validated by the host.",
    "<anchor-context>",
    "== CURRENT COGNITION ==",
    ...(beliefLines.length ? beliefLines : ["(no accumulated cognition yet)"]),
    ...driftLines,
    "== STATE ENVELOPE ==",
    JSON.stringify(envelope, null, 2),
    "</anchor-context>",
  ].join("\n");
}

export function projectMessages(envelope, currentTurn = []) {
  return [
    { role: "user", content: [{ type: "text", text: renderContext(envelope) }], timestamp: Date.now() },
    ...currentTurn,
  ];
}

