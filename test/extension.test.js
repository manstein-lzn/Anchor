import test from "node:test";
import assert from "node:assert/strict";
import { access, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import anchorExtension, { proposalCheckpoint, proposalContract } from "../src/extension.js";
import { AnchorClient } from "../src/store.js";
import { hashValue } from "../src/update.js";

test("Normal Pi is zero-intrusion and can enter or cancel Planning", async (t) => {
  const { agentDir, restore } = await isolatedAgentDir();
  t.after(restore);
  const pi = fakePi();
  anchorExtension(pi.api);
  const ctx = fakeContext(process.cwd(), pi, "session-normal", {
    select: (_title, options) => options[1],
  });

  await pi.handlers.session_start({ reason: "startup" }, ctx);
  assert.deepEqual(pi.activeTools(), ["read", "bash", "edit", "write"]);
  assert.equal(await pi.handlers.before_agent_start({ systemPrompt: "Pi", prompt: "work" }, ctx), undefined);
  assert.equal(await pi.handlers.context({ messages: [] }, ctx), undefined);
  assert.equal(await pi.handlers.session_before_compact({}, ctx), undefined);
  await assert.rejects(access(join(agentDir, "anchor", "sessions", "session-normal", "anchor.db")));

  await pi.commands.anchor.handler("start", ctx);
  assert.deepEqual(pi.activeTools(), ["read", "grep", "find", "ls", "anchor_ask", "anchor_propose"]);
  assert.match((await pi.handlers.before_agent_start({ systemPrompt: "Pi", prompt: "plan" }, ctx)).systemPrompt, /requirements discovery/);

  await pi.commands.anchor.handler("cancel", ctx);
  assert.deepEqual(pi.activeTools(), ["read", "bash", "edit", "write"]);
  assert.equal(pi.entries.filter((entry) => entry.customType === "anchor.mode").at(-1).data.mode, "normal");
});

test("first compact lazily creates Anchor without Planning", async (t) => {
  const { agentDir, restore } = await isolatedAgentDir();
  t.after(restore);
  const pi = fakePi();
  anchorExtension(pi.api);
  const ctx = fakeContext(process.cwd(), pi, "session-lazy", {
    model: { provider: "test", id: "model" },
    modelRegistry: { complete: async () => ({ content: [{ type: "toolCall", id: "bootstrap-call", name: "anchor_submit_bootstrap", arguments: {
      schema: "anchor.bootstrap.v1",
      title: "Lazy task",
      contract: { schema: "anchor.contract.v1", status: "provisional", goal: "Ship the adapter", rationale: [], acceptance_criteria: [], constraints: [], non_goals: [], risks: [], verification_commands: [], allowed_paths: [], execution_plan: "Not established." },
      cognition: { schema: "anchor.cognition.v3", situation: { current_understanding: "The task has started.", confirmed_facts: [], active_hypotheses: [], unresolved_conflicts: [], blockers: [] }, experience: { decisions: [], failed_paths: [] }, intent: { current_directive: "Ship the adapter.", accepted_next_action: "Inspect the adapter.", next_plan: ["Inspect the adapter."], open_questions: [] }, knowledge_index: [] },
    } }], usage: { input: 1, output: 1 } }) },
  });
  await pi.handlers.session_start({ reason: "startup" }, ctx);
  const result = await pi.handlers.session_before_compact({
    preparation: { firstKeptEntryId: "entry-kept", messagesToSummarize: [{ role: "user", content: [{ type: "text", text: "Ship the adapter" }] }], turnPrefixMessages: [], isSplitTurn: false, tokensBefore: 100 },
    signal: new AbortController().signal,
  }, ctx);
  assert.equal(result.compaction.details.checkpoint_version, 0);
  assert.equal(pi.entries.filter((entry) => entry.customType === "anchor.mode").at(-1).data.mode, "active");
  await access(join(agentDir, "anchor", "sessions", "session-lazy", "anchor.db"));

  const resumed = fakePi(pi.entries);
  anchorExtension(resumed.api);
  let resumedContext;
  const preparation = { firstKeptEntryId: "entry-kept", messagesToSummarize: [{ role: "user", content: [{ type: "text", text: "Ship the adapter" }] }], turnPrefixMessages: [], isSplitTurn: false, tokensBefore: 100 };
  resumedContext = fakeContext(process.cwd(), resumed, "session-lazy", {
    compact: async ({ onComplete }) => {
      const replay = await resumed.handlers.session_before_compact({ preparation, signal: new AbortController().signal }, resumedContext);
      resumed.appendCompaction(replay.compaction.details);
      onComplete({});
    },
  });
  await resumed.handlers.session_start({ reason: "resume" }, resumedContext);
  assert.equal(resumed.entries.filter((entry) => entry.type === "compaction").length, 1);
});

test("failed lazy bootstrap falls back to native compact without Anchor State", async (t) => {
  const { agentDir, restore } = await isolatedAgentDir();
  t.after(restore);
  const pi = fakePi();
  anchorExtension(pi.api);
  const ctx = fakeContext(process.cwd(), pi, "session-bootstrap-failure", {
    model: { provider: "test", id: "model" },
    modelRegistry: { complete: async () => { throw new Error("provider unavailable token=secret"); } },
  });
  await pi.handlers.session_start({ reason: "startup" }, ctx);
  const preparation = {
    firstKeptEntryId: "entry-kept",
    messagesToSummarize: [{ role: "user", content: [{ type: "text", text: "work" }] }],
    turnPrefixMessages: [],
    isSplitTurn: false,
    tokensBefore: 99,
  };
  assert.equal(await pi.handlers.session_before_compact({
    preparation,
    signal: new AbortController().signal,
  }, ctx), undefined);
  const failure = pi.entries.find((entry) => entry.customType === "anchor.bootstrap-failure");
  assert.deepEqual(failure.data, {
    schema: "anchor.bootstrap-failure.v1",
    session_id: "session-bootstrap-failure",
    stage: "model-transport",
    error_class: "Error",
    error_message: "provider unavailable token=[redacted]",
    model: "test/model",
    message_count: 1,
    messages_to_summarize: 1,
    turn_prefix_messages: 0,
    tokens_before: 99,
    is_split_turn: false,
    episode_hash: hashValue(preparation.messagesToSummarize),
    frontier_hash: hashValue({
      kind: "compact",
      session_id: "session-bootstrap-failure",
      first_kept_entry_id: "entry-kept",
      episode_hash: hashValue(preparation.messagesToSummarize),
      is_split_turn: false,
    }),
  });
  assert.match(ctx.notifications.at(-1).message, /model-transport/);
  assert.match(ctx.notifications.at(-1).message, /native compaction/);
  await assert.rejects(access(join(agentDir, "anchor", "sessions", "session-bootstrap-failure", "anchor.db")));
});

test("proposal seals after the complete turn, then creates one session Anchor", async (t) => {
  const { agentDir, restore } = await isolatedAgentDir();
  t.after(restore);
  const pi = fakePi([], { anchor: true });
  anchorExtension(pi.api);
  const ctx = fakeContext(process.cwd(), pi, "session-active", {
    select: (title, options) => title.startsWith("How should") ? options[0] : options[0],
  });

  await pi.handlers.session_start({ reason: "startup" }, ctx);
  const asked = await pi.tools.anchor_ask.execute("ask-1", { question: "Choose", options: ["A", "B"] }, undefined, undefined, ctx);
  assert.match(asked.content[0].text, /User answer: A/);

  const candidate = await pi.tools.anchor_propose.execute("call-1", proposal(), undefined, undefined, ctx);
  const statePath = join(agentDir, "anchor", "sessions", "session-active", "anchor.db");
  assert.equal(candidate.details.status, "candidate");
  await assert.rejects(access(statePath));
  assert.equal(pi.sent.length, 0);

  await pi.handlers.agent_settled({}, ctx);
  assert.equal(pi.entries.filter((entry) => entry.customType === "anchor.proposal").at(-1).data.status, "sealed");
  assert.equal(pi.entries.filter((entry) => entry.customType === "anchor.mode").at(-1).data.mode, "active");
  assert.deepEqual(pi.activeTools(), ["read", "bash", "edit", "write", "anchor_recall"]);
  assert.equal(pi.sent.length, 1);
  await access(statePath);
  const recalled = await pi.tools.anchor_recall.execute("recall-1", { locator: "checkpoint:0:item:fact-1" }, undefined, undefined, ctx);
  assert.match(recalled.content[0].text, /The user approved the scope/);
  await pi.commands.anchor.handler("recall checkpoint:0:item:fact-1", ctx);
  assert.match(ctx.notifications.at(-1).message, /The user approved the scope/);

  const projected = await pi.handlers.context({ messages: [{ role: "user", content: [{ type: "text", text: "execute" }] }] }, ctx);
  assert.match(projected.messages[0].content[0].text, /Confirmed requirements are ready/);

  const client = new AnchorClient({ workspace: process.cwd(), statePath, sessionId: "session-active" });
  const beforeUpdate = await client.recovery();
  const frontier = {
    kind: "compact",
    session_id: "session-active",
    first_kept_entry_id: "entry-kept",
    episode_hash: hashValue([{ role: "user", content: "work" }]),
    is_split_turn: false,
  };
  const committed = await client.update({
    schema: "anchor.checkpoint-candidate.v1",
    frontier,
    cognition: { ...beforeUpdate.checkpoint.cognition, current_understanding: "Recovered after the compact commit." },
    provenance: { kind: "compact", model: "test/model" },
  }, beforeUpdate.task.state_version);

  const resumed = fakePi(pi.entries);
  anchorExtension(resumed.api);
  const resumedContext = fakeContext(process.cwd(), resumed, "session-active", {
    compact: ({ onComplete }) => {
      resumed.appendCompaction({
        schema: "anchor.compact-receipt.v1",
        task_id: committed.payload.task_id,
        checkpoint_version: committed.payload.checkpoint_version,
        checkpoint_hash: committed.content_hash,
        event_id: committed.event_id,
        frontier,
      });
      onComplete({});
    },
  });
  await resumed.handlers.session_start({ reason: "resume" }, resumedContext);
  assert.equal(resumed.entries.filter((entry) => entry.type === "compaction").length, 1);
  assert.deepEqual(resumed.activeTools(), ["read", "bash", "edit", "write", "anchor_recall"]);
  assert.match((await resumed.handlers.context({ messages: [] }, resumedContext)).messages[0].content[0].text, /Recovered after the compact commit/);

  const forked = fakePi(pi.entries);
  anchorExtension(forked.api);
  const forkContext = fakeContext(process.cwd(), forked, "session-fork", { select: (_title, options) => options[1] });
  await forked.handlers.session_start({ reason: "fork" }, forkContext);
  assert.deepEqual(forked.activeTools(), ["read", "bash", "edit", "write"]);
  assert.equal(await forked.handlers.context({ messages: [] }, forkContext), undefined);
});

test("unsealed, revised, stale, and cancelled proposals never create State", async (t) => {
  const { agentDir, restore } = await isolatedAgentDir();
  t.after(restore);
  const pi = fakePi([], { anchor: true });
  anchorExtension(pi.api);
  const ctx = fakeContext(process.cwd(), pi, "session-revise", {
    select: (title, options) => title.startsWith("Review") ? options[1] : options[0],
  });
  const statePath = join(agentDir, "anchor", "sessions", "session-revise", "anchor.db");

  await pi.handlers.session_start({ reason: "startup" }, ctx);
  await pi.tools.anchor_propose.execute("call-1", proposal(), undefined, undefined, ctx);
  await pi.handlers.agent_settled({}, ctx);
  assert.equal(pi.entries.filter((entry) => entry.customType === "anchor.proposal").at(-1).data.status, "revising");
  await assert.rejects(access(statePath));

  const noUi = fakeContext(process.cwd(), pi, "session-revise", { hasUI: false });
  await pi.tools.anchor_propose.execute("call-2", proposal(), undefined, undefined, noUi);
  await pi.handlers.agent_settled({}, noUi);
  await pi.handlers.before_agent_start({ systemPrompt: "Pi", prompt: "change the goal" }, noUi);
  assert.equal(pi.entries.filter((entry) => entry.customType === "anchor.proposal").at(-1).data.status, "stale");
  await pi.commands.anchor.handler("cancel", ctx);
  await assert.rejects(access(statePath));
});

test("Planning blocks writes and refuses a proposal that shares its model turn", async (t) => {
  const { restore } = await isolatedAgentDir();
  t.after(restore);
  const pi = fakePi([], { anchor: true });
  anchorExtension(pi.api);
  const ctx = fakeContext(process.cwd(), pi, "session-batch", { hasUI: false });
  await pi.handlers.session_start({ reason: "startup" }, ctx);

  assert.equal((await pi.handlers.tool_call({ toolName: "edit", toolCallId: "edit-1" }, ctx)).block, true);
  assert.equal(await pi.handlers.tool_call({ toolName: "read", toolCallId: "read-1" }, ctx), undefined);

  await pi.handlers.turn_start({}, ctx);
  await pi.handlers.tool_execution_start({ toolCallId: "ask-1", toolName: "anchor_ask" }, ctx);
  await pi.handlers.tool_execution_start({ toolCallId: "proposal-1", toolName: "anchor_propose" }, ctx);
  await assert.rejects(
    () => pi.tools.anchor_propose.execute("proposal-1", proposal(), undefined, undefined, ctx),
    /only tool call/,
  );

  await pi.handlers.turn_start({}, ctx);
  await pi.handlers.tool_execution_start({ toolCallId: "proposal-2", toolName: "anchor_propose" }, ctx);
  await pi.tools.anchor_propose.execute("proposal-2", proposal(), undefined, undefined, ctx);
  await pi.handlers.tool_execution_start({ toolCallId: "read-2", toolName: "read" }, ctx);
  await pi.handlers.agent_settled({}, ctx);
  assert.equal(pi.entries.filter((entry) => entry.customType === "anchor.proposal").at(-1).data.status, "stale");
});

test("empty or foreign State files do not trap a new session in blocked mode", async (t) => {
  const { agentDir, restore } = await isolatedAgentDir();
  t.after(restore);
  const sessionId = "session-empty";
  const emptyPath = join(agentDir, "anchor", "sessions", sessionId, "anchor.db");
  const item = proposal();
  const proposalId = hashValue({ session_id: sessionId, proposal: item });
  const emptyClient = new AnchorClient({ workspace: process.cwd(), statePath: emptyPath });
  await assert.rejects(() => emptyClient.begin({
    sessionId,
    proposalHash: proposalId,
    title: item.title,
    contract: proposalContract(item),
    checkpoint: {},
  }), /checkpoint candidate schema/);
  await access(emptyPath);

  const planningEntry = {
    type: "custom",
    customType: "anchor.mode",
    data: { schema: "anchor.session-mode.v1", session_id: sessionId, mode: "planning" },
  };
  const planning = fakePi([planningEntry]);
  anchorExtension(planning.api);
  const planningContext = fakeContext(process.cwd(), planning, sessionId, { hasUI: false });
  await planning.handlers.session_start({ reason: "resume" }, planningContext);
  assert.deepEqual(planning.activeTools(), ["read", "grep", "find", "ls", "anchor_ask", "anchor_propose"]);

  const sharedPath = join(agentDir, "shared.db");
  const owner = "session-owner";
  const ownerProposalId = hashValue({ session_id: owner, proposal: item });
  await new AnchorClient({ workspace: process.cwd(), statePath: sharedPath }).begin({
    sessionId: owner,
    proposalHash: ownerProposalId,
    title: item.title,
    contract: proposalContract(item),
    checkpoint: proposalCheckpoint(item, owner, ownerProposalId),
  });
  const foreign = fakePi([], { "anchor-state": sharedPath });
  anchorExtension(foreign.api);
  const foreignContext = fakeContext(process.cwd(), foreign, "session-new", { select: (_title, options) => options[1] });
  await foreign.handlers.session_start({ reason: "startup" }, foreignContext);
  assert.deepEqual(foreign.activeTools(), ["read", "bash", "edit", "write"]);
  assert.match(foreignContext.notifications[0].message, /belongs to another Pi session/);
  assert.equal(Object.hasOwn(foreign.entries.filter((entry) => entry.customType === "anchor.mode").at(-1).data, "state_path"), false);
});

test("Active recovery fails closed when its Task is missing", async (t) => {
  const { agentDir, restore } = await isolatedAgentDir();
  t.after(restore);
  const sessionId = "session-missing";
  const statePath = join(agentDir, "anchor", "sessions", sessionId, "anchor.db");
  const client = new AnchorClient({ workspace: process.cwd(), statePath });
  await assert.rejects(() => client.begin({
    sessionId,
    proposalHash: hashValue("invalid-proposal"),
    title: "Invalid task",
    contract: proposalContract(proposal()),
    checkpoint: {},
  }), /checkpoint candidate schema/);

  const pi = fakePi([{
    type: "custom",
    customType: "anchor.mode",
    data: { schema: "anchor.session-mode.v1", session_id: sessionId, mode: "active" },
  }]);
  anchorExtension(pi.api);
  const ctx = fakeContext(process.cwd(), pi, sessionId, { hasUI: false });
  await pi.handlers.session_start({ reason: "resume" }, ctx);

  assert.match(ctx.notifications.at(-1).message, /Anchor Task not found/);
  assert.deepEqual(pi.activeTools(), ["read", "grep", "find", "ls"]);
  assert.match((await pi.handlers.before_agent_start({ systemPrompt: "Pi" }, ctx)).systemPrompt, /unavailable Anchor/);
});

test("recovery receipts are branch-local and stale recovery frontiers are rejected", async (t) => {
  const { agentDir, restore } = await isolatedAgentDir();
  t.after(restore);
  const sessionId = "session-frontier";
  const statePath = join(agentDir, "anchor", "sessions", sessionId, "anchor.db");
  const item = proposal();
  const proposalId = hashValue({ session_id: sessionId, proposal: item });
  const client = new AnchorClient({ workspace: process.cwd(), statePath });
  const created = await client.begin({
    sessionId,
    proposalHash: proposalId,
    title: item.title,
    contract: proposalContract(item),
    checkpoint: proposalCheckpoint(item, sessionId, proposalId),
  });
  client.taskId = created.task.task_id;
  const episode = [{ role: "user", content: "covered work" }];
  const frontier = {
    kind: "compact",
    session_id: sessionId,
    first_kept_entry_id: "expected-kept",
    episode_hash: hashValue(episode),
    is_split_turn: false,
  };
  const committed = await client.update({
    schema: "anchor.checkpoint-candidate.v1",
    frontier,
    cognition: created.checkpoint.cognition,
    provenance: { kind: "compact", model: "test/model" },
  }, created.task.state_version);
  const modeEntry = {
    type: "custom",
    customType: "anchor.mode",
    data: { schema: "anchor.session-mode.v1", session_id: sessionId, mode: "active" },
  };
  const siblingReceipt = {
    type: "compaction",
    details: {
      schema: "anchor.compact-receipt.v1",
      task_id: committed.payload.task_id,
      checkpoint_version: committed.payload.checkpoint_version,
      checkpoint_hash: committed.content_hash,
      event_id: committed.event_id,
      frontier,
    },
  };
  const pi = fakePi([modeEntry, siblingReceipt]);
  anchorExtension(pi.api);
  const branch = [pi.entries[0], {
    ...structuredClone(pi.entries[1]),
    details: {
      ...structuredClone(pi.entries[1].details),
      event_id: "tampered-event",
      frontier: { ...frontier, episode_hash: hashValue("tampered-episode") },
    },
  }];
  let compactCalled = false;
  let ctx;
  ctx = fakeContext(process.cwd(), pi, sessionId, {
    branch,
    compact: async ({ onError }) => {
      compactCalled = true;
      const result = await pi.handlers.session_before_compact({
        preparation: {
          messagesToSummarize: episode,
          turnPrefixMessages: [],
          firstKeptEntryId: "newer-kept",
        },
        signal: new AbortController().signal,
      }, ctx);
      if (result?.cancel) onError(new Error("compaction cancelled"));
    },
  });

  await pi.handlers.session_start({ reason: "resume" }, ctx);
  assert.equal(compactCalled, true);
  assert.equal(ctx.notifications.some(({ message }) => /frontier changed/.test(message)), true);
  assert.match(ctx.notifications.at(-1).message, /recovery failed/);
  assert.equal((await client.recovery()).checkpoint.checkpoint_version, 1);
});

function fakePi(initialEntries = [], flags = {}) {
  let sequence = 0;
  let leafId = null;
  const entries = initialEntries.map((entry) => {
    const copy = structuredClone(entry);
    copy.id ??= `entry-${++sequence}`;
    copy.parentId ??= leafId;
    leafId = copy.id;
    return copy;
  });
  sequence = entries.length;
  const handlers = {};
  const tools = {};
  const commands = {};
  const sent = [];
  let active = ["read", "bash", "edit", "write"];
  const available = ["read", "bash", "edit", "write", "grep", "find", "ls"];
  const api = {
    registerFlag() {},
    getFlag(name) { return flags[name]; },
    registerTool(tool) { tools[tool.name] = tool; active.push(tool.name); },
    registerCommand(name, command) { commands[name] = command; },
    on(event, handler) { handlers[event] = handler; },
    getActiveTools() { return [...active]; },
    getAllTools() { return [...new Set([...available, ...Object.keys(tools)])].map((name) => ({ name })); },
    setActiveTools(names) { active = [...names]; },
    appendEntry(customType, data) {
      const entry = { id: `entry-${++sequence}`, parentId: leafId, type: "custom", customType, data };
      entries.push(entry);
      leafId = entry.id;
    },
    setSessionName() {},
    sendMessage(message, options) { sent.push({ message, options }); },
  };
  return {
    api,
    handlers,
    tools,
    commands,
    entries,
    sent,
    activeTools: () => active,
    leafId: () => leafId,
    appendCompaction(details) {
      const entry = { id: `entry-${++sequence}`, parentId: leafId, type: "compaction", details };
      entries.push(entry);
      leafId = entry.id;
    },
  };
}

function fakeContext(cwd, pi, sessionId, options = {}) {
  const notifications = [];
  return {
    cwd,
    hasUI: options.hasUI ?? true,
    model: options.model,
    modelRegistry: options.modelRegistry,
    ui: {
      select: async (title, values) => options.select ? options.select(title, values) : undefined,
      input: async (title) => options.input ? options.input(title) : undefined,
      confirm: async () => false,
      notify(message, type) { notifications.push({ message, type }); },
      setStatus() {},
    },
    sessionManager: {
      getEntries: () => pi.entries,
      getBranch: () => options.branch ?? pi.entries,
      getLeafId: () => pi.leafId(),
      getSessionId: () => sessionId,
    },
    compact: options.compact ?? (({ onError }) => onError?.(new Error("Unexpected compact"))),
    notifications,
  };
}

async function isolatedAgentDir() {
  const agentDir = await mkdtemp(join(tmpdir(), "anchor-agent-"));
  const previous = process.env.PI_CODING_AGENT_DIR;
  process.env.PI_CODING_AGENT_DIR = agentDir;
  return {
    agentDir,
    restore() {
      if (previous === undefined) delete process.env.PI_CODING_AGENT_DIR;
      else process.env.PI_CODING_AGENT_DIR = previous;
    },
  };
}

function proposal() {
  return {
    title: "Verified task",
    goal: "Produce a verified result.",
    rationale: ["The result must survive long execution."],
    acceptance_criteria: ["The focused test passes."],
    constraints: ["Keep changes scoped."],
    non_goals: ["Do not redesign unrelated code."],
    verification_commands: ["true"],
    allowed_paths: ["README.md"],
    risks: ["A stale proposal could create the wrong Task."],
    execution_plan: "Inspect, implement, and verify.",
    current_understanding: "Confirmed requirements are ready for execution.",
    confirmed_facts: ["The user approved the scope."],
    active_hypotheses: [],
    decisions: ["Use the focused verification command."],
    blockers: [],
    open_questions: [],
    next_plan: ["Inspect the current implementation."],
    evidence_refs: [],
  };
}
