import test from "node:test";
import assert from "node:assert/strict";
import { reduceUpdateProposal } from "../src/reducer.js";

const item = (id, statement) => ({ id, statement, sources: ["episode:1"], relevance: "controls next action" });
const previous = { schema: "anchor.cognition.v3", situation: { current_understanding: "old", confirmed_facts: [item("fact-1", "API exists")], active_hypotheses: [], unresolved_conflicts: [], blockers: [] }, experience: { decisions: [item("decision-1", "Keep adapter narrow")], failed_paths: [] }, intent: { current_directive: "Finish", accepted_next_action: "Test", next_plan: ["Test"], open_questions: [] }, knowledge_index: [] };
const proposal = (overrides = {}) => ({ schema: "anchor.update-proposal.v3", situation: { current_understanding: "new" }, intent: { current_directive: "Finish", accepted_next_action: "Test", next_plan: ["Test"] }, carry_ids: ["fact-1"], revise: [], resolve: [], supersede: [], demote: [], archive: [{ item_id: "decision-1", reason: "implemented", sources: ["episode:2"] }], new_items: [], knowledge_index: [], ...overrides });

test("reducer copies carry and emits complete certificate", () => {
  const result = reduceUpdateProposal(previous, proposal(), { episode_hash: "sha256:e" });
  assert.deepEqual(result.cognition.situation.confirmed_facts[0], previous.situation.confirmed_facts[0]);
  assert.equal(result.cognition.experience.decisions.length, 0);
  assert.equal(result.transition_certificate.dispositions.length, 2);
});

test("reducer rejects omitted, duplicate and unknown decisions", () => {
  assert.throws(() => reduceUpdateProposal(previous, proposal({ carry_ids: [], archive: [{ item_id: "decision-1", reason: "done", sources: ["episode:2"] }] }), {}), /omits/);
  assert.throws(() => reduceUpdateProposal(previous, proposal({ carry_ids: ["fact-1", "fact-1"] }), {}), /duplicate/);
  assert.throws(() => reduceUpdateProposal(previous, proposal({ carry_ids: ["fact-1", "x"] }), {}), /unknown/);
});

test("reducer demotion requires exact reference and is deterministic", () => {
  const p = proposal({ carry_ids: [], demote: [{ item_id: "fact-1", reason: "recoverable", sources: ["episode:2"], reference: "checkpoint:1:item:fact-1" }] });
  const a = reduceUpdateProposal(previous, p, { episode_hash: "sha256:e" });
  const b = reduceUpdateProposal(previous, p, { episode_hash: "sha256:e" });
  assert.deepEqual(a, b);
  assert.throws(() => reduceUpdateProposal(previous, { ...p, demote: [{ ...p.demote[0], reference: "transcript:1" }] }, {}), /exact Checkpoint/);
});

test("reducer materializes references and preserves open-question operations", () => {
  const prior = structuredClone(previous);
  prior.intent.open_questions = [item("question-1", "Is the endpoint stable?")];
  const result = reduceUpdateProposal(prior, proposal({
    carry_ids: ["fact-1"],
    archive: [{ item_id: "decision-1", reason: "done", sources: ["episode:2"] }],
    revise: [{ item_id: "question-1", reason: "verified", replacement: { section: "intent.open_questions", statement: "Is the provider replayable?", sources: ["episode:2"], relevance: "blocks verification" } }],
    new_items: [{ section: "experience.decisions", statement: "Use structured submission.", sources: ["episode:2"], relevance: "prevents free-text failure" }],
    knowledge_index: [{ cue: "Prior evidence", locator: "checkpoint:1:item:decision-1", source: "episode:2" }],
  }), { episode_hash: "sha256:e" });
  assert.equal(result.cognition.intent.open_questions[0].statement, "Is the provider replayable?");
  assert.equal(result.cognition.knowledge_index[0].id.startsWith("ref-"), true);
  assert.equal(result.cognition.experience.decisions.length, 1);
});

test("reducer rejects model-authored or unbound evidence sources", () => {
  assert.throws(() => reduceUpdateProposal(previous, proposal({ new_items: [{ section: "situation.confirmed_facts", statement: "Unverified", sources: ["model:confidence"], relevance: "none" }] }), { episode_hash: "sha256:e" }), /allowed evidence references/);
  assert.throws(() => reduceUpdateProposal(previous, proposal({ knowledge_index: [{ cue: "bad", locator: "x", source: "model:confidence" }] }), { episode_hash: "sha256:e" }), /allowed evidence references/);
});
