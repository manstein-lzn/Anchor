import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { compileContext } from "../src/context.js";
import { AnchorRuntime } from "../src/runtime.js";
import { StateStore, createState } from "../src/state.js";

test("StateStore requires an explicit persistence path", () => {
  assert.throws(() => new StateStore(), /path is required/);
});

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

test("AnchorRuntime adopts the first user prompt as a new session goal", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-"));
  const store = new StateStore(join(dir, "state.json"));
  await store.init({ goal: "Awaiting first user task" });
  const listeners = [];
  const session = {
    messages: [],
    agent: {
      transformContext: undefined,
      subscribe(listener) { listeners.push(listener); return () => {}; },
    },
    prompt: async (text) => { session.messages.push({ role: "user", content: [{ type: "text", text }] }); },
    dispose() {},
  };
  const runtime = new AnchorRuntime({ session, store });
  await runtime.prompt("research the last two weeks");
  assert.equal((await store.read()).task.goal, "research the last two weeks");
  runtime.dispose();
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

test("AnchorRuntime follows Pi session identity without writing project State", async () => {
  const projectDir = await mkdtemp(join(tmpdir(), "anchor-project-"));
  const stateRoot = await mkdtemp(join(tmpdir(), "anchor-session-state-"));
  const firstStore = new StateStore(join(stateRoot, "session-one", "state.json"));
  const secondStore = new StateStore(join(stateRoot, "session-two", "state.json"));
  await firstStore.init({ goal: "first task" });
  await secondStore.init({ goal: "resumed research" });
  const makeSession = (id) => ({
    sessionManager: { getSessionId: () => id, getHeader: () => ({ type: "session", id }) },
    agent: { transformContext: undefined, subscribe() { return () => {}; } },
    settingsManager: { applyOverrides() {} },
    dispose() {},
  });
  const first = makeSession("session-one");
  const second = makeSession("session-two");
  let rebind;
  const host = {
    session: first,
    cwd: projectDir,
    setRebindSession(callback) { rebind = callback; },
    dispose() {},
  };
  const runtime = new AnchorRuntime({ runtimeHost: host, store: firstStore, sessionStateRoot: stateRoot });
  host.session = second;
  await rebind();
  const projected = await second.agent.transformContext([{ role: "user", content: [{ type: "text", text: "resume" }] }]);
  assert.match(projected[0].content[0].text, /resumed research/);
  assert.equal(await new StateStore(join(projectDir, ".anchor/state.json")).exists(), false);
  runtime.dispose();
});

test("AnchorRuntime initializes a legacy Pi session goal without project writes", async () => {
  const projectDir = await mkdtemp(join(tmpdir(), "anchor-legacy-project-"));
  const stateRoot = await mkdtemp(join(tmpdir(), "anchor-legacy-state-"));
  const bootstrapStore = new StateStore(join(stateRoot, "bootstrap", "state.json"));
  await bootstrapStore.init({ goal: "bootstrap" });
  const makeSession = (id, messages = []) => ({
    sessionManager: {
      getSessionId: () => id,
      getHeader: () => ({ type: "session", id }),
      buildSessionContext: () => ({ messages }),
    },
    agent: { transformContext: undefined, subscribe() { return () => {}; } },
    settingsManager: { applyOverrides() {} },
    dispose() {},
  });
  let rebind;
  const host = {
    session: makeSession("bootstrap"),
    cwd: projectDir,
    setRebindSession(callback) { rebind = callback; },
    dispose() {},
  };
  const runtime = new AnchorRuntime({ runtimeHost: host, store: bootstrapStore, sessionStateRoot: stateRoot });
  host.session = makeSession("legacy-session", [
    { role: "user", content: [{ type: "text", text: "research the last two weeks" }] },
    { role: "assistant", content: [{ type: "text", text: "working" }] },
  ]);
  await rebind();
  assert.equal((await runtime.state()).task.goal, "research the last two weeks");
  assert.equal(await new StateStore(join(projectDir, ".anchor/state.json")).exists(), false);
  runtime.dispose();
});

test("AnchorRuntime forks inherit State from the Pi parent session", async () => {
  const dir = await mkdtemp(join(tmpdir(), "anchor-fork-"));
  const stateRoot = join(dir, "states");
  const parentId = "parent-session";
  const childId = "child-session";
  const parentSessionFile = join(dir, "parent.jsonl");
  await writeFile(parentSessionFile, `${JSON.stringify({ type: "session", id: parentId, cwd: dir })}\n`);
  const parentStore = new StateStore(join(stateRoot, parentId, "state.json"));
  await parentStore.init({ goal: "parent task", acceptance: ["report exists"] });
  await parentStore.applyResult({ type: "state_delta", decisions: ["parent finding"] }, { expectedRevision: 0 });
  const bootstrapStore = new StateStore(join(stateRoot, "bootstrap", "state.json"));
  await bootstrapStore.init({ goal: "bootstrap" });
  const makeSession = (id, parentSession) => ({
    sessionManager: {
      getSessionId: () => id,
      getHeader: () => ({ type: "session", id, parentSession }),
      buildSessionContext: () => ({ messages: [] }),
    },
    agent: { transformContext: undefined, subscribe() { return () => {}; } },
    settingsManager: { applyOverrides() {} },
    dispose() {},
  });
  let rebind;
  const host = {
    session: makeSession("bootstrap"),
    cwd: dir,
    setRebindSession(callback) { rebind = callback; },
    dispose() {},
  };
  const runtime = new AnchorRuntime({ runtimeHost: host, store: bootstrapStore, sessionStateRoot: stateRoot });
  host.session = makeSession(childId, parentSessionFile);
  await rebind();
  const state = await runtime.state();
  assert.equal(state.task.goal, "parent task");
  assert.deepEqual(state.task.acceptance, ["report exists"]);
  assert.deepEqual(state.decisions, ["parent finding"]);
  assert.equal(runtime.store.path, join(stateRoot, childId, "state.json"));
  runtime.dispose();
});

test("AnchorRuntime keeps an explicit State path fixed across session replacement", async () => {
  const firstDir = await mkdtemp(join(tmpdir(), "anchor-first-"));
  const secondDir = await mkdtemp(join(tmpdir(), "anchor-second-"));
  const fixedPath = join(firstDir, "shared-state.json");
  const store = new StateStore(fixedPath);
  await store.init({ goal: "fixed task" });
  const makeSession = () => ({
    agent: { transformContext: undefined, subscribe() { return () => {}; } },
    settingsManager: { applyOverrides() {} },
    dispose() {},
  });
  const first = makeSession();
  const second = makeSession();
  let rebind;
  const host = {
    session: first,
    cwd: firstDir,
    setRebindSession(callback) { rebind = callback; },
    dispose() {},
  };
  const runtime = new AnchorRuntime({ runtimeHost: host, store });
  host.session = second;
  host.cwd = secondDir;
  await rebind();
  const projected = await second.agent.transformContext([{ role: "user", content: [{ type: "text", text: "resume" }] }]);
  assert.match(projected[0].content[0].text, /fixed task/);
  assert.equal(runtime.store.path, fixedPath);
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
