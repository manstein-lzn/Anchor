import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export class AnchorClient {
  constructor({ workspace = process.cwd(), statePath, taskId, sessionId, python = process.env.ANCHOR_PYTHON ?? "python3", kernel = process.env.ANCHOR_KERNEL ?? resolve(ROOT, "python/anchor_kernel.py") } = {}) {
    this.workspace = resolve(workspace);
    this.statePath = statePath ? resolve(statePath) : null;
    this.taskId = typeof taskId === "string" && taskId.trim() ? taskId.trim() : null;
    this.sessionId = typeof sessionId === "string" && sessionId.trim() ? sessionId.trim() : null;
    this.python = python;
    this.kernel = resolve(kernel);
  }

  async begin({ title, contract, checkpoint, sessionId, proposalHash }) {
    this.sessionId = requiredText(sessionId, "Anchor sessionId");
    proposalHash ??= checkpoint?.frontier?.source_hash;
    const result = await this.#call([
      "begin",
      "--session-id", this.sessionId,
      "--proposal-hash", requiredText(proposalHash, "Anchor proposalHash"),
      "--title", title,
      "--contract-json", JSON.stringify(contract),
      "--checkpoint-json", JSON.stringify(checkpoint),
    ]);
    this.taskId = result.task?.task_id ?? this.taskId;
    return result;
  }

  async recovery(sessionId = this.sessionId) {
    this.sessionId = requiredText(sessionId, "Anchor sessionId");
    const args = ["recover", "--session-id", this.sessionId];
    if (this.taskId) args.push("--task-id", this.taskId);
    const result = await this.#call(args);
    this.taskId = result.task_id;
    return result;
  }

  async update(candidate, expectedVersion) {
    if (!Number.isInteger(expectedVersion) || expectedVersion < 0) throw new TypeError("Anchor expectedVersion is required");
    return this.#call([
      "update",
      "--session-id", requiredText(this.sessionId, "Anchor sessionId"),
      "--task-id", requiredText(this.taskId, "Anchor taskId"),
      "--expected-version", String(expectedVersion),
      "--candidate-json", JSON.stringify(candidate),
    ]);
  }

  async recall(locator, contentHash) {
    return this.#call([
      "recall",
      "--session-id", requiredText(this.sessionId, "Anchor sessionId"),
      "--task-id", requiredText(this.taskId, "Anchor taskId"),
      "--locator", requiredText(locator, "Anchor recall locator"),
      ...(contentHash ? ["--content-hash", contentHash] : []),
    ]);
  }

  #call(args) {
    // ponytail: one Python process per boundary; replace with a long-lived host only if p95 is measured as a problem.
    return new Promise((resolveResult, reject) => {
      if (!this.statePath) throw new TypeError("Anchor statePath is required");
      const prefix = [this.kernel, "--workspace", this.workspace];
      prefix.push("--state-path", this.statePath);
      const child = spawn(this.python, [...prefix, ...args], {
        cwd: this.workspace,
        env: { ...process.env, PYTHONPATH: resolve(ROOT, "python") },
        stdio: ["ignore", "pipe", "pipe"],
      });
      let stdout = "";
      let stderr = "";
      child.stdout.on("data", (chunk) => { stdout += chunk; });
      child.stderr.on("data", (chunk) => { stderr += chunk; });
      child.once("error", reject);
      child.once("close", (code) => {
        if (code !== 0) {
          reject(new Error(stderr.trim() || `Anchor exited with status ${code}`));
          return;
        }
        try {
          resolveResult(JSON.parse(stdout));
        } catch (error) {
          reject(new Error(`Anchor returned invalid JSON: ${error.message}`));
        }
      });
    });
  }
}

function requiredText(value, name) {
  if (typeof value !== "string" || !value.trim()) throw new TypeError(`${name} is required`);
  return value.trim();
}
