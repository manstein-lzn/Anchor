#!/usr/bin/env node
// Phase 5 / Exp "input economics + retention":
//   fixture: N generated files, each with a unique SENTINEL_FACT code
//   arms:    anchor (State context + bounded invocations) vs baseline (plain pi)
//   model:   identical stealth/ox-alpha-ctx128k-test in both arms
//   measure: per-model-call projected chars, transcript growth, sentinel recall
//
// usage: node scripts/experiments/run-econ-exp.mjs --arm anchor|baseline \
//          [--files 20] [--file-bytes 2500] [--tag smoke]

import { mkdir, writeFile, readdir } from "node:fs/promises";
import { createAgentSession } from "@earendil-works/pi-coding-agent";
import { AnchorRuntime } from "../../src/runtime.js";
import { createCodexRuntime } from "../../src/codex-config.js";

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const index = args.indexOf(name);
  return index >= 0 ? args[index + 1] : fallback;
};
const arm = opt("--arm", "anchor");
const files = parseInt(opt("--files", "12"), 10);
const fileBytes = parseInt(opt("--file-bytes", "2500"), 10);
const tag = opt("--tag", "run");
const maxSegments = parseInt(opt("--max-segments", "12"), 10);
const anchorMaxTurnChars = parseInt(opt("--anchor-max-turn-chars", "160000"), 10);
const CODEX_HOME = process.env.CODEX_HOME ?? "/root/.anchor-openrouter-test";
const WORK = process.env.ECON_EXP_WORK ?? "/tmp/econ-exp";

// ---------- fixtures ----------
const fixtureDir = `${WORK}/fixtures-${tag}-${arm}`;
await mkdir(fixtureDir, { recursive: true });
const codes = [];
for (let i = 0; i < files; i += 1) {
  const code = `ZQ-${String(i).padStart(3, "0")}-${Math.random().toString(36).slice(2, 8).toUpperCase()}`;
  codes.push(code);
  const filler = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod ".repeat(Math.ceil(fileBytes / 70)).slice(0, fileBytes);
  const content = `DOCUMENT ${i}\nSENTINEL_FACT: ${code}\n${filler}\nEND DOCUMENT ${i}\n`;
  await writeFile(`${fixtureDir}/doc_${String(i).padStart(3, "0")}.txt`, content);
}
const mid = Math.floor(files / 2);
const questions = [
  `Q1 (early): codes of documents 000, 001, 002 -> ${codes[0]}, ${codes[1]}, ${codes[2]}`,
  `Q2 (middle): codes of documents ${mid}, ${mid + 1} -> ${codes[mid]}, ${codes[mid + 1]}`,
  `Q3 (late): codes of documents ${files - 2}, ${files - 1} -> ${codes[files - 2]}, ${codes[files - 1]}`,
];

const taskPrompt = [
  `There are ${files} text files at ${fixtureDir}/doc_000.txt ... doc_${String(files - 1).padStart(3, "0")}.txt.`,
  "Read EVERY file with the read tool (all of them, no skipping).",
  "STRICT PROTOCOL: issue exactly ONE read tool call per assistant turn, in order (doc_000, doc_001, ...). Never batch multiple reads into one turn.",
  "After reading all files, output a section 'ANSWERS' containing exactly three lines:",
  ...questions.map((q) => `- ${q.split("->")[0].trim()} (write the codes you read)`),
  "Do not read anything else. Do not write files.",
].join("\n");

// ---------- telemetry ----------
function textLengthOf(item) {
  if (typeof item === "string") return item.length;
  if (Array.isArray(item)) return item.reduce((sum, child) => sum + textLengthOf(child), 0);
  if (!item || typeof item !== "object") return 0;
  if (typeof item.text === "string") return item.text.length;
  if (typeof item.output === "string") return item.output.length;
  if (typeof item.arguments === "string") return item.arguments.length;
  if (item.arguments && typeof item.arguments === "object") return JSON.stringify(item.arguments).length;
  if (Array.isArray(item.content)) return item.content.reduce((s, c) => s + textLengthOf(c), 0);
  return 0;
}
function estimateChars(messages) {
  return messages.reduce((sum, message) => {
    const content = message?.content;
    if (typeof content === "string") return sum + content.length;
    if (!Array.isArray(content)) return sum;
    // Tool results arrive nested as {type:"tool_result", content:[...]}; a
    // flat .text check would undercount them.
    return sum + content.reduce((s, item) => s + textLengthOf(item), 0);
  }, 0);
}
const calls = [];
const events = [];
function attachTelemetry(session) {
  const agent = session.agent;
  const original = agent.transformContext?.bind(agent);
  agent.transformContext = async (messages, signal) => {
    const projected = original ? await original(messages, signal) : messages;
    calls.push({ at: Date.now(), projectedChars: estimateChars(projected), projectedMessages: projected.length, inputMessages: messages.length, inputChars: estimateChars(messages) });
    return projected;
  };
  agent.subscribe?.((event) => {
    if (["compaction_start", "compaction_end", "agent_end"].includes(event.type)) {
      events.push({ type: event.type, reason: event.reason, aborted: event.aborted === true, at: Date.now() });
    }
  });
}

// ---------- arms ----------
// Each run gets an isolated cwd: pi auto-resumes the latest session per cwd,
// and a resumed transcript contaminates the experiment (verified in e4:
// the model blended fixtures from a previous run after a checkpoint).
const runCwd = `${WORK}/run-${tag}-${arm}`;
await mkdir(runCwd, { recursive: true });
let session;
let runtime;
if (arm === "anchor") {
  ({ runtime } = await AnchorRuntime.create({
    statePath: `${WORK}/state-${tag}-${arm}.json`,
    cwd: runCwd,
    codexHome: CODEX_HOME,
    disablePiCompaction: true,
    turnBudget: { maxTurnChars: anchorMaxTurnChars },
    goal: `Read all ${files} fixture docs in order and report their sentinel codes`,
    acceptance: [
      "All files doc_000..doc_N read via the read tool, one per turn, in order",
      "Final reply contains an ANSWERS section listing the exact SENTINEL_FACT codes for documents 000,001,002,mid,mid+1,last-2,last-1",
    ],
  }));
  session = runtime.session;
} else if (arm === "baseline") {
  const codex = await createCodexRuntime({ cwd: runCwd, codexHome: CODEX_HOME });
  const created = await createAgentSession({
    modelRuntime: codex.modelRuntime,
    settingsManager: codex.settingsManager,
    model: codex.model,
    thinkingLevel: codex.modelReasoningEffort,
  });
  session = created.session;
} else throw new Error(`unknown arm: ${arm}`);

attachTelemetry(session);
const started = Date.now();
let segments = null;
try {
  if (runtime) {
    // Multi-segment continuation is part of the architecture under test.
    segments = await runtime.runTask(taskPrompt, { maxSegments });
  } else {
    await session.prompt(taskPrompt);
  }
} finally {
  // ---------- report ----------
  const last = [...(session.messages ?? [])].reverse().find((m) => m?.role === "assistant" && Array.isArray(m.content) && m.content.some((c) => c?.type === "text" && c.text.trim()));
  const answerText = Array.isArray(last?.content) ? last.content.filter((c) => c?.type === "text").map((c) => c.text).join("\n") : "";
  const recall = codes.map((code, index) => ({ index, code, found: answerText.includes(code) }));
  const foundEarly = recall.slice(0, 3).filter((r) => r.found).length;
  const foundMid = recall.slice(mid, mid + 2).filter((r) => r.found).length;
  const foundLate = recall.slice(-2).filter((r) => r.found).length;
  const result = {
    arm, files, fileBytes, tag, segments,
    codexHome: CODEX_HOME,
    contextWindow: 128000,
    anchorMaxTurnChars: arm === "anchor" ? anchorMaxTurnChars : null,
    durationMs: Date.now() - started,
    modelCalls: calls.length,
    events,
    compactions: events.filter((event) => event.type === "compaction_end" && !event.aborted).length,
    projectedCharsFirst: calls[0]?.projectedChars ?? null,
    projectedCharsLast: calls.at(-1)?.projectedChars ?? null,
    projectedCharsMax: Math.max(0, ...calls.map((c) => c.projectedChars)),
    totalProjectedChars: calls.reduce((s, c) => s + c.projectedChars, 0),
    inputCharsLast: calls.at(-1)?.inputChars ?? null,
    recall: { early: `${foundEarly}/3`, mid: `${foundMid}/2`, late: `${foundLate}/2`, details: recall },
    answerTail: answerText.slice(-800),
    calls,
    assistantUsage: (session.messages ?? [])
      .filter((message) => message?.role === "assistant" && message.usage)
      .map((message) => ({ input: message.usage.input, output: message.usage.output, total: message.usage.totalTokens, stopReason: message.stopReason })),
  };
  const outPath = `${WORK}/result-${tag}-${arm}.json`;
  await writeFile(outPath, JSON.stringify(result, null, 2));
  console.log(`[exp] arm=${arm} calls=${calls.length} durationMin=${(result.durationMs / 60000).toFixed(1)}`);
  console.log(`[exp] projected chars first=${result.projectedCharsFirst} last=${result.projectedCharsLast} max=${result.projectedCharsMax}`);
  console.log(`[exp] recall early=${result.recall.early} mid=${result.recall.mid} late=${result.recall.late}`);
  console.log(`[exp] saved ${outPath}`);
  runtime ? runtime.dispose() : session.dispose?.();
}
