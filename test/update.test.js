import test from "node:test";
import assert from "node:assert/strict";
import { BOOTSTRAP_PROPOSAL_TOOL, BOOTSTRAP_SUBMISSION_TOOL, compactFrontier, hashValue, normalizeCognition, runBootstrap, runUpdate, UPDATE_PROPOSAL_TOOL, UPDATE_SUBMISSION_TOOL, UPDATE_SYSTEM } from "../src/update.js";

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

const item = (id, statement) => ({ id, statement, sources: ["episode:bounded"], relevance: "changes the next action" });

const updateSubmission = () => ({
  schema: "anchor.cognition.v3",
  situation: {
    current_understanding: "The provider boundary is stable.",
    confirmed_facts: [item("fact-provider", "Tool replay is grouped.")],
    active_hypotheses: [], unresolved_conflicts: [], blockers: [],
  },
  experience: { decisions: [item("decision-boundary", "Use Pi's compaction boundary.")], failed_paths: [] },
  intent: {
    current_directive: "Finish the bounded Update path.",
    accepted_next_action: "Run the focused test.",
    next_plan: ["Run the focused test."],
    open_questions: [],
  },
  knowledge_index: [],
  transition_certificate: { schema: "anchor.transition.v1", dispositions: [] },
});

const proposalSubmission = (ids = [
  `confirmed_facts-${hashValue("Tool replay is grouped.").slice(-12)}`,
  `decisions-${hashValue("Use Pi's compaction boundary.").slice(-12)}`,
  `failed_paths-${hashValue("Full branch replay overflowed because it grew without bound.").slice(-12)}`,
]) => ({
  schema: "anchor.update-proposal.v2",
  situation: { current_understanding: "The provider boundary is stable." },
  intent: { current_directive: "Finish the bounded Update path.", accepted_next_action: "Run the focused test.", next_plan: ["Run the focused test."] },
  item_decisions: ids.map((item_id) => ({ item_id, disposition: "carry" })),
  new_items: [],
  knowledge_index: [],
});

const bootstrapSubmission = () => ({
  schema: "anchor.bootstrap.v1",
  title: "Ship adapter",
  contract: { schema: "anchor.contract.v1", status: "provisional", goal: "Ship adapter", rationale: [], acceptance_criteria: [], constraints: ["Do not change the API."], non_goals: [], risks: [], verification_commands: [], allowed_paths: [], execution_plan: "Not established." },
  cognition: {
    schema: "anchor.cognition.v3",
    situation: { current_understanding: "The adapter must ship without an API change.", confirmed_facts: [], active_hypotheses: [], unresolved_conflicts: [], blockers: [] },
    experience: { decisions: [], failed_paths: [] },
    intent: { current_directive: "Ship the adapter.", accepted_next_action: "Inspect the adapter.", next_plan: ["Inspect the adapter."], open_questions: [] },
    knowledge_index: [],
  },
});
const bootstrapProposalSubmission = () => ({
  schema: "anchor.bootstrap-proposal.v2",
  title: "Ship adapter",
  goal: "Ship adapter",
  uncertainties: ["Acceptance criteria are not yet confirmed."],
  intent: { current_directive: "Ship the adapter.", accepted_next_action: "Inspect the adapter.", next_plan: ["Inspect the adapter."], open_questions: [] },
  new_items: [],
});

const submissionResponse = (name, arguments_, usage = { input: 10, output: 2 }) => ({
  content: [{ type: "toolCall", id: "call-1", name, arguments: arguments_ }],
  usage,
  stopReason: "toolUse",
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

test("Update output accepts a preamble before one complete JSON object", () => {
  const result = normalizeCognition(`The requested state follows.\n${JSON.stringify(cognition())}`);

  assert.equal(result.schema, "anchor.cognition.v2");
  assert.equal(result.current_directive, "Finish the bounded Update path.");
});

test("Update output repairs raw control characters inside JSON strings", () => {
  const malformed = JSON.stringify(cognition({ current_understanding: "Line one\nLine two." })).replace("Line one\\nLine two.", "Line one\nLine two.");
  const result = normalizeCognition(malformed);

  assert.equal(result.current_understanding, "Line one\nLine two.");
});

test("Update output rejects multiple top-level JSON objects", () => {
  const raw = `${JSON.stringify(cognition())}\n${JSON.stringify(cognition())}`;
  assert.throws(() => normalizeCognition(raw), /multiple top-level JSON objects/);
});

test("content_hash is empty or a strict sha256 digest", () => {
  const reference = { id: "ref-1", cue: "Evidence", locator: "artifact:1", source: "episode:1" };
  const validHash = `sha256:${"a".repeat(64)}`;
  assert.doesNotThrow(() => normalizeCognition(JSON.stringify({ ...updateSubmission(), knowledge_index: [{ ...reference, content_hash: validHash }] })));
  assert.throws(() => normalizeCognition(JSON.stringify({ ...updateSubmission(), knowledge_index: [{ ...reference, content_hash: "model invented this" }] })), /content_hash must be a sha256 digest/);
  assert.equal(UPDATE_SUBMISSION_TOOL.parameters.properties.knowledge_index.items.properties.content_hash.anyOf.length, 2);
});

test("Bootstrap rejects an invalid content hash before persistence", async () => {
  const preparation = {
    firstKeptEntryId: "entry-kept",
    tokensBefore: 10,
    messagesToSummarize: [{ role: "user", content: [{ type: "text", text: "bootstrap" }] }],
    turnPrefixMessages: [],
    isSplitTurn: false,
  };
  const candidate = bootstrapSubmission();
  candidate.cognition.knowledge_index = [{ id: "ref-1", cue: "Evidence", locator: "artifact:1", content_hash: "not-a-digest", source: "episode:1" }];
  let persisted = false;
  await assert.rejects(runBootstrap({
    bootstrap: async () => { persisted = true; },
  }, { preparation, signal: new AbortController().signal }, {
    model: { provider: "test", id: "model" },
    sessionManager: { getSessionId: () => "session-bootstrap" },
    modelRegistry: { complete: async () => submissionResponse(BOOTSTRAP_SUBMISSION_TOOL.name, candidate) },
  }), (error) => {
    assert.equal(error.bootstrapStage, "response-validation");
    assert.match(error.message, /arguments.*schema/);
    return true;
  });
  assert.equal(persisted, false);
});

test("incomplete Update output is rejected instead of becoming State", () => {
  assert.throws(() => normalizeCognition(JSON.stringify(cognition({ current_directive: "" }))), /current_directive/);
  assert.throws(() => normalizeCognition(JSON.stringify(cognition({ next_plan: [] }))), /next_plan/);
  assert.throws(() => normalizeCognition("not json"), (error) => {
    assert.match(error.message, /invalid JSON/);
    assert.doesNotMatch(error.message, /not json/);
    return true;
  });
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
  let modelOptions;
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
      complete: async (_model, request, options) => {
        modelRequest = request;
        modelOptions = options;
        return submissionResponse(UPDATE_PROPOSAL_TOOL.name, proposalSubmission());
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
  assert.match(modelRequest.messages[0].content[0].text, /previous_active_item_ids/);
  assert.equal(modelRequest.messages.at(-1).content[0].text, "bounded work");
  assert.match(modelRequest.systemPrompt, /Submit exactly once through anchor_submit_update/i);
  assert.deepEqual(modelRequest.tools, [UPDATE_PROPOSAL_TOOL]);
  assert.equal(modelRequest.tools[0].constrainedSampling.strict, "required");
  assert.equal(modelRequest.tools[0].parameters.properties.schema.enum[0], "anchor.update-proposal.v2");
  assert.equal(modelOptions.toolChoice, "required");
  assert.equal(modelOptions.signal, event.signal);
  assert.equal(result.compaction.summary, "Anchor Checkpoint 1 committed for task task-1.");
  assert.equal(result.compaction.firstKeptEntryId, "entry-kept");
  assert.deepEqual(result.compaction.usage, { input: 10, output: 2 });
  assert.equal(result.compaction.details.schema, "anchor.compact-receipt.v1");
  assert.deepEqual(result.compaction.details.frontier, expectedFrontier);
});

test("semantic Update rejection gets one deterministic correction attempt", async () => {
  const preparation = {
    firstKeptEntryId: "entry-kept",
    tokensBefore: 123,
    messagesToSummarize: [{ role: "user", content: [{ type: "text", text: "bounded work" }] }],
    turnPrefixMessages: [],
    isSplitTurn: false,
  };
  const frontier = compactFrontier(preparation, "session-retry");
  const prior = updateSubmission();
  prior.situation.confirmed_facts = [item("old-fact", "The prior fact remains active.")];
  prior.experience.decisions = [];
  const previous = checkpoint(0, { kind: "planning", session_id: "session-retry", source_hash: hashValue({ proposal: 1 }) });
  previous.cognition = prior;
  const invalid = proposalSubmission([]);
  const corrected = proposalSubmission(["old-fact"]);
  const requests = [];
  let writes = 0;
  const result = await runUpdate({
    recovery: async () => ({ task_id: "task-retry", task: { state_version: 1 }, contract: { content: { goal: "test" } }, checkpoint: previous }),
    update: async (value) => { writes += 1; return { event_id: "event-1", content_hash: hashValue(value), payload: { schema: "anchor.checkpoint.v1", task_id: "task-retry", checkpoint_version: 1, parent: { checkpoint_version: 0, content_hash: previous.receipt.content_hash }, ...value } }; },
  }, { preparation, signal: new AbortController().signal }, {
    model: { provider: "test", id: "model" },
    sessionManager: { getSessionId: () => "session-retry" },
    modelRegistry: { complete: async (_model, request) => { requests.push(request); return submissionResponse(UPDATE_PROPOSAL_TOOL.name, requests.length === 1 ? invalid : corrected); } },
  });
  assert.equal(requests.length, 2);
  assert.match(requests[1].messages.at(-1).content[0].text, /omits old-fact/);
  assert.equal(writes, 1);
  assert.match(result.compaction.summary, /committed/);
});

test("Update submission uses one closed schema instead of free-text JSON", () => {
  assert.equal(UPDATE_SUBMISSION_TOOL.name, "anchor_submit_update");
  assert.equal(UPDATE_SUBMISSION_TOOL.parameters.additionalProperties, false);
  assert.deepEqual(UPDATE_SUBMISSION_TOOL.parameters.required, ["schema", "situation", "experience", "intent", "knowledge_index", "transition_certificate"]);
  assert.equal(UPDATE_SUBMISSION_TOOL.parameters.properties.situation.additionalProperties, false);
  assert.equal(UPDATE_SUBMISSION_TOOL.parameters.properties.transition_certificate.properties.dispositions.items.properties.disposition.enum.length, 6);
  assert.doesNotMatch(UPDATE_SYSTEM, /Return JSON|top-level JSON object/i);
});

test("Update rejects schema-invalid and duplicate submissions before State writes", async () => {
  const preparation = {
    firstKeptEntryId: "entry-kept",
    tokensBefore: 123,
    messagesToSummarize: [{ role: "user", content: [{ type: "text", text: "bounded work" }] }],
    turnPrefixMessages: [],
    isSplitTurn: false,
  };
  const previous = checkpoint(0, { kind: "planning", session_id: "session-1", source_hash: hashValue({ proposal: 1 }) });
  const base = {
    recovery: async () => ({ task_id: "task-1", task: { state_version: 7 }, contract: { content: { goal: "test" } }, checkpoint: previous }),
    update: async () => assert.fail("invalid submission must not write State"),
  };
  const invalidResponses = [
    submissionResponse(UPDATE_SUBMISSION_TOOL.name, { ...updateSubmission(), unmodeled_secret: "DO_NOT_ECHO" }),
    { content: [
      { type: "toolCall", id: "call-1", name: UPDATE_SUBMISSION_TOOL.name, arguments: updateSubmission() },
      { type: "toolCall", id: "call-2", name: UPDATE_SUBMISSION_TOOL.name, arguments: updateSubmission() },
    ], stopReason: "toolUse" },
  ];

  for (const response of invalidResponses) {
    await assert.rejects(runUpdate(base, { preparation, signal: new AbortController().signal }, {
      model: { provider: "test", id: "model" },
      sessionManager: { getSessionId: () => "session-1" },
      modelRegistry: { complete: async () => response },
    }), (error) => {
      assert.match(error.message, /response rejected/);
      assert.doesNotMatch(error.message, /DO_NOT_ECHO/);
      return true;
    });
  }
});

test("free-text Update output reports bounded diagnostics without echoing model text", async () => {
  const preparation = {
    firstKeptEntryId: "entry-kept",
    tokensBefore: 123,
    messagesToSummarize: [{ role: "user", content: [{ type: "text", text: "bounded work" }] }],
    turnPrefixMessages: [],
    isSplitTurn: false,
  };
  const previous = checkpoint(0, { kind: "planning", session_id: "session-1", source_hash: hashValue({ proposal: 1 }) });
  const secretOutput = '{"schema":"anchor.cognition.v3"}\nSECRET_RESPONSE_TEXT';
  await assert.rejects(runUpdate({
    recovery: async () => ({ task_id: "task-1", task: { state_version: 7, title: "test" }, contract: { content: { goal: "test" } }, checkpoint: previous }),
    update: async () => assert.fail("invalid output must not write State"),
  }, { preparation, signal: new AbortController().signal }, {
    model: { provider: "test", id: "model" },
    sessionManager: { getSessionId: () => "session-1" },
    modelRegistry: { complete: async () => ({ content: [{ type: "text", text: secretOutput }], stopReason: "length" }) },
  }), (error) => {
    assert.match(error.message, /Update Agent response rejected/);
    assert.match(error.message, /must submit exactly one anchor_submit_update function call/);
    assert.match(error.message, /stop_reason=length/);
    assert.match(error.message, new RegExp(`text_chars=${secretOutput.length}`));
    assert.match(error.message, /text_hash=sha256:[a-f0-9]{64}/);
    assert.doesNotMatch(error.message, /SECRET_RESPONSE_TEXT/);
    return true;
  });
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

test("first compact bootstraps provisional state from only the Episode", async () => {
  const preparation = {
    firstKeptEntryId: "entry-kept",
    tokensBefore: 321,
    messagesToSummarize: [{ role: "user", content: [{ type: "text", text: "Ship the adapter without changing the API." }] }],
    turnPrefixMessages: [],
    isSplitTurn: false,
  };
  const episode = preparation.messagesToSummarize;
  const frontier = compactFrontier(preparation, "session-bootstrap");
  let request;
  let bootstrapped;
  const anchor = {
    async bootstrap(value) {
      bootstrapped = value;
      return { task_id: "task-bootstrap", checkpoint: { checkpoint_version: 0, ...value.checkpoint, receipt: { event_id: "event-0", content_hash: hashValue(value.checkpoint) } } };
    },
  };
  const result = await runBootstrap(anchor, { preparation, signal: new AbortController().signal }, {
    model: { provider: "test", id: "model" },
    sessionManager: { getSessionId: () => "session-bootstrap" },
    modelRegistry: { complete: async (_model, value, options) => { request = value; assert.equal(options.toolChoice, "required"); return submissionResponse(BOOTSTRAP_PROPOSAL_TOOL.name, bootstrapProposalSubmission(), { input: 4, output: 4 }); } },
  });
  assert.equal(result.compaction.details.checkpoint_version, 0);
  assert.equal(bootstrapped.contract.status, "provisional");
  assert.equal(JSON.stringify(request.messages).includes("Ship the adapter"), true);
  assert.equal(JSON.stringify(request.messages).includes("full branch"), false);
  assert.equal(JSON.stringify(request.messages).includes("entry-kept"), false);
  assert.deepEqual(request.tools, [BOOTSTRAP_PROPOSAL_TOOL]);
  assert.equal(request.tools[0].constrainedSampling.strict, "required");
  assert.equal(JSON.stringify(episode).includes("Ship the adapter"), true);
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
