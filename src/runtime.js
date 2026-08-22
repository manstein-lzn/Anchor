import {
  createAgentSession,
  createAgentSessionFromServices,
  createAgentSessionRuntime,
  createAgentSessionServices,
  getAgentDir,
  SessionManager,
  SettingsManager,
} from "@earendil-works/pi-coding-agent";
import { compileContext, projectMessages } from "./context.js";
import { createCodexRuntime } from "./codex-config.js";
import { StateStore } from "./state.js";

export class AnchorRuntime {
  #lastUserIndex = null;
  #turnObservations = [];

  constructor({ session, runtimeHost, store, purpose = "work", capabilities = [], disablePiCompaction = true, turnBudget = {} }) {
    this.runtimeHost = runtimeHost;
    this._session = session ?? runtimeHost?.session;
    this.store = store;
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
    const store = new StateStore(statePath);
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
      runtime: new AnchorRuntime({ runtimeHost, store, purpose, capabilities, disablePiCompaction }),
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
      this.#bindSession(this.runtimeHost.session);
      await this._rebind?.();
    });
  }

  #bindSession(session) {
    if (!session || session === this._session && this.unsubscribe) return;
    this.unsubscribe?.();
    this._session = session;
    if (this.disablePiCompaction) session.settingsManager?.applyOverrides?.({ compaction: { enabled: false } });
    this.#attachContextTransform(session);
    this.unsubscribe = session.agent.subscribe((event) => this.#observe(session, event));
  }

  #attachContextTransform(session) {
    const original = session.agent.transformContext;
    session.agent.transformContext = async (messages, signal) => {
      const started = performance.now();
      const state = await this.store.read();
      const envelope = compileContext(state, { purpose: this.purpose, capabilities: this.capabilities });
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
          this.checkpointPending = false;
          this.lastTurnCheckpointed = false;
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
          "Continue without repeating elided work. Conclude this phase promptly:",
          "report confirmed findings, decisions, open questions, and next_action so they can be committed to authoritative state.",
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
  const content = message?.content;
  if (typeof content === "string") return content.length;
  if (!Array.isArray(content)) return 0;
  return content.reduce((sum, item) => sum + (typeof item?.text === "string" ? item.text.length : 0), 0);
}

export { StateStore };
