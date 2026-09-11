import test from "node:test";
import assert from "node:assert/strict";
import { normalizeUpdateResponse } from "../src/update.js";

const item = (id, statement) => ({ id, statement, sources: ["episode:1"], relevance: "changes the next action" });

const previous = {
  schema: "anchor.cognition.v3",
  situation: {
    current_understanding: "The task is halfway complete.",
    confirmed_facts: [item("fact-1", "The API is available.")],
    active_hypotheses: [], unresolved_conflicts: [], blockers: [],
  },
  experience: { decisions: [item("decision-1", "Keep the adapter narrow.")], failed_paths: [] },
  intent: { current_directive: "Finish the adapter.", accepted_next_action: "Run the focused test.", next_plan: ["Run the focused test."], open_questions: [] },
  knowledge_index: [],
};

test("v3 Update requires complete transition coverage and preserves only active cognition", () => {
  const next = {
    schema: "anchor.cognition.v3",
    situation: { current_understanding: "The API is available and the adapter is ready for verification.", confirmed_facts: [item("fact-1", "The API is available.")], active_hypotheses: [], unresolved_conflicts: [], blockers: [] },
    experience: { decisions: [], failed_paths: [] },
    intent: { current_directive: "Finish the adapter.", accepted_next_action: "Run the focused test.", next_plan: ["Run the focused test."], open_questions: [] },
    knowledge_index: [{ id: "ref-decision-1", cue: "Prior decision", locator: "checkpoint:1:item:decision-1", source: "episode:2" }],
    transition_certificate: {
      schema: "anchor.transition.v1",
      dispositions: [
        { item_id: "fact-1", disposition: "carry", reason: "Still controls verification.", sources: ["episode:1"] },
        { item_id: "decision-1", disposition: "archive", reason: "The decision is now embodied by the implementation.", sources: ["episode:1"] },
      ],
    },
  };
  const result = normalizeUpdateResponse(JSON.stringify(next), previous, { episode_hash: "sha256:episode" });
  assert.equal(result.cognition.schema, "anchor.cognition.v3");
  assert.deepEqual(result.cognition.experience.decisions, []);
  assert.equal(result.transition_certificate.dispositions.length, 2);
  assert.throws(() => normalizeUpdateResponse(JSON.stringify({ ...next, transition_certificate: { schema: "anchor.transition.v1", dispositions: [] } }), previous, {}), /omits|coverage/);
  assert.throws(() => normalizeUpdateResponse(JSON.stringify({ ...next, transition_certificate: { ...next.transition_certificate, dispositions: [...next.transition_certificate.dispositions, { item_id: "new-item", disposition: "archive", reason: "Invalid extra disposition.", sources: ["episode:2"] }] } }), previous, {}), /unknown previous item/);
});

test("demotion requires a recoverable reference and replacement dispositions point to live items", () => {
  const next = {
    schema: "anchor.cognition.v3",
    situation: { current_understanding: "A corrected fact is active.", confirmed_facts: [item("fact-2", "The API is available with the new endpoint.")], active_hypotheses: [], unresolved_conflicts: [], blockers: [] },
    experience: { decisions: [], failed_paths: [] },
    intent: { current_directive: "Finish.", accepted_next_action: "Verify.", next_plan: ["Verify."], open_questions: [] }, knowledge_index: [{ id: "ref-decision-1", cue: "Prior decision", locator: "checkpoint:1:item:decision-1", source: "episode:2" }],
    transition_certificate: { schema: "anchor.transition.v1", dispositions: [
      { item_id: "fact-1", disposition: "revise", reason: "User correction supersedes the old endpoint.", sources: ["episode:2"], replacement_id: "fact-2" },
      { item_id: "decision-1", disposition: "demote", reason: "Useful for audit only.", sources: ["episode:2"], reference: "checkpoint:1:item:decision-1" },
    ] },
  };
  assert.doesNotThrow(() => normalizeUpdateResponse(JSON.stringify(next), previous, {}));
  assert.throws(() => normalizeUpdateResponse(JSON.stringify({ ...next, transition_certificate: { ...next.transition_certificate, dispositions: next.transition_certificate.dispositions.map((entry) => entry.item_id === "decision-1" ? { ...entry, reference: undefined } : entry) } }), previous, {}), /reference/);
  assert.throws(() => normalizeUpdateResponse(JSON.stringify({ ...next, transition_certificate: { ...next.transition_certificate, dispositions: next.transition_certificate.dispositions.map((entry) => entry.item_id === "decision-1" ? { ...entry, reference: "transcript:old" } : entry) } }), previous, {}), /immutable Checkpoint/);
});
