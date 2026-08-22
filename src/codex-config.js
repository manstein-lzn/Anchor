import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { ModelRuntime, SettingsManager } from "@earendil-works/pi-coding-agent";

const API_BY_WIRE = { responses: "openai-responses", chat: "openai-completions" };
const THINKING_LEVELS = { off: null, minimal: "minimal", low: "low", medium: "medium", high: "high", xhigh: "xhigh", max: "max" };

export async function loadCodexConfig({ codexHome = process.env.CODEX_HOME ?? join(homedir(), ".codex") } = {}) {
  const configPath = join(codexHome, "config.toml");
  const authPath = join(codexHome, "auth.json");
  const config = parseToml(await readFile(configPath, "utf8"));
  const providerId = required(config.model_provider, "model_provider");
  const provider = config.model_providers?.[providerId];
  if (!provider) throw new Error(`Codex provider is not configured: ${providerId}`);
  const modelId = required(config.model, "model");
  const baseUrl = required(provider.base_url, `model_providers.${providerId}.base_url`);
  const api = API_BY_WIRE[provider.wire_api ?? "responses"];
  if (!api) throw new Error(`Unsupported Codex wire_api: ${provider.wire_api}`);
  const auth = JSON.parse(await readFile(authPath, "utf8"));
  const apiKey = auth.OPENAI_API_KEY ?? auth.openai_api_key;
  if (typeof apiKey !== "string" || !apiKey) throw new Error(`Codex auth is missing OPENAI_API_KEY: ${authPath}`);
  const headers = resolveHeaders(provider.env_http_headers);
  const requestRetries = nonNegativeInt(provider.request_max_retries, 0);
  const streamRetries = nonNegativeInt(provider.stream_max_retries, 0);
  const thinkingLevel = config.model_reasoning_effort ?? "medium";
  if (!Object.hasOwn(THINKING_LEVELS, thinkingLevel)) throw new Error(`Unsupported Codex reasoning effort: ${thinkingLevel}`);
  const contextWindow = Number.isInteger(config.context_window) && config.context_window > 0 ? config.context_window : null;
  const maxTokens = Number.isInteger(config.max_tokens) && config.max_tokens > 0 ? config.max_tokens : null;
  return {
    codexHome,
    configPath,
    authPath,
    providerId,
    providerName: provider.name ?? providerId,
    modelId,
    modelReasoningEffort: thinkingLevel,
    baseUrl,
    api,
    wireApi: provider.wire_api ?? "responses",
    apiKey,
    headers,
    requestMaxRetries: requestRetries,
    streamMaxRetries: streamRetries,
    disableResponseStorage: config.disable_response_storage === true,
    contextWindowOverride: contextWindow,
    maxTokensOverride: maxTokens,
  };
}

export async function createCodexRuntime({ cwd = process.cwd(), agentDir, codexHome } = {}) {
  const codex = await loadCodexConfig({ codexHome });
  const modelRuntime = await ModelRuntime.create({ modelsPath: null, refreshOnCreate: false });
  modelRuntime.registerProvider(codex.providerId, {
    name: codex.providerName,
    baseUrl: codex.baseUrl,
    api: codex.api,
    apiKey: codex.apiKey,
    headers: codex.headers,
    models: [modelDefinition(codex)],
  });
  await modelRuntime.refresh({ allowNetwork: false, providers: [codex.providerId] });

  const settingsManager = SettingsManager.create(cwd, agentDir);
  settingsManager.applyOverrides({
    defaultProvider: codex.providerId,
    defaultModel: codex.modelId,
    defaultThinkingLevel: codex.modelReasoningEffort,
    // Codex's retry counts are transport-level policy; Pi's agent retry would duplicate them.
    retry: {
      enabled: false,
      maxRetries: 0,
      provider: {
        maxRetries: codex.streamMaxRetries,
      },
    },
  });
  const model = modelRuntime.getModel(codex.providerId, codex.modelId);
  if (!model) throw new Error(`Codex model was not registered: ${codex.providerId}/${codex.modelId}`);
  return { ...codex, modelRuntime, settingsManager, model };
}

function modelDefinition(codex) {
  return {
    id: codex.modelId,
    name: codex.modelId,
    api: codex.api,
    reasoning: true,
    thinkingLevelMap: THINKING_LEVELS,
    input: ["text", "image"],
    // Optional config.toml overrides (context_window / max_tokens): used by
    // experiments to declare a smaller window than the provider's real one.
    contextWindow: codex.contextWindowOverride ?? 272000,
    maxTokens: codex.maxTokensOverride ?? 128000,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    compat: {
      supportsReasoningEffort: true,
      supportsStrictMode: true,
      supportsOpenAIGrammarTools: true,
    },
  };
}

function required(value, name) {
  if (typeof value !== "string" || !value.trim()) throw new Error(`Codex config is missing ${name}`);
  return value.trim();
}

function nonNegativeInt(value, fallback) {
  return Number.isInteger(value) && value >= 0 ? value : fallback;
}

function resolveHeaders(value) {
  if (!value || typeof value !== "object") return undefined;
  const headers = {};
  for (const [header, envName] of Object.entries(value)) {
    if (typeof envName === "string" && process.env[envName] !== undefined) headers[header] = process.env[envName];
  }
  return Object.keys(headers).length ? headers : undefined;
}

function parseToml(source) {
  const root = {};
  let section = root;
  let skipArray = false;
  for (const rawLine of source.split(/\r?\n/)) {
    const line = stripComment(rawLine).trim();
    if (!line) continue;
    if (skipArray) {
      if (line.includes("]")) skipArray = false;
      continue;
    }
    const table = line.match(/^\[([^\]]+)\]$/);
    if (table) {
      section = root;
      for (const part of table[1].split(".")) section = section[part] ??= {};
      continue;
    }
    const assignment = line.match(/^([A-Za-z0-9_-]+)\s*=\s*(.*)$/);
    if (!assignment) continue;
    const [, key, rawValue] = assignment;
    if (rawValue.startsWith("[") && !rawValue.includes("]")) {
      skipArray = true;
      continue;
    }
    section[key] = parseValue(rawValue);
  }
  return root;
}

function parseValue(value) {
  if (value.startsWith("{") && value.endsWith("}")) {
    const object = {};
    for (const pair of splitTopLevel(value.slice(1, -1), ",")) {
      const match = pair.match(/^\s*(?:"([^"]+)"|'([^']+)'|([A-Za-z0-9_-]+))\s*=\s*(.*)$/);
      if (match) object[match[1] ?? match[2] ?? match[3]] = parseValue(match[4]);
    }
    return object;
  }
  if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) return value.slice(1, -1);
  if (value === "true" || value === "false") return value === "true";
  if (/^-?\d+$/.test(value)) return Number(value);
  if (value.startsWith("[") && value.endsWith("]")) return splitTopLevel(value.slice(1, -1), ",").filter(Boolean).map(parseValue);
  return value;
}

function splitTopLevel(value, delimiter) {
  const parts = [];
  let start = 0;
  let quote = null;
  let depth = 0;
  for (let index = 0; index < value.length; index += 1) {
    const char = value[index];
    if (quote) {
      if (char === quote && value[index - 1] !== "\\") quote = null;
    } else if (char === '"' || char === "'") quote = char;
    else if (char === "{" || char === "[") depth += 1;
    else if (char === "}" || char === "]") depth -= 1;
    else if (char === delimiter && depth === 0) {
      parts.push(value.slice(start, index).trim());
      start = index + 1;
    }
  }
  parts.push(value.slice(start).trim());
  return parts;
}

function stripComment(line) {
  let quote = null;
  for (let index = 0; index < line.length; index += 1) {
    const char = line[index];
    if (quote) {
      if (char === quote && line[index - 1] !== "\\") quote = null;
    } else if (char === '"' || char === "'") quote = char;
    else if (char === "#") return line.slice(0, index);
  }
  return line;
}
