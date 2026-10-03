import { expect, test } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
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

async function stop(child?: ChildProcess) {
  if (!child || child.exitCode !== null) return;
  const exited = new Promise<void>(resolve => child.once('exit', () => resolve()));
  child.kill('SIGINT');
  await exited;
}

test('Rust-hosted React workbench runs a protected Graph and inspects its committed file', async ({ page, request }) => {
  test.setTimeout(60000);
  const repo = fileURLToPath(new URL('../../../', import.meta.url));
  const root = await mkdtemp(join(repo, '.local/rust-web-'));
  const bundle = join(root, 'bundle');
  await mkdir(bundle, { recursive: true });
  const graph = {
    objective: 'Rust host browser slice', entry: 'build',
    ops: { build: { run: "sh -c 'printf rust-native > result.txt'" } },
    nodes: [{ id: 'build', op: 'build', plugins: [] }], edges: [],
  };
  await writeFile(join(bundle, 'graph.json'), JSON.stringify(graph));
  await writeFile(join(bundle, 'manifest.json'), JSON.stringify({ format: 1, graph: 'graph.json', plugins: [] }));
  const apiPort = await freePort();
  const api = `http://127.0.0.1:${apiPort}`;
  const binary = join(repo, 'rust/target/debug/anchor-runner-host');
  let backend: ChildProcess | undefined, logs = '';
  try {
    backend = spawn(binary, ['serve'], { cwd: repo, stdio: ['ignore', 'pipe', 'pipe'], env: {
      ...process.env,
      ANCHOR_RUNNER_BUNDLE_ROOT: bundle,
      ANCHOR_RUNNER_CATALOG_ROOT: root,
      ANCHOR_RUNNER_GRAPH_NAME: 'rust-ui',
      ANCHOR_RUNNER_STATE_ROOT: join(root, 'state'),
      ANCHOR_RUNNER_WORKSPACE_ROOT: join(root, 'workspaces'),
      ANCHOR_RUNNER_LISTEN: `0.0.0.0:${apiPort}`,
      ANCHOR_RUNNER_ALLOWED_COMMANDS: 'sh,printf,cat',
      ANCHOR_API_KEYS: 'rust-browser-test-key',
    } });
    backend.stdout?.on('data', data => { logs += data.toString(); });
    backend.stderr?.on('data', data => { logs += data.toString(); });
    await expect(async () => {
      const response = await request.get(`${api}/`);
      expect(response.ok(), `${response.status()} ${await response.text()}\n${logs}`).toBeTruthy();
    }).toPass({ timeout: 20000 });
    const unauthorized = await request.get(`${api}/graphs`);
    expect(unauthorized.status()).toBe(401);
    await page.goto(api);
    await expect(page.getByRole('dialog', { name: '连接 Anchor 服务' })).toBeVisible();
    await page.getByLabel('Anchor API key').fill('rust-browser-test-key');
    await page.getByRole('button', { name: '连接', exact: true }).click();
    await expect(page.locator('[data-id="build"]')).toBeVisible();
    page.on('dialog', dialog => dialog.accept('{}'));
    await page.getByRole('button', { name: '运行工作流', exact: true }).click();
    await expect(page.locator('.execution-node.state-completed')).toHaveCount(1, { timeout: 20000 });
    await page.locator('[data-id="build"]').click();
    await page.getByRole('button', { name: '文件' }).click();
    await page.getByRole('button', { name: /result\.txt/ }).click();
    await expect(page.locator('.file-preview')).toHaveText('rust-native');
    const headers = { Authorization: 'Bearer rust-browser-test-key' };
    const run = await (await request.get(`${api}/runs`, { headers })).json();
    expect(run.runs).toHaveLength(1);
    const runId = run.runs[0].run as string;
    const detail = await (await request.get(`${api}/runs/${runId}`, { headers })).json();
    expect(detail.state.nodes.build.files).toContain('result.txt');
    const artifact = await (await request.get(`${api}/runs/${runId}/files/build/result.txt`, { headers })).json();
    expect(artifact.text).toBe('rust-native');
    await page.getByRole('button', { name: '运行看板' }).click();
    await page.getByRole('button', { name: /rust-ui，已完成/ }).click();
    await expect(page.getByText('暂无记录').first()).toBeVisible();
    await expect(page.getByText('0 秒')).toHaveCount(0);
    await expect(page.getByText('Invalid Date')).toHaveCount(0);
    await expect(page.getByText('服务在线', { exact: true })).toBeVisible();
  } finally {
    await writeFile(join(root, 'server.log'), logs);
    await stop(backend);
    console.log(`Rust web acceptance fixture: ${root}`);
  }
});
