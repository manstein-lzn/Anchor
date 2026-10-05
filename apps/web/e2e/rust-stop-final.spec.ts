import { expect, test } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { createServer as httpServer, type ServerResponse } from 'node:http';
import { createServer } from 'node:net';
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

async function freePort() {
  const server = createServer();
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('missing port');
  await new Promise<void>(resolve => server.close(() => resolve()));
  return address.port;
}

test('stop during the final model response settles visibly and resumes without replay', async ({ page, request }) => {
  test.setTimeout(90000);
  const repo = fileURLToPath(new URL('../../../', import.meta.url));
  const root = await mkdtemp(join(repo, '.local/rust-stop-final-'));
  const bundle = join(root, 'bundle');
  await mkdir(bundle);
  const graph = {
    objective: 'Stop at the final provider response', entry: 'answer',
    agents: { worker: { model: 'models.default', instructions: 'Return a short completion.' } },
    ops: { next: { run: 'printf downstream > marker.txt' } },
    nodes: [{ id: 'answer', agent: 'worker' }, { id: 'next', op: 'next' }],
    edges: [{ from: 'answer', to: 'next' }],
  };
  await writeFile(join(bundle, 'graph.json'), JSON.stringify(graph));
  await writeFile(join(bundle, 'manifest.json'), JSON.stringify({ format: 1, graph: 'graph.json', plugins: [] }));
  let pending: ServerResponse | undefined;
  const exchanges: unknown[] = [];
  const provider = httpServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(Buffer.from(chunk));
    exchanges.push(JSON.parse(Buffer.concat(chunks).toString()));
    pending = res;
  });
  await new Promise<void>(resolve => provider.listen(0, '127.0.0.1', resolve));
  const providerAddress = provider.address();
  if (!providerAddress || typeof providerAddress === 'string') throw new Error('missing provider port');
  const port = await freePort();
  const base = `http://127.0.0.1:${port}`;
  let backend: ChildProcess | undefined, logs = '';
  try {
    backend = spawn(process.env.ANCHOR_TEST_RUNTIME_BINARY || join(repo, 'rust/target/release/anchor-runner-host'), ['serve'], {
      cwd: repo, stdio: ['ignore', 'pipe', 'pipe'], env: {
        PATH: '/usr/bin:/bin',
        ANCHOR_MODEL_URL: `http://127.0.0.1:${providerAddress.port}/v1`,
        ANCHOR_MODEL_API_KEY: 'fixture', ANCHOR_MODEL_NAME: 'fixture', ANCHOR_MODEL_WIRE_API: 'chat',
        ANCHOR_RUNNER_BUNDLE_ROOT: bundle, ANCHOR_RUNNER_CATALOG_ROOT: root,
        ANCHOR_RUNNER_GRAPH_NAME: 'stop-final', ANCHOR_RUNNER_STATE_ROOT: join(root, 'state'),
        ANCHOR_RUNNER_WORKSPACE_ROOT: join(root, 'workspaces'),
        ANCHOR_RUNNER_LISTEN: `127.0.0.1:${port}`, ANCHOR_RUNNER_ALLOWED_COMMANDS: 'sh',
      },
    });
    backend.stdout?.on('data', value => { logs += value.toString(); });
    backend.stderr?.on('data', value => { logs += value.toString(); });
    await expect(async () => { expect((await request.get(`${base}/health`)).ok(), logs).toBeTruthy(); }).toPass();
    await page.goto(base);
    await expect(page.locator('[data-id="answer"]')).toBeVisible();
    page.on('dialog', dialog => dialog.accept('{}'));
    await page.getByRole('button', { name: '运行工作流', exact: true }).click();
    await expect.poll(() => exchanges.length, { timeout: 30000 }).toBe(1);
    await page.getByRole('button', { name: '停止', exact: true }).click();
    await expect(page.locator('.canvas-head .pill')).toHaveText('正在停止');
    await expect(page.getByText('请求已接收，等待当前节点收束。')).toBeVisible();
    await page.screenshot({ path: join(root, 'stopping.png') });
    const listed = await (await request.get(`${base}/runs`)).json();
    const runId = listed.runs[0].run;
    const detail = await (await request.get(`${base}/runs/${runId}`)).json();
    expect(detail.control_requested).toBe('stop');
    expect(detail.state.nodes.next).toBeUndefined();
    pending!.writeHead(200, { 'Content-Type': 'application/json' });
    pending!.end(JSON.stringify({
      id: 'fixture-final', object: 'chat.completion', created: 1, model: 'fixture',
      choices: [{ index: 0, finish_reason: 'tool_calls', message: { role: 'assistant', content: null,
        tool_calls: [{ id: 'final-1', type: 'function', function: { name: 'final_result',
          arguments: JSON.stringify({ summary: 'Completed the final response.' }) } }] } }],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
    }));
    const runPath = join(root, 'state/runs', `${runId}.json`);
    await expect.poll(async () => JSON.parse(await readFile(runPath, 'utf8')).status).toBe('stopped');
    const stopped = JSON.parse(await readFile(runPath, 'utf8'));
    expect(stopped.results.answer).toHaveLength(1);
    expect(stopped.results.next).toBeUndefined();
    await expect(page.getByRole('button', { name: '继续', exact: true })).toBeVisible();
    await page.screenshot({ path: join(root, 'stopped.png') });
    await page.getByRole('button', { name: '继续', exact: true }).click();
    await expect.poll(async () => JSON.parse(await readFile(runPath, 'utf8')).status).toBe('completed');
    const completed = await readFile(runPath, 'utf8');
    const record = JSON.parse(completed);
    expect(record.results.answer).toEqual(stopped.results.answer);
    expect(record.results.next).toHaveLength(1);
    expect(exchanges).toHaveLength(1);
    expect(await readFile(join(root, 'state/artifacts', record.results.next[0].commit.id, 'files/marker.txt'), 'utf8')).toBe('downstream');
    // A stop after completion must not turn the terminal Run back into stopped.
    await expect.poll(async () => (await (await request.get(`${base}/runs/${runId}`)).json()).active).toBe(false);
    expect((await request.post(`${base}/runs/${runId}/stop`, { data: {} })).status()).toBe(409);
    expect(await readFile(runPath, 'utf8')).toBe(completed);
    await page.screenshot({ path: join(root, 'completed.png') });
    await writeFile(join(root, 'evidence.json'), JSON.stringify({ status: 'passed', run: runId,
      provider: 'controlled delayed HTTP fixture', provider_requests: exchanges.length,
      scope: 'Real Rust host and browser; last response stop, no downstream before resume, no completed request replay' }, null, 2));
  } finally {
    pending?.destroy();
    if (backend && backend.exitCode === null) {
      const exited = new Promise<void>(resolve => backend!.once('exit', () => resolve()));
      backend.kill('SIGINT');
      await exited;
    }
    await new Promise<void>(resolve => provider.close(() => resolve()));
    await writeFile(join(root, 'host.log'), logs);
    await writeFile(join(root, 'provider-requests.json'), JSON.stringify(exchanges, null, 2));
    console.log(`Stop final acceptance: ${root}`);
  }
});
