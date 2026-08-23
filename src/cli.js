#!/usr/bin/env node
import { InteractiveMode } from "@earendil-works/pi-coding-agent";
import { AnchorRuntime, StateStore, compileContext, renderContext } from "./index.js";

const args = process.argv.slice(2);
const command = args[0] ?? "";

if (command === "init") {
  const goal = positionalArgs(1).join(" ").trim();
  if (!goal) throw new Error("usage: anchor init <goal>");
  const store = new StateStore(args.includes("--state") ? args[args.indexOf("--state") + 1] : ".anchor/state.json");
  console.log(JSON.stringify(await store.init({ goal }), null, 2));
} else if (command === "context") {
  const path = option("--state", ".anchor/state.json");
  const purpose = option("--purpose", "work");
  const state = await new StateStore(path).read();
  console.log(renderContext(compileContext(state, { purpose })));
} else if (command === "run") {
  const prompt = positionalArgs(1).join(" ").trim();
  if (!prompt) throw new Error("usage: anchor run <prompt> [--state path]");
  const { runtime } = await AnchorRuntime.create({ statePath: option("--state", ".anchor/state.json"), goal: option("--goal"), purpose: option("--purpose", "work"), codexHome: configuredCodexHome() });
  try {
    const outcome = await runtime.runTask(prompt);
    const last = [...(runtime.session.messages ?? [])].reverse().find((message) => message?.role === "assistant");
    if (last) console.log(last.content.filter((item) => item.type === "text").map((item) => item.text).join("\n"));
    console.error(`anchor context compile: ${runtime.metrics.last_compile_ms.toFixed(1)}ms`);
    if (outcome.segments > 1) console.error(`anchor segments: ${outcome.segments}${outcome.checkpointed ? " (stopped at checkpoint limit)" : ""}`);
  } finally { runtime.dispose(); }
} else if (command === "help" || args.includes("--help") || args.includes("-h")) {
  printHelp();
} else {
  const initial = positionalArgs(command === "chat" ? 1 : 0);
  const { runtime, modelFallbackMessage } = await AnchorRuntime.createInteractive({
    statePath: option("--state", ".anchor/state.json"),
    goal: option("--goal", "Interactive Anchor session"),
    codexHome: configuredCodexHome(),
  });
  if (args.includes("--new-session")) await runtime.newSession();
  await new InteractiveMode(runtime, {
    initialMessage: initial[0],
    initialMessages: initial.slice(1),
    modelFallbackMessage,
  }).run();
}

function option(name, fallback) {
  const index = args.indexOf(name);
  return index >= 0 ? args[index + 1] ?? fallback : fallback;
}

function positionalArgs(start = 1) {
  const values = [];
  for (let index = start; index < args.length; index += 1) {
    if (["--state", "--purpose", "--goal", "--codex-home"].includes(args[index])) {
      index += 1;
      continue;
    }
    if (args[index] === "--new-session") continue;
    values.push(args[index]);
  }
  return values;
}

function printHelp() {
  console.log(`Anchor: Pi-compatible State Context runtime

Usage:
  anchor                          Start interactive Anchor
  anchor "initial prompt"         Start with an initial prompt
  anchor chat                     Start interactive Anchor
  anchor init <goal>              Create State
  anchor context                  Show compiled State Context
  anchor run <prompt>             Run one non-interactive prompt

Options:
  --state <path>                  State file (default: .anchor/state.json)
  --goal <text>                   Initial task goal
  --purpose <purpose>             work, resume, review, verify, acceptance
  --codex-home <path>             Codex/Pi provider config directory
  --new-session                   Start a fresh Pi session in this directory`);
}

function configuredCodexHome() {
  return option("--codex-home", process.env.ANCHOR_CODEX_HOME ?? process.env.CODEX_HOME);
}
