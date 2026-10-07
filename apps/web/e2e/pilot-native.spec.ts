import { expect, test } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { constants } from 'node:fs';
import { copyFile, mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createServer as portServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

async function freePort() {
  const server = portServer();
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('missing port');
  await new Promise<void>(resolve => server.close(() => resolve()));
  return address.port;
}

async function stop(child?: ChildProcess) {
  if (!child || child.exitCode !== null) return;
  const exited = new Promise<void>(resolve => child.once('exit', () => resolve()));
  child.kill('SIGTERM');
  await exited;
}

test('native Rust Pilot serves React chat, tool history and restart without Python', async ({ page, request }) => {
  test.skip(!process.env.ANCHOR_TEST_NATIVE_PILOT_BINARY, 'explicit native Host binary required');
  test.setTimeout(90000);
  const web = fileURLToPath(new URL('../', import.meta.url));
  const root = await mkdtemp(join(tmpdir(), 'anchor-native-pilot-browser-'));
  const binary = join(root, 'host');
  await copyFile(process.env.ANCHOR_TEST_NATIVE_PILOT_BINARY!, binary, constants.COPYFILE_FICLONE);
  await mkdir(join(root, 'bundle'));
  await writeFile(join(root, 'bundle/graph.json'), JSON.stringify({ objective: 'native-browser-marker', entry: 'idle',
    ops: { idle: { run: 'true' } }, nodes: [{ id: 'idle', op: 'idle' }], edges: [] }));
  await writeFile(join(root, 'bundle/manifest.json'), JSON.stringify({ format: 1, graph: 'graph.json', plugins: [] }));
  const calls: unknown[] = [];
  const failures: string[] = [];
  const provider = createServer(async (incoming, response) => {
    let raw = '';
    for await (const chunk of incoming) raw += chunk.toString();
    calls.push(JSON.parse(raw));
    if (calls.length > 3 || incoming.url !== '/v1/chat/completions') {
      failures.push('unexpected provider request');
      response.writeHead(400).end('{}');
      return;
    }
    const delta = calls.length === 1 ? { role: 'assistant', tool_calls: [{ index: 0, id: 'native-call', type: 'function',
      function: { name: 'graph_read', arguments: JSON.stringify({ graph: 'native-browser' }) } }] }
      : { role: 'assistant', content: calls.length === 2 ? 'native-browser-marker #anchor/graph/native-browser' : 'Remembered native-browser-marker' };
    const common = { id: `native-${calls.length}`, object: 'chat.completion.chunk', created: 1, model: 'fixture' };
    const first = { ...common, choices: [{ index: 0, finish_reason: null, delta }] };
    const last = { ...common, choices: [{ index: 0, finish_reason: calls.length === 1 ? 'tool_calls' : 'stop', delta: {} }],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } };
    response.writeHead(200, { 'Content-Type': 'text/event-stream' }).end(`data: ${JSON.stringify(first)}\n\ndata: ${JSON.stringify(last)}\n\ndata: [DONE]\n\n`);
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address();
  if (!address || typeof address === 'string') throw new Error('missing provider port');
  const port = await freePort(), base = `http://127.0.0.1:${port}`;
  const key = 'native-browser-operator-key-32-bytes';
  const headers = { Authorization: `Bearer ${key}` };
  let backend: ChildProcess | undefined, logs = '';
  const start = () => {
    backend = spawn(binary, ['serve'], { cwd: root, env: {
      PATH: '/usr/bin:/bin', TZ: 'UTC', ANCHOR_RUNNER_BUNDLE_ROOT: join(root, 'bundle'),
      ANCHOR_RUNNER_STATE_ROOT: join(root, 'state'), ANCHOR_RUNNER_WORKSPACE_ROOT: join(root, 'work'),
      ANCHOR_RUNNER_GRAPH_NAME: 'native-browser', ANCHOR_RUNNER_LISTEN: `127.0.0.1:${port}`,
      ANCHOR_RUNNER_WEB_ROOT: join(web, 'dist'), ANCHOR_API_KEYS: JSON.stringify([key]),
      ANCHOR_MODEL_API_KEY: 'fixture-only', ANCHOR_MODEL_URL: `http://127.0.0.1:${address.port}/v1`,
      ANCHOR_MODEL_NAME: 'fixture', ANCHOR_MODEL_WIRE_API: 'chat',
    } });
    backend.stdout!.on('data', chunk => { logs += chunk.toString(); });
    backend.stderr!.on('data', chunk => { logs += chunk.toString(); });
  };
  try {
    start();
    await expect.poll(async () => {
      if (backend?.exitCode !== null) throw new Error(logs);
      try { return (await request.get(`${base}/health`, { headers })).status(); } catch { return 0; }
    }).toBe(200);
    await page.addInitScript(key => sessionStorage.setItem('anchor-api-key', key), key);
    await page.goto(base);
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    const input = page.getByLabel('发送给 Anchor Pilot');
    await input.fill('Read the native-browser graph without running it.');
    await input.press('Enter');
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('native-browser-marker');
    await expect(input).toBeEnabled();
    const sessions = (await (await request.get(`${base}/sessions`, { headers })).json()).sessions;
    expect(sessions).toHaveLength(1);
    const session = sessions[0].id;
    const turns = (await (await request.get(`${base}/sessions/${session}/turns`, { headers })).json()).turns;
    const events = await request.get(`${base}/sessions/${session}/turns/${turns[0].id}/events`, { headers });
    expect(await events.text()).toContain('tool-output-available');
    await stop(backend); start();
    await expect.poll(async () => {
      try { return (await request.get(`${base}/health`, { headers })).status(); } catch { return 0; }
    }).toBe(200);
    await page.reload();
    await expect(page.locator('.pilot-chat-heading strong')).toHaveAttribute('title', session);
    await expect(page.locator('.pilot-message.user')).toHaveCount(1);
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('native-browser-marker');
    expect(calls).toHaveLength(2);
    await input.fill('Recall the previous result without using tools.'); await input.press('Enter');
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('Remembered native-browser-marker');
    expect(calls).toHaveLength(3); expect(failures).toEqual([]);
    expect(JSON.stringify(calls[2])).toContain('native-browser-marker');
    await page.screenshot({ path: join(root, 'pilot-native.png') });
    await writeFile(join(root, 'evidence.json'), JSON.stringify({ status: 'passed', provider: 'deterministic local HTTP',
      real_model_calls: 0, python: false, session, restart: true, provider_requests: calls.length }));
    console.log(`Native Pilot browser evidence: ${root}`);
  } finally {
    await stop(backend);
    provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
    await writeFile(join(root, 'host.log'), logs);
  }
});
