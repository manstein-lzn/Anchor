import { createHash } from "node:crypto";

const GROUPS = [
  ["situation", "confirmed_facts"], ["situation", "active_hypotheses"],
  ["situation", "unresolved_conflicts"], ["situation", "blockers"],
  ["experience", "decisions"], ["experience", "failed_paths"],
  ["intent", "open_questions"],
];

export function reduceUpdateProposal(previous, proposal, frontier) {
  if (!previous || previous.schema !== "anchor.cognition.v3") throw new Error("Reducer requires anchor.cognition.v3 previous cognition");
  validateProposal(proposal);
  const ledger = new Map();
  for (const [section, field] of GROUPS) for (const item of previous[section]?.[field] ?? []) {
    if (ledger.has(item.id)) throw new Error(`duplicate previous item ${item.id}`);
    ledger.set(item.id, { section, field, item });
  }
  const decisions = new Map();
  for (const decision of proposal.item_decisions) {
    if (!ledger.has(decision.item_id)) throw new Error(`unknown previous item ${decision.item_id}`);
    if (decisions.has(decision.item_id)) throw new Error(`duplicate decision ${decision.item_id}`);
    decisions.set(decision.item_id, decision);
  }
  for (const id of ledger.keys()) if (!decisions.has(id)) throw new Error(`proposal omits ${id}`);
  const next = structuredClone(previous);
  next.knowledge_index = [];
  const carriedOpenQuestions = [];
  for (const [section, field] of GROUPS) next[section][field] = [];
  const dispositions = [];
  const usedIds = new Set();
  const add = (section, field, item) => {
    if (usedIds.has(item.id)) throw new Error(`duplicate generated item id ${item.id}`);
    usedIds.add(item.id);
    next[section][field].push(item);
  };
  for (const [section, field] of GROUPS) for (const item of previous[section]?.[field] ?? []) if (item.id) usedIds.add(item.id);
  for (const [section, field] of GROUPS) next[section][field] = [];
  usedIds.clear();
  for (const [id, prior] of ledger) {
    const op = decisions.get(id);
    const base = { item_id: id, disposition: op.disposition, reason: op.reason || `Reducer applied ${op.disposition}.`, sources: op.sources?.length ? [...op.sources] : ["reducer"] };
    validateOperation(op);
    if (op.disposition === "carry") {
      add(prior.section, prior.field, structuredClone(prior.item));
      if (prior.section === "intent") carriedOpenQuestions.push(structuredClone(prior.item));
    }
    else if (["revise", "supersede"].includes(op.disposition)) {
      const replacement = replacementItem(op.replacement, previous, frontier, proposal.item_decisions.indexOf(op));
      add(replacement.section, replacement.field, replacement.item); base.replacement_id = replacement.item.id;
    } else if (op.disposition === "demote") {
      if (!/^checkpoint:\d+:item:[^:]+$/.test(op.reference || "")) throw new Error(`demote ${id} requires an exact Checkpoint reference`);
      next.knowledge_index.push({ id: deterministicId("ref", op.reference, frontier), cue: op.reason, locator: op.reference, source: op.sources?.[0] || "reducer" });
      base.reference = op.reference;
    }
    dispositions.push(base);
  }
  for (const [index, item] of (proposal.new_items || []).entries()) {
    const normalized = normalizeItem(item, previous, frontier, index);
    add(normalized.section, normalized.field, normalized.item);
  }
  next.situation.current_understanding = required(proposal.situation?.current_understanding, "situation.current_understanding");
  const openQuestions = next.intent.open_questions;
  next.intent = normalizeIntent(proposal.intent, openQuestions.length ? openQuestions : carriedOpenQuestions);
  const references = new Map((next.knowledge_index || []).map((ref) => [ref.locator, ref]));
  for (const [index, ref] of (proposal.knowledge_index || []).entries()) {
    const normalized = normalizeReference(ref, frontier, index);
    references.set(normalized.locator, normalized);
  }
  next.knowledge_index = [...references.values()];
  return { cognition: next, transition_certificate: { schema: "anchor.transition.v1", dispositions } };
}

function replacementItem(value, previous, frontier, index) {
  const normalized = normalizeItem(value, previous, frontier, index);
  return normalized;
}
function normalizeItem(value, _previous, frontier, index) {
  if (!value || typeof value !== "object") throw new Error("proposal item is required");
  const section = value.section || "situation.confirmed_facts";
  const [group, field] = section.split(".");
  if (!GROUPS.some(([s, f]) => s === group && f === field)) throw new Error(`invalid cognition section ${section}`);
  const statement = required(value.statement, "item.statement");
  const item = { id: deterministicId("item", `${section}:${statement}:${index}`, frontier), statement, sources: [...(value.sources || [])], relevance: required(value.relevance, "item.relevance") };
  if (!item.sources.length) throw new Error("item.sources must not be empty");
  return { section: group, field, item };
}
function normalizeIntent(intent, carriedOpenQuestions) {
  return { current_directive: required(intent?.current_directive, "intent.current_directive"), accepted_next_action: required(intent?.accepted_next_action, "intent.accepted_next_action"), next_plan: intent?.next_plan?.length ? [...intent.next_plan] : [required(intent?.accepted_next_action, "intent.accepted_next_action")], open_questions: carriedOpenQuestions };
}
function normalizeReference(value, frontier, index) {
  if (!value || typeof value !== "object") throw new Error("knowledge reference is required");
  return { id: deterministicId("ref", `${value.locator}:${index}`, frontier), cue: required(value.cue, "knowledge_index.cue"), locator: required(value.locator, "knowledge_index.locator"), source: required(value.source, "knowledge_index.source") };
}
function validateProposal(proposal) {
  if (!proposal || proposal.schema !== "anchor.update-proposal.v2" || !Array.isArray(proposal.item_decisions)) throw new Error("Invalid anchor.update-proposal.v2");
  if (proposal.new_items !== undefined && !Array.isArray(proposal.new_items)) throw new Error("proposal.new_items must be an array");
  if (proposal.knowledge_index !== undefined && !Array.isArray(proposal.knowledge_index)) throw new Error("proposal.knowledge_index must be an array");
}
function validateOperation(operation) {
  const dispositions = ["carry", "revise", "resolve", "supersede", "demote", "archive"];
  if (!dispositions.includes(operation.disposition)) throw new Error(`invalid disposition for ${operation.item_id}`);
  if (operation.disposition === "carry" && (operation.replacement || operation.reference)) throw new Error(`carry ${operation.item_id} cannot include replacement or reference`);
  if (["revise", "supersede"].includes(operation.disposition) && !operation.replacement) throw new Error(`${operation.disposition} ${operation.item_id} requires replacement`);
  if (operation.disposition === "demote" && !operation.reference) throw new Error(`demote ${operation.item_id} requires reference`);
  if (operation.disposition !== "carry" && (!Array.isArray(operation.sources) || !operation.sources.length)) throw new Error(`${operation.disposition} ${operation.item_id} requires sources`);
  if (operation.disposition !== "carry" && (!operation.reason || !String(operation.reason).trim())) throw new Error(`${operation.disposition} ${operation.item_id} requires reason`);
}
function required(value, name) { if (typeof value !== "string" || !value.trim()) throw new Error(`${name} is required`); return value.trim(); }
function deterministicId(kind, value, frontier) { return `${kind}-${createHash("sha256").update(JSON.stringify([kind, value, frontier])).digest("hex").slice(0, 16)}`; }
