import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import {
  createAgentSession,
  createAgentSessionFromServices,
  createAgentSessionRuntime,
  createAgentSessionServices,
  getAgentDir,
  SessionManager,
  SettingsManager,
} from "@earendil-works/pi-coding-agent";
import { compileContext, projectMessages, verifyEvidence } from "./context.js";
import { createCodexRuntime } from "./codex-config.js";
import { StateStore } from "./state.js";

export class AnchorRuntime {
  #lastUserIndex = null;
  #turnObservations = [];
  #declarationNudged = false;
  #pendingFileWrites = new Set();
  #pendingCapture = false;
  #capturing = false;
  #turnErrored = false;
  #rawPrompt = null;
  #evidenceCache;
  #statePathFollowsCwd = false;
  #stateGoal = "Interactive Anchor session";

  constructor({ session, runtimeHost, store, purpose = "work", capabilities = [], disablePiCompaction = true, turnBudget = {}, freshness = {}, statePathFollowsCwd = false, stateGoal = "Interactive Anchor session" }) {
    this.runtimeHost = runtimeHost;
    this._session = session ?? runtimeHost?.session;
    this.store = store;
    this.#statePathFollowsCwd = statePathFollowsCwd;
    this.#stateGoal = stateGoal;
    this.purpose = purpose;
    this.capabilities = capabilities;
    this.lastContext = null;
    this.metrics = { compilations: 0, total_compile_ms: 0, last_compile_ms: 0 };
    this.fallback = Promise.resolve();
    this.disablePiCompaction = disablePiCompaction;
    // Bounded-invocation budget: when the current turn's projected traffic
    // exceeds maxTurnChars, older messages are replaced by a deterministic
    // digest and the model is asked to conclude the phase (checkpoint).
    this.turnBudget = { maxTurnChars: 160_000, tailChars: 40_000, ...turnBudget };
    this.freshness = freshness;
    this.#evidenceCache = new Map();
    this.checkpointPending = false;
    this.lastTurnCheckpointed = false;
    this.lastCheckpointInfo = null;
    this._rebind = undefined;
    this.#bindSession(this._session);
    if (runtimeHost) this.#installHostRebind();
  }

  static async create({ statePath = ".anchor/state.json", goal, acceptance, constraints, purpose, capabilities, disablePiCompaction = true, turnBudget, codexHome, ...piOptions } = {}) {
    const store = new StateStore(statePath);
    if (!(await store.exists())) await store.init({ goal, acceptance, constraints });
    const codex = await createCodexRuntime({ cwd: piOptions.cwd, agentDir: piOptions.agentDir, codexHome });
    const { session, ...runtime } = await createAgentSession({
      ...piOptions,
      modelRuntime: codex.modelRuntime,
      settingsManager: codex.settingsManager,
      model: codex.model,
      thinkingLevel: codex.modelReasoningEffort,
    });
    return { runtime: new AnchorRuntime({ session, store, purpose, capabilities, disablePiCompaction, turnBudget }), ...runtime };
  }

  static async createInteractive({ statePath = ".anchor/state.json", goal = "Interactive Anchor session", purpose = "work", capabilities = [], disablePiCompaction = true, cwd = process.cwd(), agentDir = getAgentDir(), codexHome } = {}) {
    const followsCwd = statePath === ".anchor/state.json";
    const initialStatePath = followsCwd ? join(cwd, ".anchor/state.json") : statePath;
    const store = new StateStore(initialStatePath);
    if (!(await store.exists())) await store.init({ goal });
    const sessionManager = SessionManager.create(cwd);
    const codex = await createCodexRuntime({ cwd, agentDir, codexHome });
    const createRuntime = async ({ cwd: sessionCwd, agentDir: sessionAgentDir, sessionManager: manager, sessionStartEvent }) => {
      const settingsManager = SettingsManager.create(sessionCwd, sessionAgentDir);
      settingsManager.applyOverrides({
        defaultProvider: codex.providerId,
        defaultModel: codex.modelId,
        defaultThinkingLevel: codex.modelReasoningEffort,
        retry: { enabled: false, maxRetries: 0, provider: { maxRetries: codex.streamMaxRetries } },
      });
      if (disablePiCompaction) settingsManager.applyOverrides({ compaction: { enabled: false } });
      const services = await createAgentSessionServices({ cwd: sessionCwd, agentDir: sessionAgentDir, settingsManager, modelRuntime: codex.modelRuntime });
      return {
        ...(await createAgentSessionFromServices({ services, sessionManager: manager, sessionStartEvent, model: codex.model, thinkingLevel: codex.modelReasoningEffort })),
        services,
        diagnostics: services.diagnostics,
      };
    };
    const runtimeHost = await createAgentSessionRuntime(createRuntime, { cwd, agentDir, sessionManager });
    return {
      runtime: new AnchorRuntime({ runtimeHost, store, purpose, capabilities, disablePiCompaction, statePathFollowsCwd: followsCwd, stateGoal: goal }),
      modelFallbackMessage: runtimeHost.modelFallbackMessage,
    };
  }

  get session() {
    return this.runtimeHost?.session ?? this._session;
  }

  get services() {
    return this.runtimeHost?.services;
  }

  get cwd() {
    return this.runtimeHost?.cwd ?? this.session?.sessionManager?.getCwd?.();
  }

  get diagnostics() {
    return this.runtimeHost?.diagnostics ?? [];
  }

  get modelFallbackMessage() {
    return this.runtimeHost?.modelFallbackMessage;
  }

  setBeforeSessionInvalidate(callback) {
    return this.runtimeHost?.setBeforeSessionInvalidate(callback);
  }

  setRebindSession(callback) {
    this._rebind = callback;
    if (this.runtimeHost) this.#installHostRebind();
  }

  newSession(options) {
    return this.runtimeHost.newSession(options);
  }

  switchSession(path, options) {
    return this.runtimeHost.switchSession(path, options);
  }

  fork(entryId, options) {
    return this.runtimeHost.fork(entryId, options);
  }

  importFromJsonl(path, cwd) {
    return this.runtimeHost.importFromJsonl(path, cwd);
  }

  async prompt(text, options) {
    await this.fallback;
    return this.session.prompt(text, options);
  }

  // P1: mandatory cognition declaration. Parse the final assistant message
  // for an anchor-state-delta block; commit it if valid, nudge once if the
  // turn did real work but declared nothing.
  async #captureDeclaration() {
    const messages = this.session.messages ?? [];
    const lastAssistant = [...messages].reverse().find((message) => message?.role === "assistant");
    if (lastAssistant?.stopReason === "error" || lastAssistant?.errorMessage) return;
    const parsed = parseStateDelta(messageText(lastAssistant?.content));
    if (parsed) {
      await this.#commitDeclaration(parsed);
      return;
    }
    if (this.#turnObservations.length === 0 || this.#declarationNudged) return;
    this.#declarationNudged = true;
    // Use the raw prompt: going through session.prompt would recurse into
    // this very capture path.
    await this.#rawPrompt([
      "[anchor] Your reply did not include a cognition declaration.",
      "Before finishing, append a fenced block:\n```anchor-state-delta",
      '{ "learned": "what this turn established, 1-3 sentences", "blocked": "omit if nothing", "next_action": "...", "belief_ops": [] }',
      "```",
      "Interpretation only - do not narrate your work; the harness already captured file changes and tool activity.",
    ].join("\n"));
    const nudgedMessages = this.session.messages ?? [];
    const nudgedAssistant = [...nudgedMessages].reverse().find((message) => message?.role === "assistant");
    const nudged = parseStateDelta(messageText(nudgedAssistant?.content));
    if (nudged) await this.#commitDeclaration(nudged);
    else await this.store.recordObservation({ kind: "note", summary: "declaration missing: model did not emit anchor-state-delta after nudge" });
  }

  async #commitDeclaration(parsed) {
    this.#declarationNudged = false;
    const declaration = normalizeDeclaration(parsed);
    const state = await this.store.read();
    const expectedRevision = state.revision;
    const { state_delta: delta, belief_ops } = declaration;

    // Code-derived mechanical facts: file changes become hashed evidence
    // entries automatically - the model never has to list them.
    const knownPaths = new Set(state.evidence.map((item) => item.path ?? item.ref ?? item.id));
    const autoEvidence = [];
    for (const path of this.#pendingFileWrites) {
      if (knownPaths.has(path)) continue;
      const entry = { path, type: "file_change" };
      const hash = await sha256OfFile(path);
      if (hash) entry.sha256 = hash;
      autoEvidence.push(entry);
    }
    if (autoEvidence.length > 0) delta.evidence = [...(delta.evidence ?? []), ...autoEvidence];

    if (belief_ops.length > 0) await this.store.applyBeliefOps(belief_ops, { expectedRevision });
    const meaningful = delta.completed?.length || delta.failures?.length || delta.decisions?.length || delta.open_questions?.length || delta.next_action !== undefined || delta.artifacts?.length || delta.evidence?.length || delta.phase !== undefined;
    if (meaningful) await this.store.applyResult({ type: "state_delta", ...delta }, { expectedRevision: expectedRevision + (belief_ops.length > 0 ? 1 : 0) });
    else if (belief_ops.length === 0) await this.store.recordObservation({ kind: "note", summary: "declaration empty: no state change to commit" });
    this.#pendingFileWrites.clear();
  }

  // Run a task with automatic continuation across bounded invocations:
  // when a turn ends at a working-set checkpoint, re-prompt from authoritative
  // state instead of leaving the task half-done.
  async runTask(text, { maxSegments = 6 } = {}) {
    let promptText = text;
    for (let segment = 1; ; segment += 1) {
      this.lastTurnCheckpointed = false;
      await this.prompt(promptText);
      if (!this.lastTurnCheckpointed || segment >= maxSegments) {
        return { segments: segment, checkpointed: this.lastTurnCheckpointed };
      }
      const state = await this.store.read();
      promptText = [
        "[anchor] Your previous invocation reached its working-set checkpoint and ended early.",
        "Authoritative state is unchanged since you reported it. Continue the task",
        `from next_action: ${state.next_action || "(see state context)"}.`,
        "Avoid repeating work already recorded as completed.",
      ].join("\n");
    }
  }

  async applyResult(result) {
    const state = await this.store.read();
    return this.store.applyResult(result, { expectedRevision: state.revision });
  }

  async state() {
    return this.store.read();
  }

  dispose() {
    this.unsubscribe?.();
    this.unsubscribe = undefined;
    if (this.runtimeHost) return this.runtimeHost.dispose();
    return this.session?.dispose();
  }

  #installHostRebind() {
    this.runtimeHost.setRebindSession(async () => {
      if (this.#statePathFollowsCwd) await this.#bindStateForCwd(this.runtimeHost.cwd);
      this.#bindSession(this.runtimeHost.session);
      await this._rebind?.();
    });
  }

  async #bindStateForCwd(cwd) {
    const statePath = join(cwd, ".anchor/state.json");
    if (this.store.path === statePath) return;
    const store = new StateStore(statePath);
    if (!(await store.exists())) await store.init({ goal: this.#stateGoal });
    this.store = store;
    this.#evidenceCache.clear();
  }

  #bindSession(session) {
    if (!session || session === this._session && this.unsubscribe) return;
    this.unsubscribe?.();
    this._session = session;
    if (this.disablePiCompaction) session.settingsManager?.applyOverrides?.({ compaction: { enabled: false } });
    this.#attachContextTransform(session);
    this.#wrapSessionPrompt(session);
    this.unsubscribe = session.agent.subscribe((event) => this.#observe(session, event));
  }

  // Wrap session.prompt itself so declaration capture applies to every caller
  // (interactive mode, runTask, CLI), not just AnchorRuntime.prompt. The
  // capture fires once per completed turn, gated on agent_end.
  #wrapSessionPrompt(session) {
    if (typeof session.prompt !== "function" || session.__anchorPromptWrapped) return;
    const original = session.prompt.bind(session);
    this.#rawPrompt = original;
    session.__anchorPromptWrapped = true;
    session.prompt = async (text, options) => {
      const result = await original(text, options);
      if (this.#pendingCapture && !this.#capturing) {
        this.#pendingCapture = false;
        this.#capturing = true;
        try {
          await this.#captureDeclaration();
        } finally {
          this.#capturing = false;
        }
      }
      return result;
    };
  }

  #attachContextTransform(session) {
    const original = session.agent.transformContext;
    session.agent.transformContext = async (messages, signal) => {
      const started = performance.now();
      const state = await this.store.read();
      const envelope = compileContext(state, { purpose: this.purpose, capabilities: this.capabilities, freshness: this.freshness });
      envelope.evidence_check = await verifyEvidence(state, { cache: this.#evidenceCache });
      this.lastContext = envelope;
      this.metrics.compilations += 1;
      this.metrics.last_compile_ms = performance.now() - started;
      this.metrics.total_compile_ms += this.metrics.last_compile_ms;
      const lastUser = messages.map((message, index) => [message, index]).filter(([message]) => message.role === "user").at(-1)?.[1];
      if (typeof lastUser === "number" && lastUser !== this.#lastUserIndex) {
        // New user turn: reset working-set tracking (but keep observations
        // recorded before the very first model call of the session).
        if (this.#lastUserIndex !== null) {
          this.#turnObservations = [];
          this.#pendingFileWrites.clear();
          this.checkpointPending = false;
          this.lastTurnCheckpointed = false;
          this.#declarationNudged = false;
          this.#turnErrored = false;
        }
        this.#lastUserIndex = lastUser;
      }
      const currentTurn = lastUser === undefined ? messages.slice(-1) : messages.slice(lastUser);
      const { projected } = this.#applyInvocationBudget(envelope, currentTurn);
      return original ? original(projected, signal) : projected;
    };
  }

  #applyInvocationBudget(envelope, currentTurn) {
    const totalChars = estimateChars(currentTurn);
    const { maxTurnChars, tailChars } = this.turnBudget;
    let kept = currentTurn;
    let checkpoint = null;
    if (totalChars > maxTurnChars) {
      kept = [];
      let keptChars = 0;
      for (let index = currentTurn.length - 1; index >= 0; index -= 1) {
        const size = estimateChars([currentTurn[index]]);
        if (kept.length > 0 && keptChars + size > tailChars) break;
        kept.unshift(currentTurn[index]);
        keptChars += size;
      }
      // Head-pinning: the first message of the turn carries the task
      // contract; losing it makes long invocations drift off-spec. Always
      // keep it, even at the cost of exceeding the tail budget.
      const head = currentTurn[0];
      if (!kept.includes(head)) kept.unshift(head);
      const elidedMessages = currentTurn.length - kept.length;
      const digestLines = this.#turnObservations.slice(-40).map((item) => `- ${item.tool_name || "tool"}${item.isError ? " [error]" : ""}: ${item.summary}`);
      checkpoint = {
        elided_messages: elidedMessages,
        elided_chars: totalChars - keptChars,
        note: [
          "[anchor invocation checkpoint]",
          `The earlier part of this invocation (${elidedMessages} messages, ~${totalChars - keptChars} chars of tool traffic) is no longer attached verbatim.`,
          "Raw outputs remain in the session transcript and EventLog for audit.",
          "Deterministic digest of this turn's recent tool activity:",
          "<elided-work-digest>",
          digestLines.length ? digestLines.join("\n") : "(no tool observations recorded)",
          "</elided-work-digest>",
          "",
          "Conclude this phase promptly. End your reply with a cognition declaration:",
          '```anchor-state-delta',
          '{ "learned": "...", "blocked": "omit if nothing", "next_action": "...", "belief_ops": [] }',
          '```',
        ].join("\n"),
      };
    }
    this.checkpointPending = checkpoint !== null;
    if (checkpoint) this.lastCheckpointInfo = checkpoint;
    const base = projectMessages(envelope, []);
    const projected = checkpoint
      ? [...base, { role: "user", content: [{ type: "text", text: checkpoint.note }], timestamp: Date.now() }, ...kept]
      : [...base, ...kept];
    return { projected, checkpoint };
  }

  async #observe(session, event) {
    if (event.type === "tool_call") {
      // Mechanical fact collection: file-writing tools are recorded by code,
      // never by model narration.
      const path = event?.input?.path ?? event?.input?.file_path;
      if ((event.toolName === "write" || event.toolName === "edit") && typeof path === "string" && path.trim()) {
        this.#pendingFileWrites.add(path.trim());
      }
      return;
    }
    if (event.type === "tool_execution_end") {
      this.#turnObservations.push({ tool_name: event.toolName ?? "", isError: event.isError === true, summary: summarizeToolResult(event.result).slice(0, 200) });
      await this.store.recordObservation({
        kind: "tool",
        tool_name: event.toolName,
        isError: event.isError,
        summary: summarizeToolResult(event.result),
      });
      return;
    }
    if (event.type === "agent_end") {
      this.lastTurnCheckpointed = this.checkpointPending;
      this.checkpointPending = false;
      const lastAssistant = [...(event.messages ?? [])].reverse().find((message) => message?.role === "assistant");
      this.#turnErrored = lastAssistant?.stopReason === "error" || Boolean(lastAssistant?.errorMessage);
      this.#pendingCapture = !this.#turnErrored;
    }
    if (event.type === "agent_end" && isOverflow(event.messages) && typeof session.compact === "function") {
      this.fallback = this.fallback
        .then(() => session.compact("Anchor State Context overflow fallback"))
        .catch(() => undefined);
    }
  }
}

function isOverflow(messages = []) {
  const assistant = [...messages].reverse().find((message) => message?.role === "assistant");
  if (!assistant) return false;
  const error = String(assistant.errorMessage ?? "").toLowerCase();
  return assistant.stopReason === "length" || /context|token|maximum.{0,12}(length|token)/.test(error);
}

function summarizeToolResult(result) {
  if (typeof result === "string") return result;
  if (!result || typeof result !== "object") return String(result ?? "");
  const text = Array.isArray(result.content) ? result.content.filter((item) => item?.type === "text").map((item) => item.text).join(" ") : JSON.stringify(result.details ?? result);
  return String(text).replace(/\s+/g, " ").trim();
}

function estimateChars(messages) {
  return messages.reduce((sum, message) => sum + estimateMessageChars(message), 0);
}

function estimateMessageChars(message) {
  return estimateContextValue(message?.content);
}

function estimateContextValue(value) {
  if (typeof value === "string") return value.length;
  if (Array.isArray(value)) return value.reduce((sum, item) => sum + estimateContextValue(item), 0);
  if (!value || typeof value !== "object") return 0;
  if (typeof value.text === "string") return value.text.length;
  if (typeof value.output === "string") return value.output.length;
  if (typeof value.arguments === "string") return value.arguments.length;
  if (value.arguments && typeof value.arguments === "object") return JSON.stringify(value.arguments).length;
  if (Array.isArray(value.content)) return value.content.reduce((sum, item) => sum + estimateContextValue(item), 0);
  return 0;
}

function messageText(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content.filter((item) => item?.type === "text").map((item) => item.text).join("\n");
}

export function parseStateDelta(text) {
  if (typeof text !== "string") return null;
  const match = text.match(/```anchor-state-delta\s*([\s\S]*?)```/);
  if (!match) return null;
  try {
    const parsed = JSON.parse(match[1]);
    if (!parsed || typeof parsed !== "object") return null;
    return parsed;
  } catch {
    return null;
  }
}

// Accept both the full schema ({state_delta, belief_ops}) and the slim
// interpretation-only schema ({learned, blocked, next_action, belief_ops}).
// The model only narrates interpretation; mechanical facts are code-derived.
export function normalizeDeclaration(parsed) {
  const delta = { ...(parsed.state_delta ?? {}) };
  if (typeof parsed.learned === "string" && parsed.learned.trim()) {
    delta.decisions = [...(delta.decisions ?? []), parsed.learned.trim()];
  }
  if (typeof parsed.blocked === "string" && parsed.blocked.trim()) {
    delta.open_questions = [...(delta.open_questions ?? []), parsed.blocked.trim()];
  }
  if (typeof parsed.next_action === "string") delta.next_action = parsed.next_action;
  return { state_delta: delta, belief_ops: Array.isArray(parsed.belief_ops) ? parsed.belief_ops : [] };
}

async function sha256OfFile(path) {
  try {
    const buffer = await readFile(path);
    return createHash("sha256").update(buffer).digest("hex");
  } catch {
    return null;
  }
}

export { StateStore };
