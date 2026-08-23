import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AnchorRuntime } from "../src/runtime.js";
import { StateStore } from "../src/state.js";

const bigMessage = (text, role = "assistant") => ({
  role,
  content: [{ type: "text", text }],
  timestamp: Date.now(),
});

const toolCallMessage = (id, argument = "") => ({
  role: "assistant",
  content: [{ type: "toolCall", id, name: "bash", arguments: { command: argument } }],
  timestamp: Date.now(),
});

const toolResultMessage = (id, text) => ({
  role: "toolResult",
  toolCallId: id,
  toolName: "bash",
  content: [{ type: "text", text }],
  timestamp: Date.now(),
});

function makeRuntime(store, turnBudget) {
  const listeners = [];
  const session = {
    agent: {
      transformContext: undefined,
      subscribe(listener) { listeners.push(listener); return () => {}; },
    },
    dispose() {},
  };
  const runtime = new AnchorRuntime({ session, store, purpose: "work", turnBudget });
  return { runtime, session, listeners };
}

test("Invocation budget replaces elided traffic with a deterministic digest", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "long task" });
  const { runtime, session, listeners } = makeRuntime(store, { maxTurnChars: 1000, tailChars: 400 });

  // Simulate tool observations within the current turn.
  for (let i = 0; i < 3; i += 1) {
    await listeners[0]({ type: "tool_execution_end", toolName: "bash", isError: false, result: `run ${i}` });
  }
  listeners[0]({ type: "tool_execution_end", toolName: "grep", isError: true, result: "no match" });

  const messages = [
    bigMessage("initial user request", "user"),
    ...Array.from({ length: 8 }, (_, i) => bigMessage(`tool result block ${i} `.padEnd(200, "x"))),
    bigMessage("most recent tool output"),
  ];
  const projected = await session.agent.transformContext(messages);

  // [envelope, checkpoint, ...tail]
  assert.equal(projected[0].role, "user");
  assert.match(projected[1].content[0].text, /\[anchor invocation checkpoint\]/);
  assert.match(projected[1].content[0].text, /- bash: run 2/);
  assert.match(projected[1].content[0].text, /- grep \[error\]: no match/);
  assert.equal(runtime.checkpointPending, true);
  assert.equal(runtime.lastCheckpointInfo.elided_messages > 0, true);
  // The tail keeps the most recent message verbatim.
  assert.equal(projected.at(-1).content[0].text, "most recent tool output");
  // Elided middle traffic is gone.
  assert.equal(projected.some((message) => message.content[0].text === "tool result block 0 xxx"), false);
  runtime.dispose();
});

test("Invocation budget keeps tool calls and results as one replay unit", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "paired replay" });
  const { runtime, session } = makeRuntime(store, { maxTurnChars: 500, tailChars: 400 });
  const callId = "call_keep|fc_keep";
  const messages = [
    bigMessage("initial task", "user"),
    bigMessage("x".repeat(600)),
    toolCallMessage(callId, "echo paired"),
    toolResultMessage(callId, "y".repeat(250)),
  ];
  const projected = await session.agent.transformContext(messages);
  assert.equal(projected.some((message) => message.role === "assistant" && message.content.some((item) => item.type === "toolCall" && item.id === callId)), true);
  assert.equal(projected.some((message) => message.role === "toolResult" && message.toolCallId === callId), true);
  runtime.dispose();
});

test("Invocation budget elides an oversized tool replay unit atomically", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "atomic elision" });
  const { runtime, session } = makeRuntime(store, { maxTurnChars: 500, tailChars: 200 });
  const callId = "call_drop|fc_drop";
  const messages = [
    bigMessage("initial task", "user"),
    bigMessage("x".repeat(600)),
    toolCallMessage(callId, "echo oversized"),
    toolResultMessage(callId, "y".repeat(800)),
  ];
  const projected = await session.agent.transformContext(messages);
  assert.equal(projected.some((message) => message.role === "assistant" && message.content.some((item) => item.type === "toolCall" && item.id === callId)), false);
  assert.equal(projected.some((message) => message.role === "toolResult" && message.toolCallId === callId), false);
  assert.match(projected[1].content[0].text, /invocation checkpoint/);
  assert.match(projected[1].content[0].text, /- bash: y+/);
  runtime.dispose();
});

test("A new user turn resets checkpoint state and budget accounting", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "two turns" });
  const { runtime, session } = makeRuntime(store, { maxTurnChars: 300, tailChars: 100 });

  const firstTurn = [bigMessage("turn one", "user"), bigMessage("x".repeat(600))];
  await session.agent.transformContext(firstTurn);
  assert.equal(runtime.checkpointPending, true);

  const secondTurn = [...firstTurn, bigMessage("turn two prompt", "user"), bigMessage("small reply")];
  const projected = await session.agent.transformContext(secondTurn);
  assert.equal(runtime.checkpointPending, false);
  assert.equal(projected.length, 3); // envelope + 2 current-turn messages
  runtime.dispose();
});

test("runTask continues across checkpointed invocations", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  const initial = await store.init({ goal: "multi segment" });
  await store.applyResult({ type: "state_delta", next_action: "analyze fold13 results" }, { expectedRevision: initial.revision });

  const { runtime, session } = makeRuntime(store, {});
  const prompts = [];
  let calls = 0;
  session.prompt = async (text) => {
    calls += 1;
    prompts.push(text);
    if (calls === 1) runtime.lastTurnCheckpointed = true; // simulate agent_end after checkpoint
  };
  const outcome = await runtime.runTask("do the work", { maxSegments: 5 });

  assert.deepEqual(outcome, { segments: 2, checkpointed: false });
  assert.equal(calls, 2);
  assert.equal(prompts[0], "do the work");
  assert.match(prompts[1], /\[anchor\] Your previous invocation reached its working-set checkpoint/);
  assert.match(prompts[1], /analyze fold13 results/);
  runtime.dispose();
});
