import { appendFile, mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";

const EMPTY = {
  schema: "anchor.state.v1",
  revision: 0,
  task: { goal: "", acceptance: [], constraints: [] },
  phase: "open",
  completed: [],
  failures: [],
  decisions: [],
  open_questions: [],
  next_action: "",
  artifacts: [],
  evidence: [],
  beliefs: [],
  updated_at: null,
};

export const BELIEF_STATUSES = new Set(["active", "confirmed", "superseded", "refuted"]);
export const BELIEF_KINDS = new Set([
  "doctrine",
  "constraint",
  "finding",
  "negative_result",
  "invalidated_claim",
  "protocol",
]);

function normalizeBeliefs(value, name = "beliefs") {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new TypeError(`${name} must be an array`);
  const seen = new Set();
  return value.map((item, index) => {
    const label = `${name}[${index}]`;
    if (!item || typeof item !== "object") throw new TypeError(`${label} must be an object`);
    const id = typeof item.id === "string" ? item.id.trim() : "";
    if (!id) throw new TypeError(`${label}.id is required`);
    if (seen.has(id)) throw new TypeError(`duplicate belief id: ${id}`);
    seen.add(id);
    const text = typeof item.text === "string" ? item.text.trim() : "";
    if (!text) throw new TypeError(`${label}.text is required`);
    const kind = item.kind ?? "finding";
    if (!BELIEF_KINDS.has(kind)) throw new TypeError(`${label}.kind must be one of ${[...BELIEF_KINDS].join(", ")}`);
    const status = item.status ?? "active";
    if (!BELIEF_STATUSES.has(status)) throw new TypeError(`${label}.status must be one of ${[...BELIEF_STATUSES].join(", ")}`);
    return {
      id,
      text,
      kind,
      status,
      scope: typeof item.scope === "string" && item.scope.trim() ? item.scope.trim() : "global",
      confidence: item.confidence ?? "medium",
      established_rev: Number.isInteger(item.established_rev) && item.established_rev >= 0 ? item.established_rev : null,
      source: typeof item.source === "string" && item.source.trim() ? item.source.trim() : null,
      evidence: Array.isArray(item.evidence) ? [...item.evidence] : [],
      superseded_by: typeof item.superseded_by === "string" && item.superseded_by.trim() ? item.superseded_by.trim() : null,
      as_of: typeof item.as_of === "string" && item.as_of.trim() ? item.as_of.trim() : null,
      revised_rev: Number.isInteger(item.revised_rev) && item.revised_rev >= 0 ? item.revised_rev : null,
    };
  });
}

function applyBeliefOp(state, op, index) {
  if (!op || typeof op !== "object") throw new TypeError(`belief_ops[${index}] must be an object`);
  if (op.op === "add") {
    const [belief] = normalizeBeliefs([{ ...op.belief, established_rev: state.revision + 1 }], "belief");
    if (state.beliefs.some((item) => item.id === belief.id)) throw new TypeError(`duplicate belief id: ${belief.id}`);
    state.beliefs.push(belief);
    return;
  }
  const label = `belief_ops[${index}]`;
  if (typeof op.id !== "string" || !op.id.trim()) throw new TypeError(`${label}.id is required`);
  const target = state.beliefs.find((item) => item.id === op.id);
  if (!target) throw new TypeError(`${label}: unknown belief id: ${op.id}`);
  if (op.op === "confirm") target.status = "confirmed";
  else if (op.op === "refute") {
    target.status = "refuted";
    if (typeof op.reason === "string" && op.reason.trim()) target.text = `${target.text} [refuted: ${op.reason.trim()}]`;
  } else if (op.op === "supersede") {
    if (typeof op.by !== "string" || !op.by.trim()) throw new TypeError(`${label}.by is required for supersede`);
    target.status = "superseded";
    target.superseded_by = op.by.trim();
  } else if (op.op === "amend") {
    const set = op.set ?? {};
    if (!set || typeof set !== "object" || Array.isArray(set)) throw new TypeError(`${label}.set must be an object`);
    if ("id" in set) throw new TypeError(`${label}.set.id cannot be amended`);
    const allowed = ["text", "kind", "status", "confidence", "scope", "source", "evidence", "as_of"];
    for (const key of Object.keys(set)) {
      if (!allowed.includes(key)) throw new TypeError(`${label}.set.${key} is not amendable`);
    }
    const [validated] = normalizeBeliefs([{ ...target, ...set }], "amended");
    if (state.beliefs.some((item) => item.id === validated.id && item !== target)) throw new TypeError(`duplicate belief id: ${validated.id}`);
    Object.assign(target, validated, { revised_rev: state.revision + 1 });
  } else throw new TypeError(`${label}.op must be add, confirm, refute, amend, or supersede`);
}


const asStrings = (value, name) => {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string" || !item.trim())) {
    throw new TypeError(`${name} must be an array of non-empty strings`);
  }
  return [...new Set(value.map((item) => item.trim()))];
};

export function normalizeState(input) {
  const state = structuredClone(input);
  if (state.schema !== EMPTY.schema) throw new TypeError(`unsupported state schema: ${state.schema}`);
  if (!Number.isInteger(state.revision) || state.revision < 0) throw new TypeError("revision must be a non-negative integer");
  if (!state.task || typeof state.task.goal !== "string") throw new TypeError("task.goal is required");
  state.task = {
    goal: state.task.goal.trim(),
    acceptance: asStrings(state.task.acceptance, "task.acceptance"),
    constraints: asStrings(state.task.constraints, "task.constraints"),
  };
  state.phase = typeof state.phase === "string" && state.phase.trim() ? state.phase.trim() : "open";
  for (const field of ["completed", "failures", "decisions", "open_questions"]) state[field] = asStrings(state[field], field);
  state.next_action = typeof state.next_action === "string" ? state.next_action.trim() : "";
  state.artifacts = normalizeRecords(state.artifacts, "artifacts");
  state.evidence = normalizeRecords(state.evidence, "evidence");
  state.beliefs = normalizeBeliefs(state.beliefs);
  state.updated_at ??= null;
  return state;
}

function normalizeRecords(value, name) {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.some((item) => !item || typeof item !== "object")) throw new TypeError(`${name} must be an array of objects`);
  return value.map((item) => ({ ...item }));
}

function validateResultRecords(value, name) {
  for (const item of normalizeRecords(value, name)) {
    const locator = item.path ?? item.ref ?? item.id;
    if (typeof locator !== "string" || !locator.trim()) throw new TypeError(`${name} entries require path, ref, or id`);
    if (item.sha256 !== undefined && (typeof item.sha256 !== "string" || !/^[a-f0-9]{64}$/i.test(item.sha256))) {
      throw new TypeError(`${name}.sha256 must be a 64-character hex string`);
    }
  }
}

export function createState({ goal, acceptance = [], constraints = [], beliefs = [], ...rest } = {}) {
  if (typeof goal !== "string" || !goal.trim()) throw new TypeError("goal is required");
  return normalizeState({ ...EMPTY, task: { goal, acceptance, constraints }, beliefs, ...rest });
}

export class StateStore {
  constructor(path) {
    if (typeof path !== "string" || !path.trim()) throw new TypeError("StateStore path is required");
    this.path = resolve(path);
    this.eventsPath = this.path.replace(/\.json$/, ".events.jsonl");
    this.writeQueue = Promise.resolve();
  }

  #serial(task) {
    const result = this.writeQueue.then(task);
    this.writeQueue = result.catch(() => {});
    return result;
  }

  async exists() {
    try { await readFile(this.path); return true; } catch (error) { if (error.code === "ENOENT") return false; throw error; }
  }

  async init(input) {
    return this.#serial(async () => {
      if (await this.exists()) throw new Error(`state already exists: ${this.path}`);
      await mkdir(dirname(this.path), { recursive: true });
      const state = createState(input);
      await this.#write(state);
      await this.#event({ type: "state.initialized", revision: state.revision, task: state.task });
      return state;
    });
  }

  async read() {
    return normalizeState(JSON.parse(await readFile(this.path, "utf8")));
  }

  async update(mutator, { expectedRevision, event = "state.updated", data = {} } = {}) {
    return this.#serial(() => this.#update(mutator, { expectedRevision, event, data }));
  }

  async #update(mutator, { expectedRevision, event, data = {} } = {}) {
    const current = await this.read();
    if (expectedRevision !== undefined && current.revision !== expectedRevision) {
      throw new Error(`stale state: expected ${expectedRevision}, current ${current.revision}`);
    }
    const next = normalizeState(await mutator(structuredClone(current)));
    next.revision = current.revision + 1;
    next.updated_at = new Date().toISOString();
    await this.#write(next);
    await this.#event({ type: event, revision: next.revision, ...data });
    return next;
  }

  async normalize({ expectedRevision } = {}) {
    return this.update((state) => state, { expectedRevision, event: "state.normalized" });
  }

  async applyBeliefOps(ops = [], { expectedRevision } = {}) {
    if (!Array.isArray(ops)) throw new TypeError("belief ops must be an array");
    return this.update((state) => {
      ops.forEach((op, index) => applyBeliefOp(state, op, index));
      return state;
    }, { expectedRevision, event: "state.belief_ops_applied", data: { count: ops.length } });
  }

  async recordObservation(observation) {
    // Observations are audit-only: they go to the EventLog, never auto-reduced
    // into State. Durable state changes require an explicit, validated delta.
    const item = sanitizeObservation(observation);
    return this.#serial(async () => {
      await this.#event({ type: "observation.recorded", ...item });
      return this.read();
    });
  }

  async applyResult(result, { expectedRevision } = {}) {
    validateResult(result);
    return this.#serial(() => this.#update((state) => {
      if (result.phase !== undefined) state.phase = result.phase;
      for (const field of ["completed", "failures", "decisions", "open_questions"]) {
        if (result[field]) state[field].push(...result[field]);
      }
      if (result.next_action !== undefined) state.next_action = result.next_action;
      if (result.artifacts) state.artifacts.push(...result.artifacts);
      if (result.evidence) state.evidence.push(...result.evidence);
      return state;
    }, { expectedRevision, event: "state.result_applied", data: { result_type: result.type } }));
  }

  async #write(state) {
    const temporary = `${this.path}.${process.pid}.tmp`;
    await writeFile(temporary, `${JSON.stringify(normalizeState(state), null, 2)}\n`, "utf8");
    await rename(temporary, this.path);
  }

  async #event(event) {
    await mkdir(dirname(this.eventsPath), { recursive: true });
    await appendFile(this.eventsPath, `${JSON.stringify({ at: new Date().toISOString(), ...event })}\n`, "utf8");
  }
}

function sanitizeObservation(observation = {}) {
  if (!observation || typeof observation !== "object") throw new TypeError("observation must be an object");
  const kind = observation.kind ?? "note";
  if (!['tool', 'note'].includes(kind)) throw new TypeError("observation.kind must be tool or note");
  return {
    kind,
    tool_name: typeof observation.tool_name === "string" ? observation.tool_name : "",
    isError: observation.isError === true,
    summary: String(observation.summary ?? "").slice(0, 1000),
  };
}

function validateResult(result) {
  if (!result || result.type !== "state_delta") throw new TypeError("result.type must be state_delta");
  for (const field of ["completed", "failures", "decisions", "open_questions"]) asStrings(result[field], field);
  if (result.phase !== undefined && (typeof result.phase !== "string" || !result.phase.trim())) throw new TypeError("result.phase must be a non-empty string");
  if (result.next_action !== undefined && typeof result.next_action !== "string") throw new TypeError("result.next_action must be a string");
  for (const field of ["artifacts", "evidence"]) validateResultRecords(result[field], field);
}
