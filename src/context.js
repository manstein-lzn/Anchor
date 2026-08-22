const PURPOSES = new Set(["work", "resume", "review", "verify", "acceptance"]);

export function activeBeliefs(state) {
  return state.beliefs.filter((belief) => belief.status === "active" || belief.status === "confirmed");
}

export function compileContext(state, { purpose = "work", capabilities = [], policyVersion = "anchor.context.v1" } = {}) {
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
    cognition: activeBeliefs(state),
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
    ...items.map((belief) => `- [${belief.kind}/${belief.status}] ${belief.text}${belief.source ? ` (source: ${belief.source})` : ""}`),
  ]);
  return [
    "You are a short-lived invocation operating on authoritative Anchor state.",
    "Treat this context as evidence-backed task state. Do not invent facts or permissions.",
    "The CURRENT COGNITION section is the accumulated, revised understanding of this task;",
    "it outranks any assumption you might infer from scratch. If your plan conflicts with it,",
    "state the conflict explicitly instead of silently ignoring it.",
    "Return tool calls or a candidate state_delta; durable changes are validated by the host.",
    "<anchor-context>",
    "== CURRENT COGNITION ==",
    ...(beliefLines.length ? beliefLines : ["(no accumulated cognition yet)"]),
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

