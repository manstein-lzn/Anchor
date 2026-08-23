#!/usr/bin/env node
// Phase 5 compact comparison using the existing ox-alpha@128k model.
// The experiment deliberately uses a few large read results instead of
// hundreds of small files, then asks both arms to recover facts after the
// context boundary has been crossed.

import { mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { createAgentSession, SessionManager } from "@earendil-works/pi-coding-agent";
import { AnchorRuntime } from "../../src/runtime.js";
import { createCodexRuntime } from "../../src/codex-config.js";

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const index = args.indexOf(name);
  return index >= 0 ? args[index + 1] : fallback;
};

const arm = opt("--arm", "anchor");
const tag = opt("--tag", "compact");
const chunks = Number.parseInt(opt("--chunks", "8"), 10);
const chunkBytes = Number.parseInt(opt("--chunk-bytes", "56000"), 10);
const maxSegments = Number.parseInt(opt("--max-segments", "12"), 10);
const contextWindow = Number.parseInt(opt("--context-window", process.env.COMPACT_CONTEXT_WINDOW ?? "65536"), 10);
const anchorMaxTurnChars = Number.parseInt(opt("--anchor-max-turn-chars", "45000"), 10);
const work = process.env.ECON_EXP_WORK ?? "/tmp/econ-exp-compact";
const codexHome = process.env.CODEX_HOME ?? "/root/.anchor-openrouter-test";
const runCwd = `${work}/run-${tag}-${arm}`;
const fixtureDir = `${work}/fixtures-${tag}`;
const statePath = `${work}/state-${tag}-${arm}.json`;

if (!Number.isInteger(chunks) || chunks < 4) throw new Error("--chunks must be >= 4");
if (!Number.isInteger(chunkBytes) || chunkBytes < 1000) throw new Error("--chunk-bytes must be >= 1000");

await mkdir(runCwd, { recursive: true });
await mkdir(fixtureDir, { recursive: true });

// Stable fixture content lets Anchor and baseline use the same facts without
// regenerating different sentinel codes between arms.
const facts = [];
const filler = "ANCHOR_COMPACT_PRESSURE lorem ipsum 0123456789 ".repeat(Math.ceil(chunkBytes / 48)).slice(0, chunkBytes);
for (let index = 0; index < chunks; index += 1) {
  const code = `CP-${String(index).padStart(3, "0")}-${(index * 7919 + 104729).toString(36).toUpperCase().padStart(6, "0")}`;
  facts.push(code);
  const content = [
    `CHUNK ${index}`,
    `SENTINEL_FACT: ${code}`,
    `EARLY_FACT: experiment chunk ${index} establishes durable fact ${code}`,
    filler,
    `LATE_FACT: preserve exact code ${code}`,
    `END CHUNK ${index}`,
    "",
  ].join("\n");
  await writeFile(`${fixtureDir}/chunk_${String(index).padStart(3, "0")}.txt`, content);
}

const recoveryIndices = [0, 1, Math.floor(chunks / 2), chunks - 2, chunks - 1];
const recoveryQuestion = recoveryIndices
  .map((index) => `chunk_${String(index).padStart(3, "0")}: exact SENTINEL_FACT code`)
  .join("\n");
const taskPrompt = [
  `There are ${chunks} large text files in ${fixtureDir}.`,
  "Read every chunk with the read tool, exactly one chunk per assistant turn, in ascending order.",
  "During the ingestion phase, do not summarize or answer questions; continue reading until all chunks are read.",
  "After the final chunk, emit the required anchor-state-delta declaration if you are in Anchor, then stop.",
].join("\n");
const recoveryPrompt = [
  "The ingestion phase is over and no more tools are allowed.",
  "Recover the exact sentinel codes from the durable context available to you.",
  "Output exactly an ANSWERS section with one line per requested chunk.",
  recoveryQuestion,
  "Do not guess. If a code is unavailable, write UNKNOWN.",
].join("\n");

const calls = [];
const events = [];
const originalFetch = globalThis.fetch;
let requestCount = 0;
let requestChars = 0;
const requestSizes = [];
globalThis.fetch = async (url, init = {}) => {
  if (typeof url === "string" && url.includes("openrouter.ai/api/v1/responses") && init?.body) {
    try {
      const body = JSON.parse(init.body);
      const size = Buffer.byteLength(JSON.stringify(body), "utf8");
      requestCount += 1;
      requestChars += size;
      requestSizes.push({ request: requestCount, chars: size, inputItems: Array.isArray(body.input) ? body.input.length : 0 });
    } catch {
      // Keep the experiment running if an upstream request is not JSON.
    }
  }
  return originalFetch(url, init);
};

function valueChars(value) {
  if (typeof value === "string") return value.length;
  if (Array.isArray(value)) return value.reduce((sum, item) => sum + valueChars(item), 0);
  if (!value || typeof value !== "object") return 0;
  if (typeof value.text === "string") return value.text.length;
  if (typeof value.output === "string") return value.output.length;
  if (typeof value.arguments === "string") return value.arguments.length;
  if (value.arguments && typeof value.arguments === "object") return JSON.stringify(value.arguments).length;
  if (Array.isArray(value.content)) return value.content.reduce((sum, item) => sum + valueChars(item), 0);
  return 0;
}
function messageChars(message) {
  return valueChars(message?.content);
}
function attachTelemetry(session) {
  const original = session.agent.transformContext?.bind(session.agent);
  session.agent.transformContext = async (messages, signal) => {
    const projected = original ? await original(messages, signal) : messages;
    calls.push({
      at: Date.now(),
      projectedChars: projected.reduce((sum, message) => sum + messageChars(message), 0),
      inputChars: messages.reduce((sum, message) => sum + messageChars(message), 0),
      projectedMessages: projected.length,
      inputMessages: messages.length,
    });
    return projected;
  };
  const listen = session.subscribe?.bind(session) ?? session.agent.subscribe?.bind(session.agent);
  listen?.((event) => {
    if (["compaction_start", "compaction_end", "agent_end"].includes(event.type)) {
      events.push({ type: event.type, reason: event.reason, aborted: event.aborted === true, at: Date.now() });
    }
  });
}

let session;
let runtime;
if (arm === "anchor") {
  ({ runtime } = await AnchorRuntime.create({
    statePath,
    cwd: runCwd,
    codexHome,
    disablePiCompaction: true,
    turnBudget: { maxTurnChars: anchorMaxTurnChars },
    goal: `Ingest ${chunks} large chunks and recover exact sentinel codes after the working context boundary`,
    acceptance: [
      "All chunks are read in ascending order",
      "Recovery answer preserves the exact requested sentinel codes",
    ],
  }));
  session = runtime.session;
} else if (arm === "baseline") {
  const codex = await createCodexRuntime({ cwd: runCwd, codexHome });
  const created = await createAgentSession({
    cwd: runCwd,
    sessionManager: SessionManager.create(runCwd),
    modelRuntime: codex.modelRuntime,
    settingsManager: codex.settingsManager,
    model: codex.model,
    thinkingLevel: codex.modelReasoningEffort,
  });
  session = created.session;
} else {
  throw new Error(`unknown arm: ${arm}`);
}

attachTelemetry(session);
const started = Date.now();
let ingestion;
let recovery;
try {
  if (runtime) ingestion = await runtime.runTask(taskPrompt, { maxSegments });
  else await session.prompt(taskPrompt);
  await session.waitForIdle?.();
  // Pi threshold compact is asynchronous after agent_end. Give it a short
  // settle window before the equivalent recovery question.
  await new Promise((resolve) => setTimeout(resolve, 500));
  recovery = await session.prompt(recoveryPrompt);
  await session.waitForIdle?.();
} finally {
  const messages = session.messages ?? [];
  const assistantText = messages
    .filter((message) => message?.role === "assistant")
    .flatMap((message) => Array.isArray(message.content) ? message.content : [])
    .filter((item) => item?.type === "text")
    .map((item) => item.text)
    .join("\n");
  const recall = facts.map((code, index) => ({ index, code, found: assistantText.includes(code) }));
  const state = runtime ? await runtime.state() : null;
  const result = {
    arm,
    tag,
    chunks,
    chunkBytes,
    contextWindow,
    codexHome,
    ingestion,
    recovery: recovery ? { stopReason: recovery.stopReason } : null,
    durationMs: Date.now() - started,
    modelCalls: calls.length,
    compactions: events.filter((event) => event.type === "compaction_end" && !event.aborted).length,
    events,
    recall: {
      requested: recoveryIndices,
      found: recall.filter((item) => recoveryIndices.includes(item.index) && item.found).length,
      total: recoveryIndices.length,
      details: recall,
    },
    state: state ? {
      revision: state.revision,
      decisions: state.decisions,
      next_action: state.next_action,
      beliefs: state.beliefs,
    } : null,
    requestTelemetry: {
      count: requestCount,
      totalChars: requestChars,
      sizes: requestSizes,
    },
    calls,
    assistantUsage: messages
      .filter((message) => message?.role === "assistant" && message.usage)
      .map((message) => ({ input: message.usage.input, output: message.usage.output, total: message.usage.totalTokens, stopReason: message.stopReason })),
    answerTail: assistantText.slice(-4000),
  };
  await writeFile(`${work}/result-${tag}-${arm}.json`, JSON.stringify(result, null, 2));
  console.log(`[compact-exp] arm=${arm} chunks=${chunks} calls=${calls.length} compacted=${result.compactions} recall=${result.recall.found}/${result.recall.total} durationMin=${(result.durationMs / 60000).toFixed(1)}`);
  console.log(`[compact-exp] requestCount=${requestCount} requestChars=${requestChars}`);
  console.log(`[compact-exp] saved ${work}/result-${tag}-${arm}.json`);
  runtime ? runtime.dispose() : session.dispose?.();
}
