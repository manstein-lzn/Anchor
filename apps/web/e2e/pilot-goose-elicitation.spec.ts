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
  if (!child || child.exitCode !== null) return;
  const exited = new Promise<void>(resolve => child.once('exit', () => resolve()));
  child.kill('SIGTERM');
  await exited;
}

test('Goose native form answers and deletion confirmations use the same Turn through the real Rust API', async ({ page, request }) => {
  test.skip(!process.env.ANCHOR_TEST_GOOSE_HOST_BINARY || !process.env.ANCHOR_GOOSE_BINARY, 'explicit Rust Host and pinned Goose binaries required');
  test.setTimeout(90000);
  const root = await mkdtemp(join(tmpdir(), 'anchor-goose-browser-'));
  const binary = join(root, 'host'), goose = join(root, 'goose');
  await copyFile(process.env.ANCHOR_TEST_GOOSE_HOST_BINARY!, binary, constants.COPYFILE_FICLONE);
  await copyFile(process.env.ANCHOR_GOOSE_BINARY!, goose, constants.COPYFILE_FICLONE);
  const gooseDigest = createHash('sha256').update(await readFile(goose)).digest('hex');
  await mkdir(join(root, 'bundle'));
  await writeFile(join(root, 'bundle/graph.json'), JSON.stringify({ entry: 'seed', ops: { seed: { run: 'true' } }, nodes: [{ id: 'seed', op: 'seed' }], edges: [] }));
  await writeFile(join(root, 'bundle/manifest.json'), JSON.stringify({ format: 1, graph: 'graph.json', plugins: [] }));
  const calls: Record<string, unknown>[] = [], failures: string[] = [];
  const provider = createServer(async (incoming, response) => {
    if (incoming.method === 'GET' && incoming.url === '/v1/models') {
      response.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify({ object: 'list', data: [{ id: 'fixture-browser', object: 'model', created: 1, owned_by: 'fixture' }] }));
      return;
    }
    try {
      let raw = '';
      for await (const chunk of incoming) raw += chunk.toString();
      const body = JSON.parse(raw);
      calls.push(body);
      const position = calls.length;
      if (incoming.url !== '/v1/chat/completions' || position > 6) throw new Error('unexpected provider request');
      if (position === 2 && !JSON.stringify(body.messages).includes('browser-answer-token')) throw new Error('native answer missing from model feedback');
      if (position === 4 && !JSON.stringify(body.messages).includes('deleted')) throw new Error('declined deletion feedback missing');
      if (position === 6 && !JSON.stringify(body.messages).includes('deleted')) throw new Error('confirmed deletion feedback missing');
      const tool = position % 2 === 1;
      const suffix = position === 1 ? 'ask_user' : 'graph_delete';
      const name = tool ? body.tools.find((entry: { function: { name: string } }) => entry.function.name.endsWith(`__${suffix}`))?.function.name : undefined;
      if (tool && !name) throw new Error(`missing authorized ${suffix}`);
      const arguments_ = position === 1 ? { message: '请提供浏览器测试 token', requested_schema: { type: 'object', properties: { token: { type: 'string', title: '测试 token' } }, required: ['token'] } } : { graph: 'browser-fixture' };
      const delta = tool ? { role: 'assistant', tool_calls: [{ index: 0, id: `browser-${position}`, type: 'function', function: { name, arguments: JSON.stringify(arguments_) } }] }
        : { role: 'assistant', content: position === 2 ? '已收到 browser-answer-token' : position === 4 ? '已拒绝删除，Graph 保留' : '已确认删除隔离 Graph' };
      const common = { id: `fixture-${position}`, object: 'chat.completion.chunk', created: 1, model: 'fixture-browser' };
      response.writeHead(200, { 'Content-Type': 'text/event-stream' }).end(`data: ${JSON.stringify({ ...common, choices: [{ index: 0, finish_reason: null, delta }] })}\n\ndata: ${JSON.stringify({ ...common, choices: [{ index: 0, finish_reason: tool ? 'tool_calls' : 'stop', delta: {} }], usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } })}\n\ndata: [DONE]\n\n`);
    } catch (error) {
      failures.push((error as Error).message);
      response.writeHead(400).end('{}');
    }
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const address = provider.address();
  if (!address || typeof address === 'string') throw new Error('missing provider port');
  const port = await freePort(), base = `http://127.0.0.1:${port}`;
  let backend: ChildProcess | undefined, logs = '';
  try {
    backend = spawn(binary, ['serve'], { cwd: root, env: {
      PATH: '/usr/bin:/bin', LANG: 'C.UTF-8', TZ: 'UTC', HOME: join(root, 'home'),
      ANCHOR_RUNNER_BUNDLE_ROOT: join(root, 'bundle'), ANCHOR_RUNNER_CATALOG_ROOT: root,
      ANCHOR_RUNNER_STATE_ROOT: join(root, 'state'), ANCHOR_RUNNER_WORKSPACE_ROOT: join(root, 'work'),
      ANCHOR_RUNNER_GRAPH_NAME: 'browser-fixture', ANCHOR_RUNNER_LISTEN: `127.0.0.1:${port}`,
      ANCHOR_RUNNER_WEB_ROOT: fileURLToPath(new URL('../dist', import.meta.url)),
      ANCHOR_RUNNER_SCHEDULES_PATH: join(root, 'schedules.json'), ANCHOR_RUNNER_ALLOWED_COMMANDS: 'true,sh,cat,git',
      ANCHOR_MODEL_API_KEY: 'fixture-only-not-a-secret', ANCHOR_MODEL_URL: `http://127.0.0.1:${address.port}/v1`,
      ANCHOR_MODEL_NAME: 'fixture-browser', ANCHOR_MODEL_WIRE_API: 'chat',
      ANCHOR_GOOSE_BINARY: goose, ANCHOR_GOOSE_ALLOW_SHARED_NETWORK: '1',
      ANCHOR_GOOSE_BINARY_SHA256: gooseDigest,
    } });
    backend.stdout!.on('data', chunk => { logs += chunk.toString(); });
    backend.stderr!.on('data', chunk => { logs += chunk.toString(); });
    await expect.poll(async () => {
      if (backend?.exitCode !== null) throw new Error(logs);
      try { return (await request.get(`${base}/health`)).status(); } catch { return 0; }
    }).toBe(200);
    await page.goto(base);
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    const composer = page.getByLabel('发送给 Anchor Pilot');
    await composer.fill('请用原生表单询问测试 token。');
    await page.getByRole('button', { name: '发送', exact: true }).click();
    const question = page.getByRole('region', { name: 'Pilot 提问' }).last();
    await expect(question.getByLabel('测试 token')).toBeVisible();
    await expect(composer).toBeDisabled();
    await page.reload();
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    await expect(question.getByLabel('测试 token')).toBeVisible();
    await question.getByLabel('测试 token').fill('browser-answer-token');
    await question.getByRole('button', { name: '回答', exact: true }).click();
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('已收到 browser-answer-token');
    await expect(composer).toBeEnabled();
    await composer.fill('请删除 browser-fixture。');
    await page.getByRole('button', { name: '发送', exact: true }).click();
    await expect(question).toContainText('graph-delete-v1:');
    await question.getByRole('button', { name: '拒绝', exact: true }).click();
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('已拒绝删除');
    expect((await request.get(`${base}/graphs/browser-fixture`)).status()).toBe(200);
    await expect(composer).toBeEnabled();
    await composer.fill('现在请再次删除 browser-fixture，并重新确认。');
    await page.getByRole('button', { name: '发送', exact: true }).click();
    const confirmation = question.getByLabel('确认删除此 Graph 及其运行数据', { exact: false });
    await expect(confirmation).toHaveValue('');
    await confirmation.selectOption('true');
    await question.getByRole('button', { name: '回答', exact: true }).click();
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('已确认删除隔离 Graph');
    await expect(composer).toBeEnabled();
    expect((await request.get(`${base}/graphs/browser-fixture`)).status()).toBe(404);
    const sessions = (await (await request.get(`${base}/sessions`)).json()).sessions;
    expect(sessions).toHaveLength(1);
    const session = sessions[0].id;
    const turns = (await (await request.get(`${base}/sessions/${session}/turns`)).json()).turns;
    expect(turns).toHaveLength(3);
    expect(turns.every((turn: { status: string }) => turn.status === 'completed')).toBe(true);
    expect(new Set(turns.map((turn: { goose: { session: string } }) => turn.goose.session)).size).toBe(1);
    const saved = await Promise.all(turns.map(async (turn: { id: string }) => (await (await request.get(`${base}/sessions/${session}/turns/${turn.id}/questions`)).json()).questions));
    expect(saved.flat()).toHaveLength(3);
    expect(saved.flat().every(question => question.status === 'answered')).toBe(true);
    expect(calls).toHaveLength(6);
    expect(failures).toEqual([]);
    const evidence = { status: 'passed', runtime: 'goose', real_model_requests: 0, production_data_used: false,
      python: false, refresh_pending: true, no_extra_turns: true, graph_deleted: true, turns, questions: saved, provider_requests: calls };
    await writeFile(join(root, 'evidence.json'), JSON.stringify(evidence, null, 2));
    await page.screenshot({ path: test.info().outputPath('goose-elicitation.png') });
    await writeFile(test.info().outputPath('evidence.json'), await readFile(join(root, 'evidence.json')));
    console.log(`Goose browser evidence: ${join(root, 'evidence.json')}`);
  } finally {
    await stop(backend);
    await new Promise<void>(resolve => provider.close(() => resolve()));
    await writeFile(join(root, 'host-log.txt'), logs);
  }
});
