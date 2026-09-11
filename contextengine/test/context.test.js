import test from "node:test";
import assert from "node:assert/strict";
import { compileContext, projectMessages, renderContext } from "../src/context.js";

const recovery = {
  task_id: "task-1",
  source_hash: "sha256:source",
  task: { title: "Ship adapter", state_version: 4 },
  contract: { content: { goal: "Ship adapter", acceptance_criteria: ["tests pass"] } },
  checkpoint: {
    schema: "anchor.checkpoint.v1",
    task_id: "task-1",
    checkpoint_version: 2,
    parent: { checkpoint_version: 1, content_hash: "sha256:parent" },
    frontier: { kind: "compact", session_id: "session-1", first_kept_entry_id: "entry-1", episode_hash: "sha256:episode", is_split_turn: false },
    cognition: {
      schema: "anchor.cognition.v2",
      current_understanding: "The boundary is stable.",
      current_directive: "Run tests.",
      accepted_next_action: "Run tests.",
      next_plan: ["Run tests."],
    },
    provenance: { kind: "compact", model: "test/model" },
    receipt: { event_id: "event-2", content_hash: "sha256:checkpoint" },
  },
};

const message = (role, content) => ({ role, content: typeof content === "string" ? [{ type: "text", text: content }] : content, timestamp: Date.now() });

test("context projection prepends RecoveryView without trimming Pi's active window", () => {
  const callId = "call-1";
  const messages = [
    { role: "compactionSummary", summary: "old summary", timestamp: Date.now() },
    message("user", "old request"),
    message("assistant", "old answer"),
    message("user", "continue the current plan"),
    message("assistant", [{ type: "toolCall", id: callId, name: "bash", arguments: { command: "npm test" } }]),
    { role: "toolResult", toolCallId: callId, toolName: "bash", content: [{ type: "text", text: "ok" }], timestamp: Date.now() },
  ];

  const projected = projectMessages(recovery, messages);
  assert.equal(projected.length, messages.length + 1);
  assert.deepEqual(projected.slice(1), messages);
  messages.forEach((item, index) => assert.equal(projected[index + 1], item));
  assert.match(projected[0].content[0].text, /The boundary is stable/);
  assert.equal(compileContext(recovery).checkpoint.checkpoint_version, 2);
  assert.equal(Object.hasOwn(compileContext(recovery), "source_hash"), false);
  assert.match(renderContext(compileContext(recovery)), /Checkpoint is the authoritative cognition/);
});

test("a fresh Agent can take over from the projected cognition", () => {
  const projected = projectMessages(recovery, []);
  const text = projected[0].content[0].text;
  assert.match(text, /Ship adapter/);
  assert.match(text, /The boundary is stable/);
  assert.match(text, /Run tests/);
  assert.equal(projected.length, 1);
});

test("stable cognition reaches a projection plateau across 100 Updates", () => {
  const sizes = [];
  for (let version = 100; version < 200; version += 1) {
    const view = structuredClone(recovery);
    view.checkpoint.checkpoint_version = version;
    sizes.push(JSON.stringify(compileContext(view)).length);
  }
  assert.equal(new Set(sizes).size, 1);
});
