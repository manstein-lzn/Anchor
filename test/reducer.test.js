import test from "node:test";
import assert from "node:assert/strict";
import { reduceUpdateProposal } from "../src/reducer.js";

const item = (id, statement) => ({ id, statement, sources: ["episode:1"], relevance: "controls next action" });
const previous = { schema: "anchor.cognition.v3", situation: { current_understanding: "old", confirmed_facts: [item("fact-1", "API exists")], active_hypotheses: [], unresolved_conflicts: [], blockers: [] }, experience: { decisions: [item("decision-1", "Keep adapter narrow")], failed_paths: [] }, intent: { current_directive: "Finish", accepted_next_action: "Test", next_plan: ["Test"], open_questions: [] }, knowledge_index: [] };
const proposal = (overrides = {}) => ({ schema: "anchor.update-proposal.v2", situation: { current_understanding: "new" }, intent: { current_directive: "Finish", accepted_next_action: "Test", next_plan: ["Test"] }, item_decisions: [{ item_id: "fact-1", disposition: "carry" }, { item_id: "decision-1", disposition: "archive", reason: "implemented", sources: ["episode:2"] }], new_items: [], knowledge_index: [], ...overrides });

test("reducer copies carry and emits complete certificate", () => {
  const result = reduceUpdateProposal(previous, proposal(), { episode_hash: "sha256:e" });
  assert.deepEqual(result.cognition.situation.confirmed_facts[0], previous.situation.confirmed_facts[0]);
  assert.equal(result.cognition.experience.decisions.length, 0);
  assert.equal(result.transition_certificate.dispositions.length, 2);
});

test("reducer rejects omitted, duplicate and unknown decisions", () => {
  assert.throws(() => reduceUpdateProposal(previous, proposal({ item_decisions: [{ item_id: "fact-1", disposition: "carry" }] }), {}), /omits/);
  assert.throws(() => reduceUpdateProposal(previous, proposal({ item_decisions: [...proposal().item_decisions, { item_id: "fact-1", disposition: "carry" }] }), {}), /duplicate/);
  assert.throws(() => reduceUpdateProposal(previous, proposal({ item_decisions: [{ item_id: "fact-1", disposition: "carry" }, { item_id: "decision-1", disposition: "archive" }, { item_id: "x", disposition: "archive" }] }), {}), /unknown/);
});

test("reducer demotion requires exact reference and is deterministic", () => {
  const p = proposal({ item_decisions: [{ item_id: "fact-1", disposition: "demote", reason: "recoverable", sources: ["episode:2"], reference: "checkpoint:1:item:fact-1" }, { item_id: "decision-1", disposition: "archive" }] });
  const a = reduceUpdateProposal(previous, p, { episode_hash: "sha256:e" });
  const b = reduceUpdateProposal(previous, p, { episode_hash: "sha256:e" });
  assert.deepEqual(a, b);
  assert.throws(() => reduceUpdateProposal(previous, { ...p, item_decisions: [{ ...p.item_decisions[0], reference: "transcript:1" }, p.item_decisions[1]] }, {}), /exact Checkpoint/);
});
