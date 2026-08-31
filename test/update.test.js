import test from "node:test";
import assert from "node:assert/strict";
import { compactFrontier, hashValue, normalizeCognition, runUpdate, UPDATE_SYSTEM } from "../src/update.js";

const cognition = (overrides = {}) => ({
  current_understanding: "The provider boundary is stable.",
  current_directive: "Finish the bounded Update path.",
  accepted_next_action: "Run the focused test.",
  confirmed_facts: ["Tool replay is grouped."],
  active_hypotheses: [],
  unresolved_conflicts: [],
  decisions: ["Use Pi's compaction boundary."],
  failed_paths: ["Full branch replay overflowed because it grew without bound."],
  blockers: [],
  open_questions: [],
  next_plan: ["Run the focused test."],
  evidence_refs: ["entry-tool-result"],
  directive_history: ["Finish the bounded Update path."],
  ...overrides,
});

test("Update output is normalized into complete current cognition", () => {
  const result = normalizeCognition("```json\n" + JSON.stringify(cognition({
    confirmed_facts: [" Tool replay is grouped. ", "Tool replay is grouped."],
  })) + "\n```");

  assert.equal(result.schema, "anchor.cognition.v2");
  assert.equal(result.current_directive, "Finish the bounded Update path.");
  assert.equal(result.accepted_next_action, "Run the focused test.");
  assert.deepEqual(result.confirmed_facts, ["Tool replay is grouped."]);
  assert.deepEqual(result.unresolved_conflicts, []);
});

test("incomplete Update output is rejected instead of becoming State", () => {
  assert.throws(() => normalizeCognition(JSON.stringify(cognition({ current_directive: "" }))), /current_directive/);
  assert.throws(() => normalizeCognition(JSON.stringify(cognition({ next_plan: [] }))), /next_plan/);
  assert.throws(() => normalizeCognition("not json"), /invalid JSON/);
});

test("Update discipline covers corrections, uncertainty, failures, and directive identity", () => {
  for (const phrase of ["explicit user corrections", "hypotheses", "unresolved conflicts", "failed paths", "failed or interrupted tool call", "current_directive", "accepted_next_action"]) {
    assert.match(UPDATE_SYSTEM, new RegExp(phrase, "i"));
  }
});

test("compact Update consumes only Pi's bounded Episode and commits its frontier", async () => {
  const calls = [];
  const preparation = {
    firstKeptEntryId: "entry-kept",
    tokensBefore: 123,
    messagesToSummarize: [{ role: "user", content: [{ type: "text", text: "bounded work" }] }],
    turnPrefixMessages: [],
    isSplitTurn: false,
  };
  const expectedFrontier = compactFrontier(preparation, "session-1");
  const previous = checkpoint(0, { kind: "planning", session_id: "session-1", source_hash: hashValue({ proposal: 1 }) });
  let modelRequest;
  const anchor = {
    async recovery() {
      return {
        task_id: "task-1",
        task: { state_version: 7, title: "test" },
        contract: { content: { goal: "test" } },
        checkpoint: previous,
      };
    },
    async update(value, version) {
      calls.push({ value, version });
      return {
        event_id: "event-1",
        content_hash: hashValue(value),
        payload: {
          schema: "anchor.checkpoint.v1",
          task_id: "task-1",
          checkpoint_version: 1,
          parent: { checkpoint_version: 0, content_hash: previous.receipt.content_hash },
          ...value,
        },
      };
    },
  };
  const event = {
    signal: new AbortController().signal,
    preparation,
    branchEntries: [{ role: "user", content: "DO NOT SEND THE FULL BRANCH" }],
  };
  const result = await runUpdate(anchor, event, {
    model: { provider: "test", id: "model" },
    sessionManager: { getSessionId: () => "session-1" },
    modelRegistry: {
      complete: async (_model, request) => {
        modelRequest = request;
        return { content: [{ type: "text", text: JSON.stringify(cognition()) }], usage: { input: 10, output: 2 } };
      },
    },
  });

  assert.equal(calls.length, 1);
  assert.equal(calls[0].version, 7);
  assert.equal(calls[0].value.schema, "anchor.checkpoint-candidate.v1");
  assert.deepEqual(calls[0].value.frontier, expectedFrontier);
  assert.equal(JSON.stringify(modelRequest.messages).includes("bounded work"), true);
  assert.equal(JSON.stringify(modelRequest.messages).includes("DO NOT SEND THE FULL BRANCH"), false);
  assert.equal(JSON.stringify(modelRequest.messages).includes("Return the complete anchor.cognition.v2"), false);
  assert.equal(modelRequest.messages.at(-1).content[0].text, "bounded work");
  assert.match(modelRequest.systemPrompt, /Every list item is a non-empty\s+string, never an object/);
  assert.match(modelRequest.systemPrompt, /"failed_paths": \["string"\]/);
  assert.equal(result.compaction.summary, "Anchor Checkpoint 1 committed for task task-1.");
  assert.equal(result.compaction.firstKeptEntryId, "entry-kept");
  assert.deepEqual(result.compaction.usage, { input: 10, output: 2 });
  assert.equal(result.compaction.details.schema, "anchor.compact-receipt.v1");
  assert.deepEqual(result.compaction.details.frontier, expectedFrontier);
});

test("an already committed frontier replays its receipt without another model call", async () => {
  const preparation = {
    firstKeptEntryId: "entry-kept",
    tokensBefore: 123,
    messagesToSummarize: [{ role: "user", content: [{ type: "text", text: "bounded work" }] }],
    turnPrefixMessages: [],
    isSplitTurn: false,
  };
  const frontier = compactFrontier(preparation, "session-1");
  const existing = checkpoint(3, frontier);
  const result = await runUpdate({
    recovery: async () => ({ task_id: "task-1", task: { state_version: 9 }, checkpoint: existing }),
    update: async () => assert.fail("receipt replay must not write State"),
  }, { preparation, signal: new AbortController().signal }, {
    model: { id: "unused" },
    sessionManager: { getSessionId: () => "session-1" },
    modelRegistry: { complete: async () => assert.fail("receipt replay must not invoke the model") },
  });

  assert.equal(result.compaction.details.event_id, existing.receipt.event_id);
  assert.equal(result.compaction.details.checkpoint_version, 3);
  assert.equal(Object.hasOwn(result.compaction, "usage"), false);
});

function checkpoint(version, frontier) {
  return {
    schema: "anchor.checkpoint.v1",
    task_id: "task-1",
    checkpoint_version: version,
    parent: null,
    frontier,
    cognition: { schema: "anchor.cognition.v2", ...cognition() },
    provenance: { kind: frontier.kind, model: "test/model", ...(frontier.kind === "planning" ? { confirmed_by: "user" } : {}) },
    receipt: { event_id: `event-${version}`, content_hash: hashValue({ version, frontier }) },
  };
}
