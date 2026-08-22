import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AnchorRuntime, parseStateDelta } from "../src/runtime.js";
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

function makeRuntime(store) {
  const listeners = [];
  const session = {
    messages: [],
    agent: {
      transformContext: undefined,
      subscribe(listener) { listeners.push(listener); return () => {}; },
    },
    dispose() {},
  };
  const runtime = new AnchorRuntime({ session, store, purpose: "work", turnBudget: { maxTurnChars: 1_000_000 } });
  return { runtime, session, listeners };
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
  const { runtime, session } = makeRuntime(store);

  session.prompt = async () => {
    session.messages = [{ role: "assistant", content: [{ type: "text", text: DECLARATION }] }];
  };
  await runtime.prompt("investigate");

  const state = await store.read();
  assert.deepEqual(state.completed, ["fold13 summary found"]);
  assert.equal(state.decisions[0], "aggregate 11-13 first");
  assert.equal(state.next_action, "compute mean tau");
  const belief = state.beliefs.find((item) => item.id === "sh:fold13-weak");
  assert.equal(belief.status, "active");
  // The belief was established by its own commit; the following delta commit
  // advanced the revision once more.
  assert.equal(belief.established_rev, state.revision - 1);
  runtime.dispose();
});

test("prompt nudges once when the turn did work but declared nothing", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "enforce declaration" });
  const { runtime, session, listeners } = makeRuntime(store);

  let calls = 0;
  const prompts = [];
  session.prompt = async (text) => {
    calls += 1;
    prompts.push(text);
    if (calls === 1) {
      // Simulate tool activity this turn so the nudge condition holds.
      await listeners[0]({ type: "tool_execution_end", toolName: "bash", isError: false, result: "ls" });
      session.messages = [{ role: "assistant", content: [{ type: "text", text: "I looked around but found nothing worth declaring." }] }];
    } else {
      // After the nudge, the model declares.
      session.messages = [{ role: "assistant", content: [{ type: "text", text: "Sure.\n```anchor-state-delta\n" + JSON.stringify({ state_delta: { completed: ["declared after nudge"] } }) + "\n```" }] }];
    }
  };
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

  const { runtime, session } = makeRuntime(store);
  session.prompt = async () => {
    session.messages = [{ role: "assistant", content: [{ type: "text", text: "```anchor-state-delta\n{\"state_delta\":{\"completed\":\"not-an-array\"},\"belief_ops\":[]}\n```" }] }];
  };
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
  const { runtime, session, listeners } = makeRuntime(store);

  const artifactPath = join(dir, "artifact.txt");
  // Model writes a file and declares ONLY interpretation (slim schema).
  listeners[0]({ type: "tool_call", toolName: "write", input: { path: artifactPath } });
  const { writeFile } = await import("node:fs/promises");
  await writeFile(artifactPath, "deterministic content");

  session.prompt = async () => {
    session.messages = [{ role: "assistant", content: [{ type: "text", text: "```anchor-state-delta\n" + JSON.stringify({ learned: "fold13 needs a rerun before aggregation.", next_action: "rerun fold13" }) + "\n```" }] }];
  };
  await runtime.prompt("write report");

  const state = await store.read();
  assert.deepEqual(state.decisions, ["fold13 needs a rerun before aggregation."]);
  assert.equal(state.next_action, "rerun fold13");
  const evidence = state.evidence.find((item) => item.path === artifactPath);
  assert.equal(evidence.type, "file_change");
  assert.match(evidence.sha256 ?? "", /^[a-f0-9]{64}$/);
  runtime.dispose();
});

test("normalizeDeclaration maps the slim schema onto state_delta fields", async () => {
  const { normalizeDeclaration } = await import("../src/runtime.js");
  const mapped = normalizeDeclaration({ learned: "L", blocked: "B", next_action: "N" });
  assert.deepEqual(mapped.state_delta.decisions, ["L"]);
  assert.deepEqual(mapped.state_delta.open_questions, ["B"]);
  assert.equal(mapped.state_delta.next_action, "N");
  assert.deepEqual(mapped.belief_ops, []);
  const full = normalizeDeclaration({ state_delta: { completed: ["x"] }, belief_ops: [{ op: "confirm", id: "a" }] });
  assert.deepEqual(full.state_delta.completed, ["x"]);
  assert.equal(full.belief_ops.length, 1);
});
