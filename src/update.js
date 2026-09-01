import { createHash } from "node:crypto";
import { convertToLlm } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { Check, Errors } from "typebox/value";
import { reduceUpdateProposal } from "./reducer.js";

const nonEmptyString = () => Type.String({ minLength: 1 });
const stringArraySchema = (options = {}) => Type.Array(nonEmptyString(), options);
const schemaName = (value) => Type.String({ enum: [value] });
const closedObject = (properties) => Type.Object(properties, { additionalProperties: false });

const COGNITION_ITEM_SCHEMA = closedObject({
  id: nonEmptyString(),
  statement: nonEmptyString(),
  sources: stringArraySchema({ minItems: 1 }),
  relevance: nonEmptyString(),
});

const KNOWLEDGE_REFERENCE_SCHEMA = closedObject({
  id: nonEmptyString(),
  cue: nonEmptyString(),
  locator: nonEmptyString(),
  content_hash: Type.Union([
    Type.Literal(""),
    Type.String({ pattern: "^sha256:[0-9a-f]{64}$" }),
  ]),
  source: nonEmptyString(),
});

const COGNITION_SCHEMA = closedObject({
  schema: schemaName("anchor.cognition.v3"),
  situation: closedObject({
    current_understanding: nonEmptyString(),
    confirmed_facts: Type.Array(COGNITION_ITEM_SCHEMA),
    active_hypotheses: Type.Array(COGNITION_ITEM_SCHEMA),
    unresolved_conflicts: Type.Array(COGNITION_ITEM_SCHEMA),
    blockers: Type.Array(COGNITION_ITEM_SCHEMA),
  }),
  experience: closedObject({
    decisions: Type.Array(COGNITION_ITEM_SCHEMA),
    failed_paths: Type.Array(COGNITION_ITEM_SCHEMA),
  }),
  intent: closedObject({
    current_directive: nonEmptyString(),
    accepted_next_action: nonEmptyString(),
    next_plan: stringArraySchema({ minItems: 1 }),
    open_questions: Type.Array(COGNITION_ITEM_SCHEMA),
  }),
  knowledge_index: Type.Array(KNOWLEDGE_REFERENCE_SCHEMA),
});

const TRANSITION_SCHEMA = closedObject({
  schema: schemaName("anchor.transition.v1"),
  dispositions: Type.Array(closedObject({
    item_id: nonEmptyString(),
    disposition: Type.String({ enum: ["carry", "revise", "resolve", "supersede", "demote", "archive"] }),
    reason: nonEmptyString(),
    sources: stringArraySchema({ minItems: 1 }),
    replacement_id: Type.String({ description: "Replacement item id for revise or supersede; otherwise an empty string." }),
    reference: Type.String({ description: "Exact immutable Checkpoint recovery reference for demote; otherwise an empty string." }),
  })),
});

const UPDATE_SUBMISSION_SCHEMA = closedObject({
  ...COGNITION_SCHEMA.properties,
  transition_certificate: TRANSITION_SCHEMA,
});

const CONTRACT_SCHEMA = closedObject({
  schema: schemaName("anchor.contract.v1"),
  status: schemaName("provisional"),
  goal: nonEmptyString(),
  rationale: stringArraySchema(),
  acceptance_criteria: stringArraySchema(),
  constraints: stringArraySchema(),
  non_goals: stringArraySchema(),
  risks: stringArraySchema(),
  verification_commands: stringArraySchema(),
  allowed_paths: stringArraySchema(),
  execution_plan: nonEmptyString(),
});

const BOOTSTRAP_SUBMISSION_SCHEMA = closedObject({
  schema: schemaName("anchor.bootstrap.v1"),
  title: nonEmptyString(),
  contract: CONTRACT_SCHEMA,
  cognition: COGNITION_SCHEMA,
});

const PROPOSAL_ITEM_SCHEMA = closedObject({
  section: Type.String({ enum: ["situation.confirmed_facts", "situation.active_hypotheses", "situation.unresolved_conflicts", "situation.blockers", "experience.decisions", "experience.failed_paths", "intent.open_questions"] }),
  statement: nonEmptyString(),
  sources: stringArraySchema({ minItems: 1 }),
  relevance: nonEmptyString(),
});
const PROPOSAL_DECISION_SCHEMA = closedObject({
  item_id: nonEmptyString(),
  disposition: Type.String({ enum: ["carry", "revise", "resolve", "supersede", "demote", "archive"] }),
  reason: Type.Optional(nonEmptyString()),
  sources: Type.Optional(stringArraySchema()),
  replacement: Type.Optional(PROPOSAL_ITEM_SCHEMA),
  reference: Type.Optional(nonEmptyString()),
});
const UPDATE_PROPOSAL_SCHEMA = closedObject({
  schema: schemaName("anchor.update-proposal.v2"),
  situation: closedObject({ current_understanding: nonEmptyString() }),
  intent: closedObject({ current_directive: nonEmptyString(), accepted_next_action: nonEmptyString(), next_plan: stringArraySchema() }),
  item_decisions: Type.Array(PROPOSAL_DECISION_SCHEMA),
  new_items: Type.Array(PROPOSAL_ITEM_SCHEMA),
  knowledge_index: Type.Array(closedObject({ cue: nonEmptyString(), locator: nonEmptyString(), source: nonEmptyString() })),
});
const BOOTSTRAP_PROPOSAL_SCHEMA = closedObject({
  schema: schemaName("anchor.bootstrap-proposal.v2"),
  title: nonEmptyString(),
  goal: nonEmptyString(),
  uncertainties: stringArraySchema(),
  intent: closedObject({ current_directive: nonEmptyString(), accepted_next_action: nonEmptyString(), next_plan: stringArraySchema(), open_questions: stringArraySchema() }),
  new_items: Type.Array(PROPOSAL_ITEM_SCHEMA),
});

export const UPDATE_SUBMISSION_TOOL = Object.freeze({
  name: "anchor_submit_update",
  label: "Submit Anchor Update",
  description: "Submit the complete Anchor cognition candidate and Transition Certificate. This function records no side effects and is never executed.",
  parameters: UPDATE_SUBMISSION_SCHEMA,
  constrainedSampling: { type: "json_schema", strict: "prefer" },
});

export const BOOTSTRAP_SUBMISSION_TOOL = Object.freeze({
  name: "anchor_submit_bootstrap",
  label: "Submit Anchor Bootstrap",
  description: "Submit the provisional Anchor Contract and initial cognition. This function records no side effects and is never executed.",
  parameters: BOOTSTRAP_SUBMISSION_SCHEMA,
  constrainedSampling: { type: "json_schema", strict: "prefer" },
});

export const UPDATE_PROPOSAL_TOOL = Object.freeze({
  name: "anchor_submit_update",
  label: "Submit Anchor Update Proposal",
  description: "Submit semantic Anchor changes. Anchor deterministically materializes the complete Checkpoint candidate.",
  parameters: UPDATE_PROPOSAL_SCHEMA,
  constrainedSampling: { type: "json_schema", strict: "required" },
});

export const ANCHOR_UPDATE_PROTOCOL = "anchor.update-proposal.v2";

export const BOOTSTRAP_PROPOSAL_TOOL = Object.freeze({
  name: "anchor_submit_bootstrap",
  label: "Submit Anchor Bootstrap Proposal",
  description: "Submit a provisional semantic bootstrap proposal. Anchor owns Contract and Checkpoint materialization.",
  parameters: BOOTSTRAP_PROPOSAL_SCHEMA,
  constrainedSampling: { type: "json_schema", strict: "required" },
});

export const UPDATE_PROPOSAL_SYSTEM = `You are the Anchor Update Agent.

Submit one anchor.update-proposal.v2 semantic proposal for exactly the supplied
Checkpoint and Pi Episode. Address every previous active item exactly once in
item_decisions. Use carry only for unchanged items; changed items require revise
or supersede with a replacement. Resolve, archive, and demote require explicit
reason and sources; demote requires an exact immutable Checkpoint reference.
Anchor materializes the complete Checkpoint and Transition Certificate. Do not
submit durable metadata, hashes, Task identity, versions, or frontier fields.
Submit exactly once through anchor_submit_update and do not return free-text JSON.`;

export const BOOTSTRAP_PROPOSAL_SYSTEM = `You are the Anchor Bootstrap Agent.

Submit one anchor.bootstrap-proposal.v2 proposal from exactly the supplied Pi
Episode. Keep the Contract provisional, preserve unknowns in uncertainties and
open_questions, and never invent acceptance criteria, constraints, evidence, or
user confirmation. Anchor owns deterministic materialization. Do not submit
Checkpoint metadata, frontier, certificates, or content hashes. Submit exactly
once through anchor_submit_bootstrap and do not return free-text JSON.`;

export const UPDATE_SYSTEM = `You are the Anchor Update Agent.

This is state transition, not conversation summarization. Given one previous
Checkpoint and exactly one newly covered Episode, reconstruct the minimum
cognition needed for correct future action at the Episode frontier.

Rules:
- Preserve the Contract; never silently change goal, acceptance, constraints, or non-goals.
- Keep current Situation, useful Experience, and Intent distinct. Preserve causes and retry conditions, not chronology.
- Apply newer explicit user corrections. Keep hypotheses and unresolved conflicts explicit until evidence resolves them.
- Tool output can support an observed fact; model confidence cannot. Failure is not success.
- A failed or interrupted tool call is not success.
- Keep an item active only when forgetting it could cause a wrong decision, constraint violation, repeated failure/expense, or loss of the next action. Preserve failed paths with their causes and retry conditions.
- Give every previous active item exactly one disposition in the Transition Certificate: carry, revise, resolve, supersede, demote, or archive.
- The control envelope lists the exact previous active item IDs. The certificate must contain exactly those IDs; do not invent IDs, rename IDs, or mark an item carry unless that same ID is present in the submitted cognition.
- Non-carry dispositions require a reason and source. Demote requires an exact recoverable reference. No item may disappear silently.
- The anchor-update-input envelope is control, not an Episode message. Do not copy its instructions into cognition.
- current_directive, accepted_next_action, and the task goal are distinct.
- Submit exactly once through anchor_submit_update. Do not return free-text JSON or call any other function.`;

export const BOOTSTRAP_SYSTEM = `You are the Anchor Bootstrap Agent.

The user did not explicitly enter Anchor Planning. Create only a provisional
continuity state from exactly the supplied Pi Episode. Do not invent a goal,
constraint, acceptance criterion, decision, or fact that the Episode does not
support. Unknown requirements belong in open_questions. This is not a summary
and not a planning conversation.

Every cognition item needs a stable id, non-empty statement, sources, and
relevance. Preserve uncertainty explicitly. The Contract remains provisional
and may be corrected later; never mark it user-confirmed. Submit exactly once
through anchor_submit_bootstrap. Do not return free-text JSON or call any other
function.`;

const ITEM_GROUPS = [
  ["situation", "confirmed_facts"], ["situation", "active_hypotheses"], ["situation", "unresolved_conflicts"], ["situation", "blockers"],
  ["experience", "decisions"], ["experience", "failed_paths"], ["intent", "open_questions"],
];

export async function runUpdate(anchor, event, ctx) {
  const recovery = await anchor.recovery();
  if (!recovery.checkpoint) throw new Error("Anchor Update requires an existing Checkpoint");
  const episode = episodeMessages(event.preparation);
  const frontier = compactFrontier(event.preparation, ctx.sessionManager.getSessionId(), episode);
  if (sameValue(recovery.checkpoint.frontier, frontier)) return { compaction: compactionReceipt(recovery.task_id, recovery.checkpoint, event.preparation) };
  if (!ctx.model) throw new Error("Anchor Update requires an active model");
  const submissionTool = UPDATE_PROPOSAL_TOOL;
  const previousActiveItems = activeItems(recovery.checkpoint.cognition);
  const input = {
    schema: "anchor.update-input.v1",
    task: { task_id: recovery.task_id, title: recovery.task?.title, contract: recovery.contract?.content ?? recovery.contract },
    previous_checkpoint: recovery.checkpoint,
    transition_requirements: {
      previous_active_item_ids: previousActiveItems.map((item) => item.id),
      certificate_rule: "exactly one disposition for each listed ID and no other ID",
    },
    target_frontier: frontier,
  };
  const baseMessages = [{ role: "user", content: [{ type: "text", text: `<anchor-update-input>\n${JSON.stringify(input, null, 2)}\n</anchor-update-input>` }] }, ...convertToLlm(episode)];
  let response;
  let normalized;
  let validationError;
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const feedback = validationError ? [{ role: "user", content: [{ type: "text", text: `<anchor-validation-feedback>\nThe previous submission was rejected by deterministic validation: ${validationError.message}\nCorrect that issue and submit the complete candidate again through anchor_submit_update.\n</anchor-validation-feedback>` }] }] : [];
    response = await ctx.modelRegistry.complete(ctx.model, {
      systemPrompt: UPDATE_PROPOSAL_SYSTEM,
      messages: [...baseMessages, ...feedback],
      tools: [submissionTool],
    }, submissionOptions(ctx.model, event.signal));
    try {
      normalized = normalizeUpdateResponse(submissionArguments(response, submissionTool, "Update"), recovery.checkpoint.cognition, frontier);
      validationError = undefined;
      break;
    } catch (error) {
      validationError = error;
      if (attempt === 1) throw responseError(error, response, "Update");
    }
  }
  const committed = await anchor.update({ schema: "anchor.checkpoint-candidate.v1", frontier, cognition: normalized.cognition, transition_certificate: normalized.transition_certificate, provenance: { kind: "compact", model: modelName(ctx.model) } }, recovery.task?.state_version);
  return { compaction: { ...compactionReceipt(recovery.task_id, checkpointFromEvent(committed), event.preparation), usage: response.usage } };
}

export async function runBootstrap(anchor, event, ctx) {
  let episode;
  let frontier;
  try {
    episode = episodeMessages(event.preparation);
  } catch (error) {
    throw bootstrapStageError(error, "episode");
  }
  try {
    frontier = compactFrontier(event.preparation, ctx.sessionManager.getSessionId(), episode);
  } catch (error) {
    throw bootstrapStageError(error, "frontier");
  }
  if (!ctx.model) throw bootstrapStageError(new Error("Anchor Bootstrap requires an active model"), "model-transport");
  const submissionTool = BOOTSTRAP_PROPOSAL_TOOL;
  let response;
  try {
    response = await ctx.modelRegistry.complete(ctx.model, {
      systemPrompt: BOOTSTRAP_PROPOSAL_SYSTEM,
      messages: [...convertToLlm(episode)],
      tools: [submissionTool],
    }, submissionOptions(ctx.model, event.signal));
  } catch (error) {
    throw bootstrapStageError(error, "model-transport");
  }
  let parsed;
  let contract;
  let cognition;
  let title;
  try {
    parsed = submissionArguments(response, submissionTool, "Bootstrap");
    ({ title, contract, cognition } = normalizeBootstrapProposal(parsed, frontier));
  } catch (error) {
    throw bootstrapStageError(error, "response-validation");
  }
  try {
    const created = await anchor.bootstrap({
      sessionId: ctx.sessionManager.getSessionId(),
      proposalHash: frontier.episode_hash,
      title,
      contract,
      checkpoint: {
        schema: "anchor.checkpoint-candidate.v1",
        frontier,
        cognition,
        provenance: { kind: "compact", model: modelName(ctx.model), bootstrap: true },
      },
    });
    return { compaction: { ...compactionReceipt(created.task_id, created.checkpoint, event.preparation), usage: response.usage } };
  } catch (error) {
    throw bootstrapStageError(error, "state-bootstrap");
  }
}


function bootstrapStageError(error, stage) {
  const result = error instanceof Error ? error : new Error(String(error));
  result.bootstrapStage ??= stage;
  result.bootstrapErrorClass ??= error?.constructor?.name || "Error";
  return result;
}

export function episodeMessages(preparation) {
  if (!Array.isArray(preparation?.messagesToSummarize) || !Array.isArray(preparation?.turnPrefixMessages)) throw new TypeError("Pi compaction Episode is required");
  const episode = [...preparation.messagesToSummarize, ...preparation.turnPrefixMessages];
  if (!episode.length) throw new TypeError("Pi compaction Episode is empty");
  return episode;
}

export function compactFrontier(preparation, sessionId, episode = episodeMessages(preparation)) {
  if (typeof sessionId !== "string" || !sessionId.trim()) throw new TypeError("Pi session identity is required");
  if (typeof preparation?.firstKeptEntryId !== "string" || !preparation.firstKeptEntryId) throw new TypeError("Pi firstKeptEntryId is required");
  return { kind: "compact", session_id: sessionId, first_kept_entry_id: preparation.firstKeptEntryId, episode_hash: hashValue(episode), is_split_turn: Boolean(preparation.isSplitTurn) };
}

export function normalizeCognition(raw) {
  const parsed = structuredCandidate(raw);
  return parsed?.schema === "anchor.cognition.v3" ? normalizeV3(parsed) : normalizeLegacy(parsed);
}

export function normalizeUpdateResponse(raw, previous, frontier) {
  const parsed = structuredCandidate(raw);
  const legacyPrevious = previous?.schema !== "anchor.cognition.v3";
  const prior = previous?.schema === "anchor.cognition.v3" ? previous : legacyToV3(normalizeLegacy(previous));
  if (parsed?.schema === "anchor.update-proposal.v2") return reduceUpdateProposal(prior, parsed, frontier);
  if (parsed?.schema === "anchor.cognition.v3") return { cognition: normalizeV3(parsed), transition_certificate: legacyPrevious ? legacyTransition(prior, parsed, frontier) : normalizeTransition(parsed.transition_certificate, prior, parsed) };
  const cognition = normalizeLegacy(parsed);
  return { cognition: legacyToV3(cognition), transition_certificate: legacyTransition(prior, cognition, frontier) };
}

export function hashValue(value) { return `sha256:${createHash("sha256").update(canonicalJson(value)).digest("hex")}`; }

function normalizeV3(value) {
  if (!value || typeof value !== "object") throw new Error("Update Agent must return an object");
  const cognition = { schema: "anchor.cognition.v3", situation: {}, experience: {}, intent: {}, knowledge_index: [] };
  cognition.situation.current_understanding = required(value.situation?.current_understanding, "situation.current_understanding");
  for (const field of ["confirmed_facts", "active_hypotheses", "unresolved_conflicts", "blockers"]) cognition.situation[field] = itemList(value.situation?.[field], field);
  for (const field of ["decisions", "failed_paths"]) cognition.experience[field] = itemList(value.experience?.[field], field);
  cognition.intent.current_directive = required(value.intent?.current_directive, "intent.current_directive");
  cognition.intent.accepted_next_action = required(value.intent?.accepted_next_action, "intent.accepted_next_action");
  cognition.intent.next_plan = stringList(value.intent?.next_plan, "intent.next_plan");
  cognition.intent.open_questions = itemList(value.intent?.open_questions, "open_questions");
  cognition.knowledge_index = references(value.knowledge_index);
  return cognition;
}

function normalizeBootstrapContract(value) {
  if (!value || typeof value !== "object" || value.schema !== "anchor.contract.v1") throw new Error("Bootstrap Contract must use anchor.contract.v1");
  return {
    schema: "anchor.contract.v1",
    status: "provisional",
    goal: required(value.goal, "contract.goal"),
    execution_plan: required(value.execution_plan || "Not established.", "contract.execution_plan"),
    ...Object.fromEntries(["rationale", "acceptance_criteria", "constraints", "non_goals", "verification_commands", "allowed_paths", "risks"].map((field) => [field, stringList(value[field], `contract.${field}`)])),
  };
}

function normalizeBootstrapProposal(value, frontier) {
  if (value?.schema === "anchor.bootstrap.v1") return { title: required(value.title, "bootstrap.title"), contract: normalizeBootstrapContract(value.contract), cognition: normalizeV3(value.cognition) };
  if (!value || value.schema !== "anchor.bootstrap-proposal.v2") throw new Error("Bootstrap Agent must submit anchor.bootstrap-proposal.v2");
  const goal = required(value.goal, "bootstrap.goal");
  const openQuestions = stringList(value.intent?.open_questions, "bootstrap.intent.open_questions");
  const uncertainties = stringList(value.uncertainties, "bootstrap.uncertainties");
  const items = (value.new_items || []).map((entry, index) => {
    const group = entry.section?.split(".");
    if (!group || group.length !== 2 || !ITEM_GROUPS.some(([section, field]) => section === group[0] && field === group[1])) throw new Error(`invalid bootstrap cognition section ${entry.section}`);
    const statement = required(entry.statement, "bootstrap.item.statement");
    const sources = stringList(entry.sources, "bootstrap.item.sources");
    if (!sources.length) throw new Error("bootstrap.item.sources must not be empty");
    return { section: group[0], field: group[1], id: `item-${hashValue({ frontier, index, section: entry.section, statement }).slice(-16)}`, statement, sources, relevance: required(entry.relevance, "bootstrap.item.relevance") };
  });
  const cognition = { schema: "anchor.cognition.v3", situation: { current_understanding: uncertainties.length ? `${goal} Unknowns remain: ${uncertainties.join("; ")}` : goal, confirmed_facts: [], active_hypotheses: [], unresolved_conflicts: [], blockers: [] }, experience: { decisions: [], failed_paths: [] }, intent: { current_directive: required(value.intent?.current_directive, "bootstrap.intent.current_directive"), accepted_next_action: required(value.intent?.accepted_next_action, "bootstrap.intent.accepted_next_action"), next_plan: stringList(value.intent?.next_plan, "bootstrap.intent.next_plan"), open_questions: openQuestions.map((statement, index) => ({ id: `question-${hashValue({ frontier, index, statement }).slice(-16)}`, statement, sources: [`episode:${frontier.episode_hash}`], relevance: "requires clarification before execution" })) }, knowledge_index: [] };
  for (const item of items) cognition[item.section][item.field].push({ id: item.id, statement: item.statement, sources: item.sources, relevance: item.relevance });
  return { title: required(value.title || goal, "bootstrap.title"), contract: { schema: "anchor.contract.v1", status: "provisional", goal, execution_plan: "Not established.", rationale: [], acceptance_criteria: [], constraints: [], non_goals: [], risks: [], verification_commands: [], allowed_paths: [] }, cognition };
}

function normalizeTransition(value, previous, next) {
  const previousItems = activeItems(previous);
  if (!previousItems.length && value === undefined) return { schema: "anchor.transition.v1", dispositions: [] };
  if (!value || value.schema !== "anchor.transition.v1" || !Array.isArray(value.dispositions)) throw new Error("Transition Certificate is required");
  const seen = new Set();
  const previousIds = new Set(previousItems.map((item) => item.id));
  const nextIds = new Set(activeItems(next).map((item) => item.id));
  const dispositions = value.dispositions.map((entry) => {
    if (!entry || typeof entry !== "object" || typeof entry.item_id !== "string" || seen.has(entry.item_id)) throw new Error("Transition Certificate item coverage is invalid");
    if (!previousIds.has(entry.item_id)) throw new Error(`Transition Certificate references unknown previous item ${entry.item_id}`);
    seen.add(entry.item_id);
    if (!["carry", "revise", "resolve", "supersede", "demote", "archive"].includes(entry.disposition)) throw new Error("Transition disposition is invalid");
    const result = { item_id: entry.item_id, disposition: entry.disposition, reason: required(entry.reason, `transition.${entry.item_id}.reason`), sources: stringList(entry.sources, `transition.${entry.item_id}.sources`) };
    if (entry.disposition === "carry" && !nextIds.has(entry.item_id)) throw new Error("carried item is missing from cognition");
    if (entry.disposition === "revise" || entry.disposition === "supersede") {
      result.replacement_id = required(entry.replacement_id, `transition.${entry.item_id}.replacement_id`);
      if (!nextIds.has(result.replacement_id)) throw new Error(`transition replacement ${result.replacement_id} is missing`);
    }
    if (entry.disposition === "demote") {
      result.reference = required(entry.reference, `transition.${entry.item_id}.reference`);
      if (!/^checkpoint:\d+:item:[^:]+$/.test(result.reference)) throw new Error(`transition.${entry.item_id}.reference must identify an immutable Checkpoint item`);
      if (!next.knowledge_index.some((reference) => reference.locator === result.reference)) throw new Error(`transition.${entry.item_id}.reference must remain in knowledge_index`);
    }
    return result;
  });
  for (const item of previousItems) if (!seen.has(item.id)) throw new Error(`Transition Certificate omits ${item.id}`);
  return { schema: "anchor.transition.v1", dispositions };
}

function legacyTransition(previous, legacy, frontier) {
  const prior = previous?.schema === "anchor.cognition.v3" ? previous : legacyToV3(previous);
  const next = legacyToV3(legacy);
  const nextIds = new Set(activeItems(next).map((item) => item.id));
  return { schema: "anchor.transition.v1", dispositions: activeItems(prior).map((item) => ({ item_id: item.id, disposition: nextIds.has(item.id) ? "carry" : "archive", reason: nextIds.has(item.id) ? "Legacy cognition carried forward." : "Legacy cognition is no longer active.", sources: [`episode:${frontier.episode_hash}`] })) };
}

function legacyToV3(value) {
  const section = (group, field) => (value[field] ?? []).map((text) => ({ id: `${group}-${hashValue(text).slice(-12)}`, statement: text, sources: value.evidence_refs?.length ? value.evidence_refs : ["legacy:checkpoint"], relevance: group }));
  return { schema: "anchor.cognition.v3", situation: { current_understanding: value.current_understanding, confirmed_facts: section("confirmed_facts", "confirmed_facts"), active_hypotheses: section("active_hypotheses", "active_hypotheses"), unresolved_conflicts: section("unresolved_conflicts", "unresolved_conflicts"), blockers: section("blockers", "blockers") }, experience: { decisions: section("decisions", "decisions"), failed_paths: section("failed_paths", "failed_paths") }, intent: { current_directive: value.current_directive, accepted_next_action: value.accepted_next_action, next_plan: value.next_plan, open_questions: section("open_questions", "open_questions") }, knowledge_index: (value.evidence_refs ?? []).map((ref) => ({ id: `ref-${hashValue(ref).slice(-12)}`, cue: "Legacy evidence reference", locator: ref, source: ref })) };
}

function activeItems(cognition) { return cognition?.schema === "anchor.cognition.v3" ? ITEM_GROUPS.flatMap(([section, field]) => cognition[section]?.[field] ?? []) : []; }
function itemList(value, name) { if (value === undefined) return []; if (!Array.isArray(value)) throw new Error(`Update field ${name} must be item[]`); return value.map((item) => { const sources = stringList(item?.sources, `${name}.sources`); if (!sources.length) throw new Error(`Update field ${name}.sources must not be empty`); return { id: required(item?.id, `${name}.id`), statement: required(item?.statement ?? item?.text, `${name}.statement`), sources, relevance: required(item?.relevance, `${name}.relevance`) }; }); }
function references(value) { if (value === undefined) return []; if (!Array.isArray(value)) throw new Error("knowledge_index must be reference[]"); return value.map((ref) => { const contentHash = ref?.content_hash; if (contentHash !== undefined && contentHash !== null && contentHash !== "" && !/^sha256:[0-9a-f]{64}$/.test(contentHash)) throw new Error("knowledge_index.content_hash must be a sha256 digest"); return { id: required(ref?.id, "knowledge_index.id"), cue: required(ref?.cue, "knowledge_index.cue"), locator: required(ref?.locator, "knowledge_index.locator"), ...(contentHash ? { content_hash: contentHash } : {}), source: required(ref?.source, "knowledge_index.source") }; }); }
function normalizeLegacy(value) { if (!value || (value.schema !== undefined && value.schema !== "anchor.cognition.v2")) throw new Error("Update Agent must return anchor.cognition.v3"); const result = { schema: "anchor.cognition.v2" }; for (const field of ["current_understanding", "current_directive", "accepted_next_action"]) result[field] = required(value[field], field); for (const field of ["confirmed_facts", "active_hypotheses", "unresolved_conflicts", "decisions", "failed_paths", "blockers", "open_questions", "next_plan", "evidence_refs", "directive_history"]) result[field] = stringList(value[field], field); if (!result.next_plan.length) throw new Error("Update field next_plan is required"); return result; }
function structuredCandidate(raw) {
  if (typeof raw === "string") return parseJson(raw);
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error("Anchor candidate must be an object");
  return raw;
}

function parseJson(raw) {
  const text = String(raw ?? "").replace(/^\uFEFF/, "").trim();
  let initialError;
  try {
    return parseJsonCandidate(text);
  } catch (error) {
    initialError = error;
  }

  const fenced = [...text.matchAll(/```(?:json)?\s*([\s\S]*?)```/gi)];
  if (fenced.length > 1) throw invalidJsonError("multiple JSON code blocks", initialError);
  if (fenced.length === 1) {
    try {
      return parseJsonCandidate(fenced[0][1].trim());
    } catch (error) {
      throw invalidJsonError(jsonErrorReason(error), error);
    }
  }

  const objects = topLevelJsonObjects(text).flatMap((candidate) => {
    try {
      return [{ ...candidate, value: parseJsonCandidate(candidate.text) }];
    } catch {
      return [];
    }
  });
  if (objects.length > 1) throw invalidJsonError("multiple top-level JSON objects", initialError);
  if (objects.length === 1 && !text.slice(objects[0].end).trim()) return objects[0].value;
  throw invalidJsonError(jsonErrorReason(initialError), initialError);
}
function parseJsonCandidate(candidate) {
  try {
    return JSON.parse(candidate);
  } catch (error) {
    const repaired = repairJsonStrings(candidate);
    if (repaired !== candidate) return JSON.parse(repaired);
    throw error;
  }
}
function topLevelJsonObjects(text) {
  const objects = [];
  for (let start = text.indexOf("{"); start !== -1; start = text.indexOf("{", start + 1)) {
    let depth = 0;
    let inString = false;
    let escaped = false;
    for (let index = start; index < text.length; index += 1) {
      const char = text[index];
      if (inString) {
        if (escaped) escaped = false;
        else if (char === "\\") escaped = true;
        else if (char === '"') inString = false;
        continue;
      }
      if (char === '"') inString = true;
      else if (char === "{") depth += 1;
      else if (char === "}" && --depth === 0) {
        objects.push({ text: text.slice(start, index + 1), start, end: index + 1 });
        start = index;
        break;
      }
    }
  }
  return objects;
}
function repairJsonStrings(text) {
  const validEscapes = new Set(['"', "\\", "/", "b", "f", "n", "r", "t", "u"]);
  let repaired = "";
  let inString = false;
  for (let index = 0; index < text.length; index += 1) {
    const char = text[index];
    if (!inString) {
      repaired += char;
      if (char === '"') inString = true;
      continue;
    }
    if (char === '"') {
      repaired += char;
      inString = false;
      continue;
    }
    if (char === "\\") {
      const next = text[index + 1];
      if (next === undefined) {
        repaired += "\\\\";
        continue;
      }
      if (next === "u" && /^[0-9a-fA-F]{4}$/.test(text.slice(index + 2, index + 6))) {
        repaired += text.slice(index, index + 6);
        index += 5;
        continue;
      }
      if (validEscapes.has(next)) {
        repaired += `\\${next}`;
        index += 1;
        continue;
      }
      repaired += "\\\\";
      continue;
    }
    const code = char.codePointAt(0);
    repaired += code !== undefined && code <= 0x1f ? controlCharacterEscape(char, code) : char;
  }
  return repaired;
}
function controlCharacterEscape(char, code) {
  return { "\b": "\\b", "\f": "\\f", "\n": "\\n", "\r": "\\r", "\t": "\\t" }[char] ?? `\\u${code.toString(16).padStart(4, "0")}`;
}
function invalidJsonError(reason, cause) {
  return new Error(`Update Agent returned invalid JSON: ${reason}`, { cause });
}
function jsonErrorReason(error) {
  const message = error instanceof Error ? error.message : "";
  if (/Unexpected end of JSON input/i.test(message)) return "truncated JSON";
  if (/after JSON/i.test(message)) return "trailing content after the JSON object";
  const position = message.match(/position\s+(\d+)/i)?.[1];
  return position ? `syntax error at position ${position}` : "JSON syntax error";
}
function responseError(error, response, agent) {
  const message = error instanceof Error ? error.message : String(error);
  const output = messageText(response?.content);
  const stopReason = String(response?.stopReason || response?.rawStopReason || "unknown").replace(/[^a-zA-Z0-9_.-]/g, "").slice(0, 40) || "unknown";
  return new Error(`${agent} Agent response rejected: ${message} (stop_reason=${stopReason}, text_chars=${output.length}, text_hash=${hashValue(output)})`, { cause: error });
}
function submissionArguments(response, tool, agent) {
  const calls = Array.isArray(response?.content) ? response.content.filter((item) => item?.type === "toolCall") : [];
  if (calls.length !== 1 || calls[0]?.name !== tool.name) {
    throw new Error(`${agent} Agent must submit exactly one ${tool.name} function call; received ${calls.length} tool calls`);
  }
  if (!calls[0].arguments || typeof calls[0].arguments !== "object" || Array.isArray(calls[0].arguments)) {
    throw new Error(`${agent} Agent submitted invalid ${tool.name} arguments`);
  }
  const arguments_ = structuredClone(calls[0].arguments);
  normalizeOptionalNulls(arguments_, tool.parameters);
  if (!Check(tool.parameters, arguments_)) {
    const paths = [...Errors(tool.parameters, arguments_)].slice(0, 8).map((error) => error.path || "/");
    const suffix = paths.length ? ` at ${paths.join(", ")}` : "";
    throw new Error(`${agent} Agent submitted arguments that do not match the ${tool.name} schema${suffix}`);
  }
  return arguments_;
}

function normalizeOptionalNulls(value, schema) {
  if (Array.isArray(value)) {
    if (schema?.items) for (const item of value) normalizeOptionalNulls(item, schema.items);
    return;
  }
  if (!value || typeof value !== "object" || !schema?.properties) return;
  const requiredProperties = new Set(schema.required ?? []);
  for (const [key, propertySchema] of Object.entries(schema.properties)) {
    if (!(key in value)) continue;
    if (value[key] === null && !requiredProperties.has(key) && !Check(propertySchema, null)) delete value[key];
    else normalizeOptionalNulls(value[key], propertySchema);
  }
}

function submissionOptions(model, signal) {
  const anyChoiceApis = new Set(["anthropic-messages", "google-generative-ai", "google-vertex", "bedrock-converse"]);
  return { signal, toolChoice: anyChoiceApis.has(model?.api) ? "any" : "required" };
}
function required(value, name) { if (typeof value !== "string" || !value.trim()) throw new Error(`Update field ${name} is required`); return value.trim(); }
function stringList(value, name) { if (value === undefined) return []; if (!Array.isArray(value) || value.some((item) => typeof item !== "string" || !item.trim())) throw new Error(`Update field ${name} must be string[]`); return [...new Set(value.map((item) => item.trim()))]; }
function compactionReceipt(taskId, checkpoint, preparation) { return { summary: `Anchor Checkpoint ${checkpoint.checkpoint_version} committed for task ${taskId}.`, firstKeptEntryId: preparation.firstKeptEntryId, tokensBefore: preparation.tokensBefore, details: { schema: "anchor.compact-receipt.v1", task_id: taskId, checkpoint_version: checkpoint.checkpoint_version, checkpoint_hash: checkpoint.receipt?.content_hash, event_id: checkpoint.receipt?.event_id, frontier: checkpoint.frontier } }; }
function checkpointFromEvent(event) { return { ...event.payload, receipt: { event_id: event.event_id, content_hash: event.content_hash } }; }
function canonicalJson(value) { if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`; if (value && typeof value === "object") return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`; return JSON.stringify(value); }
function sameValue(left, right) { return canonicalJson(left) === canonicalJson(right); }
function modelName(model) { return [model?.provider, model?.id].filter(Boolean).join("/") || "unknown"; }
function messageText(content) { return Array.isArray(content) ? content.filter((item) => item?.type === "text").map((item) => item.text).join("\n").trim() : String(content ?? "").trim(); }
