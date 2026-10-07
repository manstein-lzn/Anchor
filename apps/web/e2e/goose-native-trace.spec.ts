import { expect, test } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { constants } from 'node:fs';
import { createHash } from 'node:crypto';
import { copyFile, mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createServer as portServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

test.use({ launchOptions: { executablePath: process.env.ANCHOR_BROWSER_BINARY } });

async function freePort() {
  const server = portServer();
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('missing port');
  await new Promise<void>(resolve => server.close(() => resolve()));
  return address.port;
}

async function stop(child?: ChildProcess) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise<void>(resolve => child.once('exit', () => resolve()));
  child.kill('SIGTERM');
  await exited;
}

function receipt(value: unknown): string | undefined {
  if (typeof value === 'string') {
    try { return receipt(JSON.parse(value)); } catch { return undefined; }
  }
  if (Array.isArray(value)) return value.map(receipt).find(item => item !== undefined);
  if (value && typeof value === 'object') {
    const object = value as Record<string, unknown>;
    if (typeof object.anchor_receipt === 'string') return object.anchor_receipt;
    return Object.values(object).map(receipt).find(item => item !== undefined);
  }
}

test('real Rust Host and Goose expose active trace before canonical completion and keep it after refresh', async ({ page, request }) => {
  test.skip(!process.env.ANCHOR_TEST_GOOSE_HOST_BINARY || !process.env.ANCHOR_GOOSE_BINARY, 'explicit Host and pinned Goose binaries required');
  test.setTimeout(90000);
  const root = await mkdtemp(join(tmpdir(), 'anchor-goose-trace-browser-'));
  const binary = join(root, 'host'), goose = join(root, 'goose');
  await copyFile(process.env.ANCHOR_TEST_GOOSE_HOST_BINARY!, binary, constants.COPYFILE_FICLONE);
  await copyFile(process.env.ANCHOR_GOOSE_BINARY!, goose, constants.COPYFILE_FICLONE);
  const gooseHash = createHash('sha256').update(await readFile(goose)).digest('hex');
  expect(gooseHash).toBe('bdf35eb00d8dcc0218fe1150a3673446f351ea699ed579062628351f00cac340');
  const graph = { objective: 'Isolated native trace browser fixture', entry: 'worker',
    agents: { worker: { model: 'models.default', instructions: 'Inspect workspace, then final_result with route verify.' } },
    ops: { verify: { run: "sh -c 'cat /in/worker/evidence.txt > verified.txt'" } },
    nodes: [{ id: 'worker', agent: 'worker' }, { id: 'verify', op: 'verify' }], edges: [{ from: 'worker', to: 'verify' }] };
  await mkdir(join(root, 'bundle'));
  await writeFile(join(root, 'bundle/graph.json'), JSON.stringify(graph));
  await writeFile(join(root, 'bundle/manifest.json'), JSON.stringify({ format: 1, graph: 'graph.json', plugins: [] }));
  const calls: unknown[] = [], failures: string[] = [], pageErrors: string[] = [];
  page.on('pageerror', error => pageErrors.push(error.message));
  let release = () => {};
  const gate = new Promise<void>(resolve => { release = resolve; });
  const provider = createServer(async (incoming, response) => {
    if (incoming.method === 'GET' && incoming.url === '/v1/models') {
      response.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify({ object: 'list', data: [{ id: 'fixture-browser', object: 'model', owned_by: 'fixture', created: 1 }] }));
      return;
    }
    try {
      let raw = '';
      for await (const chunk of incoming) raw += chunk.toString();
      const body = JSON.parse(raw);
      calls.push(body);
      const position = calls.length;
      if (incoming.url !== '/v1/chat/completions' || body.stream !== true || position > 3) throw new Error('unexpected local Provider request');
      const common = { id: `browser-${position}`, object: 'chat.completion.chunk', created: 1, model: 'fixture-browser' };
      const chunks = [];
      const chunk = (delta: unknown, finish: string | null) => ({ ...common, choices: [{ index: 0, delta, finish_reason: finish }] });
      if (position < 3) {
        const suffix = position === 1 ? 'anchor_run' : 'final_result';
        const definitions = body.tools.filter((tool: { function: { name: string } }) => tool.function.name.endsWith(`__${suffix}`));
        if (definitions.length !== 1) throw new Error('missing unique authorized tool');
        const observed = receipt(body.messages.filter((message: { role: string }) => message.role === 'tool').at(-1));
        if (position === 2 && !observed) throw new Error('missing actual business receipt');
        if (position === 2) await gate;
        if (position === 1) chunks.push(chunk({ role: 'assistant', content: 'Native browser draft, not a completed reply.' }, null));
        const args = position === 1 ? { command: ['sh', '-c', 'set -eu; test ! -e evidence.txt; printf once > evidence.txt; cat evidence.txt'] }
          : { summary: 'Canonical native reply', route: 'verify', observed_receipt: observed };
        chunks.push(chunk({ role: 'assistant', tool_calls: [{ index: 0, id: `native-call-${position}`, type: 'function', function: { name: definitions[0].function.name, arguments: JSON.stringify(args) } }] }, null));
      } else chunks.push(chunk({ role: 'assistant', content: 'Native node result was submitted.' }, null));
      chunks.push({ ...chunk({}, position < 3 ? 'tool_calls' : 'stop'), usage: { prompt_tokens: 11, completion_tokens: 7, total_tokens: 18 } });
      response.writeHead(200, { 'Content-Type': 'text/event-stream' }).end(chunks.map(item => `data: ${JSON.stringify(item)}\n\n`).join('') + 'data: [DONE]\n\n');
    } catch (error) {
      failures.push((error as Error).message);
      response.writeHead(400).end('{}');
    }
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address();
  if (!address || typeof address === 'string') throw new Error('missing Provider port');
  const port = await freePort(), base = `http://127.0.0.1:${port}`;
  let backend: ChildProcess | undefined, logs = '';
  try {
    backend = spawn(binary, ['serve'], { cwd: root, env: {
      PATH: '/usr/bin:/bin', LANG: 'C.UTF-8', TZ: 'UTC', HOME: join(root, 'home'),
      ANCHOR_RUNNER_BUNDLE_ROOT: join(root, 'bundle'), ANCHOR_RUNNER_CATALOG_ROOT: root,
      ANCHOR_RUNNER_STATE_ROOT: join(root, 'state'), ANCHOR_RUNNER_WORKSPACE_ROOT: join(root, 'work'),
      ANCHOR_RUNNER_GRAPH_NAME: 'native-trace', ANCHOR_RUNNER_LISTEN: `127.0.0.1:${port}`,
      ANCHOR_RUNNER_WEB_ROOT: fileURLToPath(new URL('../dist', import.meta.url)),
      ANCHOR_RUNNER_SCHEDULES_PATH: join(root, 'schedules.json'), ANCHOR_RUNNER_ALLOWED_COMMANDS: 'true,sh,cat,git',
      ANCHOR_MODEL_API_KEY: 'fixture-only-not-a-secret', ANCHOR_MODEL_URL: `http://127.0.0.1:${address.port}/v1`,
      ANCHOR_MODEL_NAME: 'fixture-browser', ANCHOR_MODEL_WIRE_API: 'chat', ANCHOR_GOOSE_BINARY: goose,
      ANCHOR_GOOSE_BINARY_SHA256: gooseHash, ANCHOR_GOOSE_ALLOW_SHARED_NETWORK: '1',
    } });
    backend.stderr!.on('data', chunk => { logs += chunk.toString(); });
    await expect.poll(async () => {
      if (backend?.exitCode !== null || backend?.signalCode !== null) throw new Error(logs);
      try { return (await request.get(`${base}/health`)).status(); } catch { return 0; }
    }).toBe(200);
    const response = await request.post(`${base}/trigger`, { data: { graph: 'native-trace' } });
    expect(response.status()).toBe(202);
    const run = (await response.json()).run;
    await expect.poll(() => calls.length).toBe(2);
    const during = await (await request.get(`${base}/runs/${run}`)).json();
    expect(during.state.status).toBe('running');
    expect(during.state.nodes.worker).toBeUndefined();
    await page.goto(base);
    await page.getByRole('button', { name: '\u67e5\u770b\u5f53\u524d\u8fd0\u884c', exact: true }).click();
    await page.locator('.execution-node').filter({ has: page.locator('strong', { hasText: /^worker$/ }) }).click();
    await expect(page.locator('.inspector .said')).toContainText('Native browser draft');
    const call = page.locator('.inspector details.call').filter({ hasText: 'evidence.txt' }).first();
    await call.locator('summary').click();
    await expect(call).toContainText('once');
    await page.screenshot({ path: join(root, 'live-desktop.png') });
    release();
    await expect.poll(async () => (await (await request.get(`${base}/runs/${run}`)).json()).state.status).toBe('completed');
    await expect(page.locator('.inspector .node-summary')).toContainText('Canonical native reply');
    await page.reload();
    await page.getByRole('button', { name: /^native-trace\uff0c\u5df2\u5b8c\u6210\uff0c/ }).click();
    await page.getByRole('dialog').getByRole('button', { name: '\u67e5\u770b\u8282\u70b9\u4e0e\u4ea7\u7269' }).click();
    await page.locator('.execution-node').filter({ has: page.locator('strong', { hasText: /^worker$/ }) }).click();
    await expect(page.locator('.inspector .said').first()).toContainText('Native browser draft');
    await expect(page.locator('.inspector .node-summary')).toContainText('Canonical native reply');
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.locator('.inspector').evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
    await page.screenshot({ path: join(root, 'settled-mobile.png'), fullPage: true });
    expect(pageErrors).toEqual([]); expect(failures).toEqual([]); expect(calls).toHaveLength(3);
    const saved = JSON.parse(await readFile(join(root, 'state/runs', `${run}.json`), 'utf8'));
    for (const [node, file] of [['worker', 'evidence.txt'], ['verify', 'verified.txt']]) {
      const artifact = await readFile(join(root, 'state/artifacts', saved.results[node][0].commit.id, 'files', file), 'utf8');
      expect(artifact).toBe('once');
    }
    await writeFile(join(root, 'evidence.json'), JSON.stringify({ status: 'passed', provider: 'deterministic loopback',
      real_model_calls: 0, actual_host: true, actual_goose: true, python: false, provider_requests: calls.length,
      host_sha256: createHash('sha256').update(await readFile(binary)).digest('hex'), goose_sha256: gooseHash, run, during }));
    console.log(`Native trace browser evidence: ${root}`);
  } finally {
    release(); await stop(backend); provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
    await writeFile(join(root, 'host.log'), logs);
  }
});
