export function compileContext(recovery) {
  if (!recovery || typeof recovery !== "object") throw new TypeError("Anchor RecoveryView is required");
  if (!recovery.checkpoint) throw new TypeError("Anchor Checkpoint is required");
  const contract = recovery.contract?.content ?? recovery.contract ?? {};
  const cognition = projectCognition(recovery.checkpoint.cognition);
  return {
    schema: "anchor.context.v4",
    task_id: recovery.task_id,
    task: { title: recovery.task?.title, lifecycle_status: recovery.task?.lifecycle_status },
    contract: pick(contract, ["schema", "status", "goal", "acceptance_criteria", "constraints", "non_goals"]),
    cognition,
    checkpoint: {
      schema: recovery.checkpoint.schema,
      task_id: recovery.checkpoint.task_id,
      checkpoint_version: recovery.checkpoint.checkpoint_version,
      frontier: recovery.checkpoint.frontier,
      provenance: recovery.checkpoint.provenance,
      cognition,
    },
  };
}

export function renderContext(envelope) {
  return [
    "You are continuing one long-running Anchor Task.",
    "The Checkpoint is the authoritative cognition through its source frontier.",
    "The Pi messages after it are the newer working episode and may contain a later user directive.",
    "Keep the Task goal, current directive, and accepted next action distinct.",
    "Do not retry failed paths without new evidence or claim unverified work as complete.",
    "<anchor-context>",
    "== TASK CONTRACT ==",
    JSON.stringify({ ...envelope.task, contract: envelope.contract }, null, 2),
    "== ACTIVE COGNITION ==",
    JSON.stringify(envelope.cognition, null, 2),
    "== CHECKPOINT FRONTIER ==",
    JSON.stringify({ checkpoint_version: envelope.checkpoint.checkpoint_version, frontier: envelope.checkpoint.frontier, provenance: envelope.checkpoint.provenance }, null, 2),
    "</anchor-context>",
  ].join("\n");
}

export function projectMessages(recovery, messages = []) {
  const envelope = compileContext(recovery);
  return [{ role: "user", content: [{ type: "text", text: renderContext(envelope) }], timestamp: Date.now() }, ...messages];
}

function projectCognition(cognition = {}) {
  if (cognition.schema === "anchor.cognition.v3") return cognition;
  // Legacy v2 is accepted only as a read view for pre-upgrade fixtures.
  return {
    schema: "anchor.cognition.v3-legacy-view",
    situation: {
      current_understanding: cognition.current_understanding ?? "",
      confirmed_facts: strings(cognition.confirmed_facts),
      active_hypotheses: strings(cognition.active_hypotheses),
      unresolved_conflicts: strings(cognition.unresolved_conflicts),
      blockers: strings(cognition.blockers),
    },
    experience: { decisions: strings(cognition.decisions), failed_paths: strings(cognition.failed_paths) },
    intent: {
      current_directive: cognition.current_directive ?? "",
      accepted_next_action: cognition.accepted_next_action ?? "",
      next_plan: strings(cognition.next_plan),
      open_questions: strings(cognition.open_questions),
    },
    knowledge_index: strings(cognition.evidence_refs).map((locator, index) => ({ id: `legacy-ref-${index}`, cue: "Legacy evidence reference", locator, source: locator })),
  };
}

function strings(value) { return Array.isArray(value) ? value : []; }
function pick(value, fields) { return Object.fromEntries(fields.filter((field) => value[field] !== undefined).map((field) => [field, value[field]])); }
