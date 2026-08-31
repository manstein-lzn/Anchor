import { createHash } from "node:crypto";
import { convertToLlm } from "@earendil-works/pi-coding-agent";

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
- Non-carry dispositions require a reason and source. Demote requires an exact recoverable reference. No item may disappear silently.
- The anchor-update-input envelope is control, not an Episode message. Do not copy its instructions into cognition.
- Legacy list outputs are accepted only by migration tests: every legacy list item is a non-empty string, never an object.
- Every list item is a non-empty string, never an object, in legacy compatibility input.
- Legacy shape example: \"failed_paths\": [\"string\"].
- Return JSON only using anchor.cognition.v3 and anchor.transition.v1. Every item has stable id, non-empty statement, sources, and relevance.
{
  "schema": "anchor.cognition.v3",
  "situation": {"current_understanding":"string","confirmed_facts":[{"id":"fact-1","statement":"string","sources":["episode:..."],"relevance":"string"}],"active_hypotheses":[],"unresolved_conflicts":[],"blockers":[]},
  "experience": {"decisions":[],"failed_paths":[]},
  "intent": {"current_directive":"string","accepted_next_action":"string","next_plan":["string"],"open_questions":[]},
  "knowledge_index": [{"id":"ref-1","cue":"string","locator":"string","source":"episode:..."}]
}
{"schema":"anchor.transition.v1","dispositions":[{"item_id":"fact-1","disposition":"carry|revise|resolve|supersede|demote|archive","reason":"string","sources":["episode:..."],"replacement_id":"optional","reference":"optional"}]}`;

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
  const input = { schema: "anchor.update-input.v1", task: { task_id: recovery.task_id, title: recovery.task?.title, contract: recovery.contract?.content ?? recovery.contract }, previous_checkpoint: recovery.checkpoint, target_frontier: frontier };
  const response = await ctx.modelRegistry.complete(ctx.model, {
    systemPrompt: UPDATE_SYSTEM,
    messages: [{ role: "user", content: [{ type: "text", text: `<anchor-update-input>\n${JSON.stringify(input, null, 2)}\n</anchor-update-input>` }] }, ...convertToLlm(episode)],
  }, { signal: event.signal });
  const normalized = normalizeUpdateResponse(messageText(response.content), recovery.checkpoint.cognition, frontier);
  const committed = await anchor.update({ schema: "anchor.checkpoint-candidate.v1", frontier, cognition: normalized.cognition, transition_certificate: normalized.transition_certificate, provenance: { kind: "compact", model: modelName(ctx.model) } }, recovery.task?.state_version);
  return { compaction: { ...compactionReceipt(recovery.task_id, checkpointFromEvent(committed), event.preparation), usage: response.usage } };
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
  const parsed = parseJson(raw);
  return parsed?.schema === "anchor.cognition.v3" ? normalizeV3(parsed) : normalizeLegacy(parsed);
}

export function normalizeUpdateResponse(raw, previous, frontier) {
  const parsed = parseJson(raw);
  if (parsed?.schema === "anchor.cognition.v3") return { cognition: normalizeV3(parsed), transition_certificate: normalizeTransition(parsed.transition_certificate, previous, parsed) };
  const cognition = normalizeLegacy(parsed);
  return { cognition: legacyToV3(cognition), transition_certificate: legacyTransition(previous, cognition, frontier) };
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

function normalizeTransition(value, previous, next) {
  const previousItems = activeItems(previous);
  if (!previousItems.length && value === undefined) return { schema: "anchor.transition.v1", dispositions: [] };
  if (!value || value.schema !== "anchor.transition.v1" || !Array.isArray(value.dispositions)) throw new Error("Transition Certificate is required");
  const seen = new Set();
  const nextIds = new Set(activeItems(next).map((item) => item.id));
  const dispositions = value.dispositions.map((entry) => {
    if (!entry || typeof entry !== "object" || typeof entry.item_id !== "string" || seen.has(entry.item_id)) throw new Error("Transition Certificate item coverage is invalid");
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
function references(value) { if (value === undefined) return []; if (!Array.isArray(value)) throw new Error("knowledge_index must be reference[]"); return value.map((ref) => ({ id: required(ref?.id, "knowledge_index.id"), cue: required(ref?.cue, "knowledge_index.cue"), locator: required(ref?.locator, "knowledge_index.locator"), ...(ref?.content_hash ? { content_hash: ref.content_hash } : {}), source: required(ref?.source, "knowledge_index.source") })); }
function normalizeLegacy(value) { if (!value || (value.schema !== undefined && value.schema !== "anchor.cognition.v2")) throw new Error("Update Agent must return anchor.cognition.v3"); const result = { schema: "anchor.cognition.v2" }; for (const field of ["current_understanding", "current_directive", "accepted_next_action"]) result[field] = required(value[field], field); for (const field of ["confirmed_facts", "active_hypotheses", "unresolved_conflicts", "decisions", "failed_paths", "blockers", "open_questions", "next_plan", "evidence_refs", "directive_history"]) result[field] = stringList(value[field], field); if (!result.next_plan.length) throw new Error("Update field next_plan is required"); return result; }
function parseJson(raw) { const match = String(raw ?? "").match(/```(?:json)?\s*([\s\S]*?)```/i); try { return JSON.parse(match ? match[1] : raw); } catch { throw new Error("Update Agent returned invalid JSON"); } }
function required(value, name) { if (typeof value !== "string" || !value.trim()) throw new Error(`Update field ${name} is required`); return value.trim(); }
function stringList(value, name) { if (value === undefined) return []; if (!Array.isArray(value) || value.some((item) => typeof item !== "string" || !item.trim())) throw new Error(`Update field ${name} must be string[]`); return [...new Set(value.map((item) => item.trim()))]; }
function compactionReceipt(taskId, checkpoint, preparation) { return { summary: `Anchor Checkpoint ${checkpoint.checkpoint_version} committed for task ${taskId}.`, firstKeptEntryId: preparation.firstKeptEntryId, tokensBefore: preparation.tokensBefore, details: { schema: "anchor.compact-receipt.v1", task_id: taskId, checkpoint_version: checkpoint.checkpoint_version, checkpoint_hash: checkpoint.receipt?.content_hash, event_id: checkpoint.receipt?.event_id, frontier: checkpoint.frontier } }; }
function checkpointFromEvent(event) { return { ...event.payload, receipt: { event_id: event.event_id, content_hash: event.content_hash } }; }
function canonicalJson(value) { if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`; if (value && typeof value === "object") return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`; return JSON.stringify(value); }
function sameValue(left, right) { return canonicalJson(left) === canonicalJson(right); }
function modelName(model) { return [model?.provider, model?.id].filter(Boolean).join("/") || "unknown"; }
function messageText(content) { return Array.isArray(content) ? content.filter((item) => item?.type === "text").map((item) => item.text).join("\n").trim() : String(content ?? "").trim(); }
