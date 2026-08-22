import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import assert from "node:assert/strict";
import { createCodexRuntime, loadCodexConfig } from "../src/codex-config.js";

test("loads Codex provider and registers the Responses model without Pi config", async () => {
  const home = await mkdtemp(join(tmpdir(), "anchor-codex-"));
  await writeFile(join(home, "config.toml"), `
model = "gpt-test"
model_reasoning_effort = "high"
model_provider = "gateway"
disable_response_storage = true

[model_providers.gateway]
name = "Gateway"
base_url = "https://gateway.test/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 2
env_http_headers = { "X-Test" = "ANCHOR_TEST_HEADER" }

[model_providers.gateway.auth]
command = "unused"
args = [
  "-c",
  "unused",
]
`);
  await writeFile(join(home, "auth.json"), JSON.stringify({ OPENAI_API_KEY: "test-key" }));
  const previousHeader = process.env.ANCHOR_TEST_HEADER;
  process.env.ANCHOR_TEST_HEADER = "header-value";
  try {
    const config = await loadCodexConfig({ codexHome: home });
    assert.deepEqual({
      providerId: config.providerId,
      modelId: config.modelId,
      api: config.api,
      baseUrl: config.baseUrl,
      reasoning: config.modelReasoningEffort,
      retries: [config.requestMaxRetries, config.streamMaxRetries],
      headers: config.headers,
    }, {
      providerId: "gateway",
      modelId: "gpt-test",
      api: "openai-responses",
      baseUrl: "https://gateway.test/v1",
      reasoning: "high",
      retries: [0, 2],
      headers: { "X-Test": "header-value" },
    });
    const runtime = await createCodexRuntime({ codexHome: home });
    assert.equal(runtime.model.provider, "gateway");
    assert.equal(runtime.model.id, "gpt-test");
    assert.equal(runtime.model.api, "openai-responses");
    assert.equal(runtime.settingsManager.getProviderRetrySettings().maxRetries, 2);
    assert.equal(runtime.settingsManager.getRetrySettings().enabled, false);
  } finally {
    if (previousHeader === undefined) delete process.env.ANCHOR_TEST_HEADER;
    else process.env.ANCHOR_TEST_HEADER = previousHeader;
  }
  assert.match(await readFile(join(home, "auth.json"), "utf8"), /test-key/);
});
