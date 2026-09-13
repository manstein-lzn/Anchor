/**
 * Minimal Pi Agent Node probe.
 *
 * This is deliberately outside Anchor's production runtime. It proves that
 * Pi's SDK can execute one constrained node with an Anchor-shaped input and a
 * single host-owned read tool. No Anchor database or Pi upstream source is
 * modified.
 */

import { getModel } from "/home/mansteinl/pi/packages/ai/src/index.ts";
import { readFileSync } from "node:fs";
import {
	AuthStorage,
	createAgentSession,
	createExtensionRuntime,
	ModelRegistry,
	SessionManager,
	SettingsManager,
	type ResourceLoader,
	type ToolDefinition,
} from "/home/mansteinl/pi/packages/coding-agent/src/index.ts";
import { Type } from "typebox";

const cwd = "/tmp";
const agentDir = "/tmp/anchor-pi-node-probe-agent";
const authPath = "/home/mansteinl/.pi/agent/auth.json";
const sessionPath = "/tmp/anchor-pi-node-probe-session";
const model = getModel("deepseek", "deepseek-v4-flash");
if (!model) throw new Error("DeepSeek model is unavailable from the Pi SDK");

const resourceLoader: ResourceLoader = {
	getExtensions: () => ({ extensions: [], errors: [], runtime: createExtensionRuntime() }),
	getSkills: () => ({ skills: [], diagnostics: [] }),
	getPrompts: () => ({ prompts: [], diagnostics: [] }),
	getThemes: () => ({ themes: [], diagnostics: [] }),
	getAgentsFiles: () => ({ agentsFiles: [] }),
	getSystemPrompt: () => "You are a constrained Anchor graph node. Use only the supplied task input and available tools. Return the requested result.",
	getAppendSystemPrompt: () => [],
	extendResources: () => {},
	reload: async () => {},
};

const readDeclaredArtifact: ToolDefinition = {
	name: "anchor_read_declared_artifact",
	label: "Anchor read declared artifact",
	description: "Read the one immutable artifact explicitly declared by the node input.",
	parameters: Type.Object({
		ref: Type.String({ description: "The declared artifact reference" }),
	}),
	execute: async (_toolCallId, params) => {
		const value = (params as { ref: string }).ref;
		if (value !== "artifact://probe/answer") {
			return { content: [{ type: "text", text: "DENIED: undeclared artifact" }], details: { denied: true } };
		}
		return { content: [{ type: "text", text: "The declared artifact contains exactly: READY" }], details: { source: value } };
	},
};

const authStorage = AuthStorage.create(authPath);
const piModels = JSON.parse(readFileSync("/home/mansteinl/.pi/agent/models.json", "utf8"));
const apiKey = piModels.providers?.DeepSeek?.apiKey;
if (typeof apiKey !== "string" || !apiKey) throw new Error("DeepSeek credential is unavailable");
authStorage.setRuntimeApiKey("deepseek", apiKey);
const modelRegistry = ModelRegistry.inMemory(authStorage);
const settingsManager = SettingsManager.inMemory({
	compaction: { enabled: true },
	retry: { enabled: false, maxRetries: 0 },
});

const { session } = await createAgentSession({
	cwd,
	agentDir,
	model,
	modelRegistry,
	authStorage,
	resourceLoader,
	settingsManager,
	tools: [],
	customTools: [readDeclaredArtifact],
	sessionManager: SessionManager.create(cwd, sessionPath),
	thinkingLevel: "off",
});

const events: string[] = [];
session.subscribe((event) => {
	if (["agent_start", "turn_start", "turn_end", "tool_execution_start", "tool_execution_end", "compaction_start", "compaction_end", "agent_end"].includes(event.type)) {
		events.push(event.type);
	}
});

try {
	await session.prompt(`Anchor Node input (immutable declared snapshot):
{"objective":"Read the declared artifact and return its exact value.","allowed_artifacts":["artifact://probe/answer"],"constraints":["Use the Anchor read tool only","Return only the artifact value"]}`);
	const stats = session.getSessionStats();
	console.log(JSON.stringify({
		status: "completed",
		sessionId: session.sessionId,
		sessionFile: session.sessionFile ?? null,
		events,
		messageCount: session.agent.state.messages.length,
		contextUsage: stats.contextUsage,
		tokens: stats.tokens,
	}, null, 2));
} finally {
	session.dispose();
}
