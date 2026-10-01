import { expect, test } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { OurGraph, OurRunDetail } from '../src/model';

async function port() {
  const server = createServer();
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('missing port');
  await new Promise<void>(resolve => server.close(() => resolve()));
  return address.port;
}
async function stop(child: ChildProcess | undefined) {
  if (!child || child.exitCode !== null) return;
  const exited = new Promise<void>(resolve => child.once('exit', () => resolve()));
  child.kill('SIGINT');
  await exited;
}

test('real backend: paired authoring, simultaneous native nodes, joined artifacts and invalid-pair rejection', async ({ page, request }) => {
  test.setTimeout(90000);
  const repo = fileURLToPath(new URL('../../../', import.meta.url));
  const root = await mkdtemp(join(repo, '.local/parallel-browser-'));
  const apiPort = await port(), webPort = await port();
  const api = `http://127.0.0.1:${apiPort}`, base = `http://127.0.0.1:${webPort}`;
  let backend: ChildProcess | undefined, frontend: ChildProcess | undefined;
  let logs = '';
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    const config = join(root, 'runtime.json');
    await writeFile(config, '{"models":[]}');
    backend = spawn(join(repo, '.venv/bin/python'), ['-m', 'anchor', '--root', root, '--config', config,
      '--host', '127.0.0.1', '--port', String(apiPort)], { cwd: repo, stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, ANCHOR_API_KEYS: '', ANCHOR_API_KEY: '' } });
    backend.stdout?.on('data', data => { logs += data.toString(); });
    backend.stderr?.on('data', data => { logs += data.toString(); });
    frontend = spawn(join(repo, 'apps/web/node_modules/.bin/vite'), ['--host', '127.0.0.1', '--port', String(webPort)],
      { cwd: join(repo, 'apps/web'), stdio: 'ignore', env: { ...process.env, ANCHOR_WEB_API_URL: api } });
    await expect(async () => { expect((await request.get(`${base}/graphs`)).ok()).toBeTruthy(); }).toPass({ timeout: 15000 });
    const created = await request.post(`${base}/graphs`, { data: { name: 'parallel-ui', definition: {
      entry: 'finish', nodes: [{ id: 'finish', op: 'finish' }], edges: [],
      ops: { finish: { run: 'cat /in/branch-a/result.txt /in/branch-b/result.txt > combined.txt' } },
    } } });
    expect(created.ok(), await created.text()).toBeTruthy();
    await page.goto(base);
    await page.getByRole('button', { name: '添加节点', exact: true }).click();
    await page.getByRole('button', { name: '并行分支 在入口前' }).click();
    await expect(page.getByLabel('配对收束节点')).toHaveValue('join');
    await page.getByRole('button', { name: '保存', exact: true }).click();
    await expect(page.getByText('已保存。', { exact: true })).toBeVisible();
    const graph: OurGraph = (await (await request.get(`${base}/graphs/parallel-ui`)).json()).definition;
    expect(graph.ops?.fanout.fanout?.join).toBe('join');
    // Native sandbox command branches make the overlap observable without a model fixture.
    for (const node of graph.nodes.filter(node => node.agent)) {
      delete node.agent; node.op = node.id;
      graph.ops![node.id] = { run: `sleep 5; echo '${node.id}' > result.txt`, writes: ['result.txt'] };
    }
    const saved = await request.put(`${base}/graphs/parallel-ui`, { data: { definition: graph } });
    expect(saved.ok(), await saved.text()).toBeTruthy();
    const invalid = structuredClone(graph);
    invalid.ops!.fanout = { fanout: { join: 'absent' } };
    expect((await request.put(`${base}/graphs/parallel-ui`, { data: { definition: invalid } })).status()).toBe(400);
    await page.reload();
    page.on('dialog', dialog => dialog.accept('{}'));
    const triggered = page.waitForResponse(response => response.url().endsWith('/trigger') && response.request().method() === 'POST');
    await page.getByRole('button', { name: '运行工作流', exact: true }).click();
    const run = (await (await triggered).json()).run as string;
    await expect(page.locator('.execution-node.state-running')).toHaveCount(2);
    await expect(page.getByLabel('活动节点', { exact: true })).toContainText('branch-a');
    await expect(page.getByLabel('活动节点', { exact: true })).toContainText('branch-b');
    await expect(page.locator('[data-id="join"]')).toContainText('尚未执行');
    await page.screenshot({ path: join(root, 'parallel-active.png'), fullPage: true });
    await expect(async () => {
      const detail: OurRunDetail = await (await request.get(`${base}/runs/${run}`)).json();
      expect(detail.state.status).toBe('finished');
      expect(detail.state.active).toEqual({});
    }).toPass({ timeout: 20000 });
    const detail = await (await request.get(`${base}/runs/${run}`)).json();
    const manifest = await (await request.get(`${base}/runs/${run}/files/join/join.json`)).json();
    const result = await (await request.get(`${base}/runs/${run}/files/finish/combined.txt`)).json();
    expect(result.text).toBe('branch-a\nbranch-b\n');
    expect(JSON.parse(manifest.text).branches).toHaveLength(2);
    expect((await (await request.get(`${base}/runs`)).json()).runs).toHaveLength(1);
    await writeFile(join(root, 'evidence.json'), JSON.stringify({ detail, manifest, result, errors }, null, 2));
    await expect(page.locator('.execution-node.state-running')).toHaveCount(0);
    await expect(page.locator('.execution-node.state-completed')).toHaveCount(5);
    await page.screenshot({ path: join(root, 'parallel-finished.png'), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByRole('button', { name: '图编排', exact: true })).toBeVisible();
    await page.screenshot({ path: join(root, 'parallel-mobile.png'), fullPage: true });
    expect(errors).toEqual([]);
  } finally {
    await writeFile(join(root, 'server.log'), logs);
    await stop(frontend); await stop(backend);
    console.log(`Parallel browser evidence: ${root}`);
  }
});
