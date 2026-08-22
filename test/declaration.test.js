import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AnchorRuntime, normalizeDeclaration, parseStateDelta } from "../src/runtime.js";
import { StateStore } from "../src/state.js";

const DECLARATION = [
  "Work done.",
  "```anchor-state-delta",
  JSON.stringify({
    state_delta: { completed: ["fold13 summary found"], decisions: ["aggregate 11-13 first"], next_action: "compute mean tau" },
    belief_ops: [{ op: "add", belief: { id: "sh:fold13-weak", text: "fold13 blind Tau is 0.30, far below peers.", kind: "negative_result", scope: "workstream:tgraph_alignment", confidence: "high" } }],
  }),
  "```",
].join("\n");

function makeRuntime(store, promptImpl) {
  const listeners = [];
  const session = {
    messages: [],
    // The wrapper installed at bind time delegates to this implementation;
    // tests fire agent_end through listeners[0] to gate the capture.
    prompt: async (text) => promptImpl(text),
    agent: {
      transformContext: undefined,
      subscribe(listener) { listeners.push(listener); return () => {}; },
    },
    dispose() {},
  };
  const runtime = new AnchorRuntime({ session, store, purpose: "work", turnBudget: { maxTurnChars: 1_000_000 } });
  const endTurn = () => listeners[0]({ type: "agent_end", messages: [] });
  return { runtime, session, listeners, endTurn };
}

test("parseStateDelta extracts fenced declarations and rejects malformed ones", () => {
  const parsed = parseStateDelta(`before\n\`\`\`anchor-state-delta\n{"state_delta":{"next_action":"x"}}\n\`\`\`\nafter`);
  assert.deepEqual(parsed.state_delta.next_action, "x");
  assert.equal(parseStateDelta("no block here"), null);
  assert.equal(parseStateDelta("```anchor-state-delta\nnot json\n```"), null);
});

test("prompt commits a valid declaration into State (delta + beliefs)", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "capture cognition" });
  const { runtime, session, endTurn } = makeRuntime(store, () => {
    session.messages = [{ role: "assistant", content: [{ type: "text", text: DECLARATION }] }];
  });

  // Simulate a completed turn, then any post-turn prompt resolution
  // triggers the gated capture.
  endTurn();
  await runtime.prompt("investigate");

  const state = await store.read();
  assert.deepEqual(state.completed, ["fold13 summary found"]);
  assert.equal(state.decisions[0], "aggregate 11-13 first");
  assert.equal(state.next_action, "compute mean tau");
  const belief = state.beliefs.find((item) => item.id === "sh:fold13-weak");
  assert.equal(belief.status, "active");
  assert.equal(belief.established_rev, state.revision - 1);
  runtime.dispose();
});

test("prompt nudges once when the turn did work but declared nothing", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "enforce declaration" });
  let calls = 0;
  const prompts = [];
  let toolListener = null;
  const { runtime, session, listeners } = makeRuntime(store, async (text) => {
    calls += 1;
    prompts.push(text);
    if (calls === 1) {
      await toolListener({ type: "tool_execution_end", toolName: "bash", isError: false, result: "ls" });
      session.messages = [{ role: "assistant", content: [{ type: "text", text: "I looked around but found nothing worth declaring." }] }];
      listeners[0]({ type: "agent_end", messages: [] });
    } else {
      session.messages = [{ role: "assistant", content: [{ type: "text", text: "Sure.\n```anchor-state-delta\n" + JSON.stringify({ state_delta: { completed: ["declared after nudge"] } }) + "\n```" }] }];
    }
  });
  toolListener = listeners[0];
  await runtime.prompt("do work");

  assert.equal(calls, 2);
  assert.match(prompts[1], /did not include a cognition declaration/);
  const state = await store.read();
  assert.deepEqual(state.completed, ["declared after nudge"]);
  runtime.dispose();
});

test("invalid declaration payloads are rejected without corrupting State", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  const initial = await store.init({ goal: "reject bad deltas" });

  const { runtime, session, endTurn } = makeRuntime(store, () => {
    session.messages = [{ role: "assistant", content: [{ type: "text", text: "```anchor-state-delta\n{\"state_delta\":{\"completed\":\"not-an-array\"},\"belief_ops\":[]}\n```" }] }];
  });
  endTurn();
  await assert.rejects(() => runtime.prompt("bad output"), /must be an array of non-empty strings/);

  const state = await store.read();
  assert.equal(state.revision, initial.revision);
  assert.deepEqual(state.completed, []);
  runtime.dispose();
});

test("file changes are auto-captured as hashed evidence without model narration", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "auto evidence" });
  const artifactPath = join(dir, "artifact.txt");
  await writeFile(artifactPath, "deterministic content");

  const { runtime, session, listeners, endTurn } = makeRuntime(store, () => {
    session.messages = [{ role: "assistant", content: [{ type: "text", text: "```anchor-state-delta\n" + JSON.stringify({ learned: "fold13 needs a rerun before aggregation.", next_action: "rerun fold13" }) + "\n```" }] }];
  });

  // Model writes a file; the harness captures it - no model mention required.
  listeners[0]({ type: "tool_call", toolName: "write", input: { path: artifactPath } });
  endTurn();
  await runtime.prompt("write report");

  const state = await store.read();
  assert.deepEqual(state.decisions, ["fold13 needs a rerun before aggregation."]);
  assert.equal(state.next_action, "rerun fold13");
  const evidence = state.evidence.find((item) => item.path === artifactPath);
  assert.equal(evidence.type, "file_change");
  assert.match(evidence.sha256 ?? "", /^[a-f0-9]{64}$/);
  runtime.dispose();
});

test("normalizeDeclaration maps the slim schema onto state_delta fields", () => {
  const mapped = normalizeDeclaration({ learned: "L", blocked: "B", next_action: "N" });
  assert.deepEqual(mapped.state_delta.decisions, ["L"]);
  assert.deepEqual(mapped.state_delta.open_questions, ["B"]);
  assert.equal(mapped.state_delta.next_action, "N");
  assert.deepEqual(mapped.belief_ops, []);
  const full = normalizeDeclaration({ state_delta: { completed: ["x"] }, belief_ops: [{ op: "confirm", id: "a" }] });
  assert.deepEqual(full.state_delta.completed, ["x"]);
  assert.equal(full.belief_ops.length, 1);
});
