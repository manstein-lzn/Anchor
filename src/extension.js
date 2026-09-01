import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { getAgentDir } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { projectMessages } from "./context.js";
import { AnchorClient } from "./store.js";
import { ANCHOR_UPDATE_PROTOCOL, compactFrontier, hashValue, runBootstrap, runUpdate } from "./update.js";

const MODE = "anchor.mode";
const PROPOSAL = "anchor.proposal";
const BOOTSTRAP_FAILURE = "anchor.bootstrap-failure";
const ANCHOR_TOOLS = new Set(["anchor_ask", "anchor_propose", "anchor_recall"]);
const RECALL_TOOL = "anchor_recall";
const PLANNING_TOOLS = ["read", "grep", "find", "ls", "anchor_ask", "anchor_propose"];
const ACCEPT = "Accept and create Anchor";
const REVISE = "Revise through discussion";
const CANCEL = "Cancel Anchor Planning";

const PLANNING_PROMPT = `You are initializing the durable Anchor for this Pi session.

This is requirements discovery, not implementation. Work with the user until you
have a precise model of the goal, motivation, current baseline, acceptance
criteria, constraints, non-goals, risks, evidence, verification, and execution
plan. Ask one focused question at a time when the answer materially changes the
Task. Inspect read-only project evidence instead of asking the user for facts you
can discover. Do not edit files or begin the work.

Call anchor_propose only when a fresh agent could execute without guessing any
critical decision. The proposal becomes reviewable only after the complete
proposing turn is stored, and it becomes authoritative only after explicit user
acceptance.`;

const proposalSchema = Type.Object({
  title: Type.String({ minLength: 1 }),
  goal: Type.String({ minLength: 1 }),
  rationale: Type.Array(Type.String({ minLength: 1 }), { minItems: 1 }),
  acceptance_criteria: Type.Array(Type.String({ minLength: 1 }), { minItems: 1 }),
  constraints: Type.Array(Type.String({ minLength: 1 })),
  non_goals: Type.Array(Type.String({ minLength: 1 })),
  verification_commands: Type.Array(Type.String({ minLength: 1 })),
  allowed_paths: Type.Array(Type.String({ minLength: 1 })),
  risks: Type.Array(Type.String({ minLength: 1 })),
  execution_plan: Type.String({ minLength: 1 }),
  current_understanding: Type.String({ minLength: 1 }),
  confirmed_facts: Type.Array(Type.String({ minLength: 1 })),
  active_hypotheses: Type.Array(Type.String({ minLength: 1 })),
  decisions: Type.Array(Type.String({ minLength: 1 })),
  blockers: Type.Array(Type.String({ minLength: 1 })),
  open_questions: Type.Array(Type.String({ minLength: 1 })),
  next_plan: Type.Array(Type.String({ minLength: 1 }), { minItems: 1 }),
  evidence_refs: Type.Array(Type.String({ minLength: 1 })),
});

export default function anchorExtension(pi) {
  const localCodex = registerLocalCodexProvider(pi);
  let mode = "normal";
  let anchor = null;
  let statePath = null;
  let healthError = null;
  let pendingProposal = null;
  let toolsBeforePlanning = null;
  let turnToolCalls = new Set();
  let expectedRecoveryFrontier = null;

  pi.registerFlag("anchor", {
    description: "Start this Pi session in Anchor Planning",
    type: "boolean",
  });
  pi.registerFlag("anchor-state", {
    description: "Use an explicit Anchor State database",
    type: "string",
  });

  function defaultStatePath(sessionId) {
    return join(getAgentDir(), "anchor", "sessions", sessionId, "anchor.db");
  }

  function normalTools() {
    return pi.getActiveTools().filter((name) => !ANCHOR_TOOLS.has(name));
  }

  function setNormalTools() {
    pi.setActiveTools(toolsBeforePlanning ?? normalTools());
    toolsBeforePlanning = null;
  }

  function setActiveTools() {
    const available = new Set(pi.getAllTools().map((tool) => tool.name));
    const base = toolsBeforePlanning ?? normalTools();
    toolsBeforePlanning = null;
    pi.setActiveTools([...base, ...(available.has(RECALL_TOOL) ? [RECALL_TOOL] : [])]);
  }

  function setPlanningTools(includeAnchorTools = true) {
    if (!toolsBeforePlanning) toolsBeforePlanning = normalTools();
    const available = new Set(pi.getAllTools().map((tool) => tool.name));
    pi.setActiveTools(PLANNING_TOOLS.filter((name) => available.has(name) && (includeAnchorTools || !ANCHOR_TOOLS.has(name))));
  }

  function persistMode(nextMode, ctx) {
    const sessionId = ctx.sessionManager.getSessionId();
    const data = { schema: "anchor.session-mode.v1", session_id: sessionId, mode: nextMode };
    if (statePath !== defaultStatePath(sessionId)) data.state_path = statePath;
    pi.appendEntry(MODE, data);
  }

  function enterNormal(ctx, { persist = true } = {}) {
    mode = "normal";
    anchor = null;
    healthError = null;
    pendingProposal = null;
    setNormalTools();
    ctx.ui.setStatus("anchor", undefined);
    if (persist) persistMode(mode, ctx);
  }

  function enterPlanning(ctx, { persist = true } = {}) {
    mode = "planning";
    anchor = null;
    healthError = null;
    pendingProposal = null;
    setPlanningTools();
    ctx.ui.setStatus("anchor", "Anchor: planning");
    if (persist) persistMode(mode, ctx);
  }

  async function enterActive(client, ctx, { persist = true } = {}) {
    const recovery = await client.recovery(ctx.sessionManager.getSessionId());
    mode = "active";
    anchor = client;
    healthError = null;
    pendingProposal = null;
    setActiveTools();
    ctx.ui.setStatus("anchor", `Anchor: ${recovery.task.title}`);
    if (persist) persistMode(mode, ctx);
    return recovery;
  }

  function block(error, ctx) {
    mode = "active";
    anchor = null;
    healthError = error instanceof Error ? error.message : String(error);
    setPlanningTools(false);
    ctx.ui.setStatus("anchor", "Anchor: blocked");
    ctx.ui.notify(`Anchor recovery failed: ${healthError}`, "error");
  }

  function recordProposal(status, proposal, proposalId, ctx, model = pendingProposal?.model ?? modelName(ctx.model), toolCallId = pendingProposal?.toolCallId) {
    pi.appendEntry(PROPOSAL, {
      schema: "anchor.proposal.v1",
      session_id: ctx.sessionManager.getSessionId(),
      proposal_id: proposalId,
      status,
      proposal,
      model,
      ...(toolCallId ? { tool_call_id: toolCallId } : {}),
    });
    pendingProposal = { status, proposal, proposalId, model, toolCallId, entryId: ctx.sessionManager.getLeafId?.() ?? null };
  }

  function invalidateProposal(status, ctx) {
    if (!pendingProposal) return;
    recordProposal(status, pendingProposal.proposal, pendingProposal.proposalId, ctx, pendingProposal.model, pendingProposal.toolCallId);
    pendingProposal = null;
  }

  async function commitProposal(ctx) {
    if (mode !== "planning" || pendingProposal?.status !== "sealed") throw new Error("No sealed Anchor proposal is ready");
    if (pendingProposal.entryId && ctx.sessionManager.getLeafId?.() !== pendingProposal.entryId) {
      invalidateProposal("stale", ctx);
      throw new Error("The Anchor proposal is stale; continue Planning and propose again");
    }
    const { proposal, proposalId, model } = pendingProposal;
    const sessionId = ctx.sessionManager.getSessionId();
    const client = new AnchorClient({ workspace: ctx.cwd, statePath });
    const created = await client.begin({
      sessionId,
      proposalHash: proposalId,
      title: proposal.title,
      contract: proposalContract(proposal),
      checkpoint: proposalCheckpoint(proposal, sessionId, proposalId, model),
    });
    client.taskId = created.task.task_id;
    await enterActive(client, ctx);
    pi.setSessionName(proposal.title);
    pi.sendMessage({
      customType: "anchor.execution-start",
      content: "The user accepted the Anchor Contract. Begin execution from the authoritative Checkpoint and current next plan.",
      display: false,
      details: { task_id: created.task.task_id },
    }, { triggerTurn: true, deliverAs: "followUp" });
    return created;
  }

  async function reviewProposal(ctx, forcedAction) {
    if (mode !== "planning") return ctx.ui.notify("Anchor Planning is not active.", "warning");
    if (pendingProposal?.status !== "sealed") return ctx.ui.notify("There is no sealed Anchor proposal to review.", "warning");
    if (pendingProposal.entryId && ctx.sessionManager.getLeafId?.() !== pendingProposal.entryId) {
      invalidateProposal("stale", ctx);
      return ctx.ui.notify("The proposal became stale after the session changed. Continue Planning.", "warning");
    }
    const action = forcedAction ?? await ctx.ui.select("Review Anchor proposal", [ACCEPT, REVISE, CANCEL]);
    if (action === ACCEPT) {
      await commitProposal(ctx);
    } else if (action === REVISE) {
      invalidateProposal("revising", ctx);
      ctx.ui.setStatus("anchor", "Anchor: planning");
    } else if (action === CANCEL) {
      invalidateProposal("cancelled", ctx);
      enterNormal(ctx);
    }
  }

  pi.registerTool({
    name: RECALL_TOOL,
    label: "Recall Anchor Evidence",
    description: "Recall one exact demoted cognition item from an immutable Checkpoint.",
    promptSnippet: "Use only when a current decision needs the exact content behind a Knowledge Index reference",
    parameters: Type.Object({
      locator: Type.String({ minLength: 1 }),
      content_hash: Type.Optional(Type.String({ minLength: 1 })),
    }),
    executionMode: "sequential",
    async execute(_toolCallId, args, _signal, _onUpdate, ctx) {
      if (mode !== "active" || !anchor) throw new Error("anchor_recall is available only for an active Anchor");
      const result = await anchor.recall(args.locator, args.content_hash);
      return { content: [{ type: "text", text: JSON.stringify(result) }], details: result };
    },
  });

  pi.registerTool({
    name: "anchor_ask",
    label: "Ask Anchor Question",
    description: "Ask one consequential Planning question using native Pi input.",
    promptSnippet: "Ask one focused question when its answer materially changes the Anchor proposal",
    parameters: Type.Object({
      question: Type.String({ minLength: 1 }),
      options: Type.Optional(Type.Array(Type.String({ minLength: 1 }), { maxItems: 5 })),
    }),
    executionMode: "sequential",
    async execute(_toolCallId, args, _signal, _onUpdate, ctx) {
      if (mode !== "planning") throw new Error("anchor_ask is available only during Anchor Planning");
      if (!ctx.hasUI) return { content: [{ type: "text", text: "Ask the question in normal conversation; native input is unavailable." }] };
      const options = args.options ?? [];
      let answer;
      if (options.length) {
        const other = "Write another answer";
        answer = await ctx.ui.select(args.question, [...options, other]);
        if (answer === other) answer = await ctx.ui.input(args.question);
      } else {
        answer = await ctx.ui.input(args.question);
      }
      return { content: [{ type: "text", text: answer ? `User answer: ${answer}` : "The user cancelled without answering." }] };
    },
  });

  pi.registerTool({
    name: "anchor_propose",
    label: "Propose Anchor",
    description: "Submit a complete Anchor Contract and initial cognition for review after this turn settles.",
    promptSnippet: "Propose a complete durable Anchor only after resolving every critical ambiguity",
    promptGuidelines: ["Do not call anchor_propose early. Its turn ends before independent user review."],
    parameters: proposalSchema,
    executionMode: "sequential",
    async execute(toolCallId, proposal, _signal, _onUpdate, ctx) {
      if (mode !== "planning") throw new Error("anchor_propose is available only during Anchor Planning");
      if (turnToolCalls.size > 1 || (turnToolCalls.size === 1 && !turnToolCalls.has(toolCallId))) {
        throw new Error("anchor_propose must be the only tool call in its model turn");
      }
      const proposalId = hashValue({ session_id: ctx.sessionManager.getSessionId(), proposal });
      recordProposal("candidate", proposal, proposalId, ctx, modelName(ctx.model), toolCallId);
      return {
        content: [{ type: "text", text: "Anchor proposal recorded. It becomes reviewable after this complete turn is stored." }],
        details: { status: "candidate", proposal_id: proposalId },
        terminate: true,
      };
    },
  });

  pi.registerCommand("anchor", {
    description: "Start, inspect, review, or cancel Anchor Planning",
    handler: async (args, ctx) => {
      const raw = args.trim();
      const [command = "status", ...commandParts] = raw.split(/\s+/);
      if (command === "start") {
        if (mode === "active") return ctx.ui.notify("This session already has an active Anchor.", "warning");
        if (mode === "planning") return ctx.ui.notify("Anchor Planning is already active.", "info");
        enterPlanning(ctx);
        return;
      }
      if (command === "review") return reviewProposal(ctx);
      if (command === "accept") return reviewProposal(ctx, ACCEPT);
      if (command === "cancel") {
        if (mode !== "planning") return ctx.ui.notify("Only Anchor Planning can be cancelled.", "warning");
        invalidateProposal("cancelled", ctx);
        enterNormal(ctx);
        return;
      }
      if (command === "recall") {
        if (mode !== "active" || !anchor) return ctx.ui.notify("An active Anchor is required for recall.", "warning");
        const locator = commandParts.join(" ");
        if (!locator) return ctx.ui.notify("Usage: /anchor recall checkpoint:<version>:item:<id>", "warning");
        const result = await anchor.recall(locator);
        return ctx.ui.notify(JSON.stringify(result.item), "info");
      }
      if (command !== "status") return ctx.ui.notify("Usage: /anchor [start|status|review|cancel|recall <locator>]", "warning");
      if (healthError) return ctx.ui.notify(`Anchor is blocked: ${healthError}`, "error");
      if (mode === "normal") return ctx.ui.notify("Normal Pi mode. Use /anchor start for durable long-running work.", "info");
      if (mode === "planning") return ctx.ui.notify(pendingProposal?.status === "sealed" ? "Anchor proposal is ready for /anchor review." : "Anchor Planning is active.", "info");
      const recovery = await anchor.recovery(ctx.sessionManager.getSessionId());
      ctx.ui.notify(`${recovery.task.task_id}: ${recovery.task.title}\n${recovery.task.lifecycle_status}\nNext: ${nextAction(recovery.checkpoint.cognition)}\nProtocol: ${ANCHOR_UPDATE_PROTOCOL}`, "info");
    },
  });

  pi.registerCommand("update", {
    description: "Update Anchor cognition and roll the context window",
    handler: async (_args, ctx) => {
      if (!anchor) return ctx.ui.notify("An active Anchor is required for Update.", "warning");
      return new Promise((resolvePromise, reject) => ctx.compact({ onComplete: resolvePromise, onError: reject }));
    },
  });

  pi.on("session_start", async (event, ctx) => {
    if (localCodex) {
      const model = ctx.modelRegistry.find("local-codex", "gpt-5.6-sol");
      if (model) await pi.setModel(model);
    }
    setNormalTools();
    mode = "normal";
    anchor = null;
    healthError = null;
    pendingProposal = null;

    const sessionId = ctx.sessionManager.getSessionId();
    const entries = sessionEntries(ctx);
    const savedMode = lastOwned(entries, MODE, sessionId)?.data;
    const savedProposal = lastOwned(entries, PROPOSAL, sessionId);
    const explicitPath = String(pi.getFlag("anchor-state") || process.env.ANCHOR_STATE_PATH || "").trim();
    statePath = savedMode?.state_path || (explicitPath ? resolve(ctx.cwd, explicitPath) : defaultStatePath(sessionId));

    if (savedProposal?.data?.status === "sealed") {
      pendingProposal = {
        status: "sealed",
        proposal: savedProposal.data.proposal,
        proposalId: savedProposal.data.proposal_id,
        model: savedProposal.data.model ?? "unknown",
        toolCallId: savedProposal.data.tool_call_id,
        entryId: savedProposal.id ?? null,
      };
    }

    if (savedMode?.mode === "active" || (savedMode?.mode !== "normal" && existsSync(statePath))) {
      try {
        const recovery = await enterActive(new AnchorClient({ workspace: ctx.cwd, statePath }), ctx, { persist: savedMode?.mode !== "active" });
        await recoverUndeliveredCheckpoint(recovery, ctx);
        return;
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        if (/Anchor Task not found/.test(message) && savedMode?.mode !== "active") {
          // Initialization may have stopped after creating the schema but before Task commit.
        } else if (/different session/.test(message) && savedMode?.mode !== "active") {
          ctx.ui.notify("The selected Anchor State belongs to another Pi session; using this session's default State instead.", "warning");
          statePath = defaultStatePath(sessionId);
          if (savedMode?.mode === "planning") persistMode("planning", ctx);
        } else {
          block(error, ctx);
          return;
        }
      }
    }
    if (savedMode?.mode === "planning") {
      mode = "planning";
      setPlanningTools();
      ctx.ui.setStatus("anchor", pendingProposal ? "Anchor: review" : "Anchor: planning");
      return;
    }
    if (savedMode?.mode === "normal") return enterNormal(ctx, { persist: false });

    const explicitlyEnabled = pi.getFlag("anchor") === true || truthy(process.env.ANCHOR_ENABLED);
    if (explicitlyEnabled) return enterPlanning(ctx);
    return enterNormal(ctx);
  });

  pi.on("before_agent_start", async (event, ctx) => {
    if (healthError) {
      return { systemPrompt: `${event.systemPrompt}\n\nThis session has an unavailable Anchor. Do not perform project work. Report this recovery error and wait: ${healthError}` };
    }
    if (mode !== "planning") return;
    if (pendingProposal?.status === "sealed" && String(event.prompt ?? "").trim()) invalidateProposal("stale", ctx);
    return { systemPrompt: `${event.systemPrompt}\n\n${PLANNING_PROMPT}` };
  });

  pi.on("turn_start", () => {
    turnToolCalls = new Set();
  });

  pi.on("tool_execution_start", (event, ctx) => {
    turnToolCalls.add(event.toolCallId);
    if (pendingProposal?.status === "candidate" && event.toolCallId !== pendingProposal.toolCallId) {
      invalidateProposal("stale", ctx);
    }
  });

  pi.on("tool_call", (event) => {
    if (mode === "planning" && !PLANNING_TOOLS.includes(event.toolName)) {
      return { block: true, reason: `Anchor Planning is read-only; ${event.toolName} is unavailable.` };
    }
    if (mode !== "planning" && event.toolName !== RECALL_TOOL && ANCHOR_TOOLS.has(event.toolName)) {
      return { block: true, reason: `${event.toolName} is available only during Anchor Planning.` };
    }
    if (mode !== "active" && event.toolName === RECALL_TOOL) {
      return { block: true, reason: "anchor_recall is available only for an active Anchor." };
    }
  });

  pi.on("agent_settled", async (_event, ctx) => {
    if (mode !== "planning" || pendingProposal?.status !== "candidate") return;
    recordProposal("sealed", pendingProposal.proposal, pendingProposal.proposalId, ctx);
    ctx.ui.setStatus("anchor", "Anchor: review");
    if (ctx.hasUI) await reviewProposal(ctx);
  });

  pi.on("context", async (event, ctx) => {
    if (!anchor) return;
    return { messages: projectMessages(await anchor.recovery(ctx.sessionManager.getSessionId()), event.messages) };
  });

  pi.on("session_before_compact", async (event, ctx) => {
    if (!anchor && mode === "normal") {
      try {
        const client = new AnchorClient({ workspace: ctx.cwd, statePath });
        const result = await runBootstrap(client, event, ctx);
        await enterActive(client, ctx);
        return result;
      } catch (error) {
        if (event.signal?.aborted) return { cancel: true };
        recordBootstrapFailure(error, event, ctx);
        // Bootstrap is opportunistic; Pi's native compact remains the fallback.
        return;
      }
    }
    if (!anchor) return;
    try {
      if (expectedRecoveryFrontier
        && hashValue(compactFrontier(event.preparation, ctx.sessionManager.getSessionId())) !== hashValue(expectedRecoveryFrontier)) {
        throw new Error("Pi compaction frontier changed after the Anchor Checkpoint commit");
      }
      return await runUpdate(anchor, event, ctx);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (!event.signal.aborted) ctx.ui.notify(`Anchor Update failed: ${message}`, "error");
      return { cancel: true };
    }
  });

  function recordBootstrapFailure(error, event, ctx) {
    const preparation = event.preparation;
    const messagesToSummarize = Array.isArray(preparation?.messagesToSummarize) ? preparation.messagesToSummarize : [];
    const turnPrefixMessages = Array.isArray(preparation?.turnPrefixMessages) ? preparation.turnPrefixMessages : [];
    const details = {
      schema: "anchor.bootstrap-failure.v1",
      session_id: ctx.sessionManager.getSessionId(),
      stage: error?.bootstrapStage ?? "unknown",
      error_class: error?.bootstrapErrorClass ?? error?.constructor?.name ?? "Error",
      error_message: redactDiagnostic(error instanceof Error ? error.message : String(error)),
      model: modelName(ctx.model),
      message_count: messagesToSummarize.length + turnPrefixMessages.length,
      messages_to_summarize: messagesToSummarize.length,
      turn_prefix_messages: turnPrefixMessages.length,
      ...(typeof preparation?.tokensBefore === "number" ? { tokens_before: preparation.tokensBefore } : {}),
      ...(typeof preparation?.isSplitTurn === "boolean" ? { is_split_turn: preparation.isSplitTurn } : {}),
    };
    try {
      details.episode_hash = hashValue([...messagesToSummarize, ...turnPrefixMessages]);
      if (typeof preparation?.firstKeptEntryId === "string" && preparation.firstKeptEntryId) {
        details.frontier_hash = hashValue({
          kind: "compact",
          session_id: details.session_id,
          first_kept_entry_id: preparation.firstKeptEntryId,
          episode_hash: details.episode_hash,
          is_split_turn: Boolean(preparation.isSplitTurn),
        });
      }
    } catch {
      // The diagnostic must not interfere with Pi's native fallback.
    }
    try {
      pi.appendEntry(BOOTSTRAP_FAILURE, details);
    } catch {
      // Transcript observability is best effort; native compaction remains authoritative here.
    }
    try {
      ctx.ui.notify(`Anchor Bootstrap failed at ${details.stage}: ${details.error_message}. Continuing with Pi native compaction.`, "error");
    } catch {
      // A UI failure must not prevent native compaction.
    }
  }

  async function recoverUndeliveredCheckpoint(recovery, ctx) {
    const branch = () => ctx.sessionManager.getBranch();
    if (recovery.checkpoint.checkpoint_version === 0 && recovery.checkpoint.frontier?.kind === "planning") return;
    if (checkpointDelivered(recovery.checkpoint, branch())) return;
    ctx.ui.setWorkingMessage?.("Recovering Anchor Checkpoint receipt...");
    expectedRecoveryFrontier = recovery.checkpoint.frontier;
    try {
      await new Promise((resolvePromise, reject) => ctx.compact({ onComplete: resolvePromise, onError: reject }));
    } finally {
      expectedRecoveryFrontier = null;
      ctx.ui.setWorkingMessage?.();
    }
    if (!checkpointDelivered(recovery.checkpoint, branch())) {
      throw new Error("Pi did not persist the recovered Anchor Checkpoint receipt");
    }
  }
}

function registerLocalCodexProvider(pi) {
  const codexHome = process.env.CODEX_HOME || join(process.env.HOME || ".", ".codex");
  const configPath = join(codexHome, "config.toml");
  const authPath = join(codexHome, "auth.json");
  const config = readText(configPath);
  const baseUrl = String(process.env.ANCHOR_CODEX_BASE_URL || config.match(/base_url\s*=\s*"([^"]+)"/)?.[1] || "").trim();
  const apiKey = String(process.env.ANCHOR_CODEX_API_KEY || readCodexKey(authPath) || "").trim();
  if (!baseUrl || !apiKey || typeof pi.registerProvider !== "function") return;
  pi.registerProvider("local-codex", {
    name: "Local Codex",
    baseUrl: baseUrl.replace(/\/$/, ""),
    // Resolved from Codex auth.json (or the explicit override) above; Pi keeps
    // this credential in its provider runtime and Anchor never persists it.
    apiKey,
    api: "openai-responses",
    models: [{
      id: "gpt-5.6-sol",
      name: "GPT-5.6 Sol (local Codex)",
      reasoning: true,
      input: ["text", "image"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 200000,
      maxTokens: 32768,
      compat: { supportsStrictMode: true },
    }],
  });
  return true;
}

function readText(path) { try { return readFileSync(path, "utf8"); } catch { return ""; } }
function readCodexKey(path) {
  try { const auth = JSON.parse(readFileSync(path, "utf8")); return auth.OPENAI_API_KEY || auth.openai_api_key || ""; } catch { return ""; }
}

function sessionEntries(ctx) {
  return ctx.sessionManager.getEntries?.() ?? ctx.sessionManager.getBranch();
}

function lastOwned(entries, customType, sessionId) {
  return [...entries].reverse().find((entry) => entry.type === "custom" && entry.customType === customType && entry.data?.session_id === sessionId);
}


function redactDiagnostic(message) {
  return String(message || "Unknown bootstrap failure")
    .replace(/Bearer\s+\S+/gi, "Bearer [redacted]")
    .replace(/((?:api[_-]?key|authorization|token)[=: ]+)\S+/gi, "$1[redacted]")
    .slice(0, 500);
}

function checkpointDelivered(checkpoint, entries) {
  return entries.some((entry) => entry.type === "compaction"
    && entry.details?.schema === "anchor.compact-receipt.v1"
    && entry.details.task_id === checkpoint.task_id
    && entry.details.checkpoint_version === checkpoint.checkpoint_version
    && entry.details.checkpoint_hash === checkpoint.receipt?.content_hash
    && entry.details.event_id === checkpoint.receipt?.event_id
    && hashValue(entry.details.frontier) === hashValue(checkpoint.frontier));
}

function truthy(value) {
  return ["1", "true", "yes"].includes(String(value ?? "").toLowerCase());
}

function proposalContract(proposal) {
  return {
    schema: "anchor.contract.v1",
    goal: proposal.goal,
    rationale: proposal.rationale,
    acceptance_criteria: proposal.acceptance_criteria,
    constraints: proposal.constraints,
    non_goals: proposal.non_goals,
    verification_commands: proposal.verification_commands,
    allowed_paths: proposal.allowed_paths,
    risks: proposal.risks,
    execution_plan: proposal.execution_plan,
  };
}

function proposalCheckpoint(proposal, sessionId, proposalId = hashValue({ session_id: sessionId, proposal }), model = "unknown") {
  return {
    schema: "anchor.checkpoint-candidate.v1",
    frontier: { kind: "planning", session_id: sessionId, source_hash: proposalId },
    cognition: {
      schema: "anchor.cognition.v3",
      situation: {
        current_understanding: proposal.current_understanding,
        confirmed_facts: proposal.confirmed_facts.map((statement, index) => item("fact", statement, proposal.evidence_refs, index)),
        active_hypotheses: proposal.active_hypotheses.map((statement, index) => item("hypothesis", statement, proposal.evidence_refs, index)),
        unresolved_conflicts: [],
        blockers: proposal.blockers.map((statement, index) => item("blocker", statement, proposal.evidence_refs, index)),
      },
      experience: { decisions: proposal.decisions.map((statement, index) => item("decision", statement, proposal.evidence_refs, index)), failed_paths: [] },
      intent: { current_directive: proposal.goal, accepted_next_action: proposal.next_plan[0], next_plan: proposal.next_plan, open_questions: proposal.open_questions.map((statement, index) => item("question", statement, proposal.evidence_refs, index)) },
      knowledge_index: proposal.evidence_refs.map((locator, index) => ({ id: `ref-${index + 1}`, cue: "Planning evidence", locator, source: locator })),
    },
    transition_certificate: { schema: "anchor.transition.v1", dispositions: [] },
    provenance: { kind: "planning", model, confirmed_by: "user" },
  };
}

function modelName(model) {
  return [model?.provider, model?.id].filter(Boolean).join("/") || "unknown";
}

function nextAction(cognition) {
  return cognition?.intent?.accepted_next_action ?? cognition?.accepted_next_action ?? "(not recorded)";
}

function item(kind, statement, sources = [], index = 0) {
  return { id: `${kind}-${index + 1}`, statement, sources: sources.length ? sources : ["planning:user-confirmed"], relevance: kind };
}

export { proposalCheckpoint, proposalContract };
