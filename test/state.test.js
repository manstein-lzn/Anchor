import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { compileContext } from "../src/context.js";
import { AnchorRuntime } from "../src/runtime.js";
import { StateStore, createState } from "../src/state.js";

test("StateStore persists revisions and deterministic normalization", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  const initial = await store.init({ goal: "ship Anchor", acceptance: ["tests pass"] });
  const next = await store.applyResult({ type: "state_delta", completed: ["tests pass"], next_action: "review" }, { expectedRevision: initial.revision });
  assert.equal(next.revision, 1);
  assert.deepEqual(next.completed, ["tests pass"]);
  await assert.rejects(() => store.applyResult({ type: "state_delta", failures: ["stale"] }, { expectedRevision: 0 }), /stale state/);
  assert.match(await readFile(join(dir, "state.events.jsonl"), "utf8"), /state.result_applied/);
});

test("Observations are audit-only and do not mutate State", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "observe tools" });
  await Promise.all([
    store.recordObservation({ kind: "tool", tool_name: "read", summary: "a" }),
    store.recordObservation({ kind: "tool", tool_name: "bash", summary: "b" }),
  ]);
  const state = await store.read();
  assert.deepEqual(state.completed, []);
  assert.equal(state.revision, 0);
  const events = await readFile(join(dir, "state.events.jsonl"), "utf8");
  assert.match(events, /observation.recorded/);
  const recorded = events.trim().split("\n").filter((line) => line.includes("observation.recorded"));
  assert.equal(recorded.length, 2);
});

test("ContextEngine projects state without transcript history", () => {
  const context = compileContext(createState({ goal: "recover task", acceptance: ["artifact exists"] }), { purpose: "resume" });
  assert.equal(context.goal, "recover task");
  assert.equal(context.purpose, "resume");
  assert.deepEqual(context.acceptance, ["artifact exists"]);
  assert.equal(Object.hasOwn(context, "messages"), false);
});

test("AnchorRuntime replaces only the model-facing context", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "resume work" });
  const listeners = [];
  const session = {
    agent: {
      transformContext: undefined,
      subscribe(listener) { listeners.push(listener); return () => listeners.splice(listeners.indexOf(listener), 1); },
    },
    dispose() {},
  };
  const runtime = new AnchorRuntime({ session, store, purpose: "resume" });
  const projected = await session.agent.transformContext([
    { role: "user", content: [{ type: "text", text: "continue" }] },
  ]);
  assert.equal(projected[0].role, "user");
  assert.match(projected[0].content[0].text, /resume work/);
  assert.equal(projected[1].content[0].text, "continue");
  assert.equal(runtime.metrics.compilations, 1);
  assert.equal(listeners.length, 1);
  runtime.dispose();
  assert.equal(listeners.length, 0);
});

test("AnchorRuntime keeps overflow compact as an explicit fallback", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "overflow recovery" });
  let compactCalls = 0;
  const listeners = [];
  const session = {
    settingsManager: { applyOverrides() {} },
    agent: {
      transformContext: undefined,
      subscribe(listener) { listeners.push(listener); return () => {}; },
    },
    compact: async () => { compactCalls += 1; },
    prompt: async () => {},
    dispose() {},
  };
  const runtime = new AnchorRuntime({ session, store });
  await listeners[0]({ type: "agent_end", messages: [{ role: "assistant", stopReason: "length", content: [] }] });
  await runtime.prompt("continue");
  assert.equal(compactCalls, 1);
  runtime.dispose();
});

test("AnchorRuntime rebinds State Context when Pi replaces a session", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "switch sessions" });
  const makeSession = () => ({
    agent: {
      transformContext: undefined,
      subscribe() { return () => {}; },
    },
    settingsManager: { applyOverrides() {} },
    dispose() {},
  });
  const first = makeSession();
  const second = makeSession();
  let rebind;
  const host = {
    session: first,
    setRebindSession(callback) { rebind = callback; },
    dispose() {},
  };
  const runtime = new AnchorRuntime({ runtimeHost: host, store });
  runtime.setRebindSession(() => {});
  host.session = second;
  await rebind();
  const projected = await second.agent.transformContext([{ role: "user", content: [{ type: "text", text: "continue" }] }]);
  assert.match(projected[0].content[0].text, /switch sessions/);
  runtime.dispose();
});
