import { expect, test } from '@playwright/test';
import { spawn, execFileSync, type ChildProcess } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, symlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:net';

/** Uses the real API and sandbox, isolated from the developer's running research. */
test('Plugin selection, lazy instructions and historical records work with a real library', async ({ page, request }) => {
  test.setTimeout(60000);
  const repo = fileURLToPath(new URL('../../../', import.meta.url));
  const root = await mkdtemp(join(tmpdir(), 'anchor-plugin-browser-'));
  const config = join(root, 'runtime.json');
  const probe = createServer();
  await new Promise<void>(resolve => probe.listen(0, '127.0.0.1', resolve));
  const address = probe.address();
  if (!address || typeof address === 'string') throw new Error('no test port');
  const port = address.port;
  await new Promise<void>(resolve => probe.close(() => resolve()));
  const base = `http://127.0.0.1:${port}`;
  let server: ChildProcess | undefined;
  try {
    await mkdir(join(root, 'library/plugins'), { recursive: true });
    await mkdir(join(root, 'library/tools/scholarly'), { recursive: true });
    await symlink(join(repo, 'plugins/academic-research'), join(root, 'library/plugins/academic-research'));
    await writeFile(join(root, 'library/tools/scholarly/tool.json'), JSON.stringify({
      entrypoint: join(repo, '.venv/bin/anchor-scholarly'), environment: join(repo, '.venv'), imports: [join(repo, 'src')],
    }));
    await writeFile(config, JSON.stringify({ models: [] }));
    server = spawn(join(repo, '.venv/bin/python'), ['-m', 'anchor', '--root', root,
      '--config', config, '--host', '127.0.0.1', '--port', String(port)], { cwd: repo, stdio: 'ignore' });
    await expect(async () => expect((await request.get(`${base}/plugins`)).ok()).toBeTruthy()).toPass({ timeout: 10000 });
    const response = await request.post(`${base}/graphs`, { data: { name: 'plugin-proof', definition: {
      entry: 'research', agents: { worker: { model: 'test', instructions: 'Use Plugin resources.' } },
      nodes: [{ id: 'research', agent: 'worker' }], edges: [],
    } } });
    expect(response.ok()).toBeTruthy();
    await page.goto(base);
    await page.locator('.react-flow__node').filter({ hasText: 'research' }).click();
    const checkbox = page.getByRole('checkbox', { name: '学术调研' });
    await checkbox.check();
    await page.getByRole('button', { name: '查看说明', exact: true }).click();
    await expect(page.locator('.plugin-instructions').getByRole('heading', { name: '学术调研', exact: true })).toHaveCount(2);
    await expect(page.locator('.plugin-instructions')).toContainText('/tools/scholarly/run');
    expect((await request.get(`${base}/plugins/academic-research/files/instructions.md`)).ok()).toBeTruthy();
    await page.getByRole('button', { name: '保存', exact: true }).click();
    await expect(async () => {
      const graph = await (await request.get(`${base}/graphs/plugin-proof`)).json();
      expect(graph.definition.nodes[0].plugins).toEqual(['academic-research']);
    }).toPass();
    await expect(page.getByLabel('挂载 1 个 Plugin')).toBeVisible();
    await page.screenshot({ path: test.info().outputPath('plugin-editor.png'), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(checkbox).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy();
    await page.setViewportSize({ width: 1440, height: 1000 });

    // Script only replaces the model; CLI, mounts, scholarly executable and Git are real.
    const script = join(root, 'model.json');
    await writeFile(script, JSON.stringify({ research: [
      'cat /plugins/academic-research/instructions.md > notes.md',
      '/tools/scholarly/run --help > tool-help.txt',
      'anchor-done --summary "Plugin and shared tool verified"',
    ] }));
    execFileSync(join(repo, '.venv/bin/python'), ['-m', 'anchor.simple', join(root, 'workspaces/plugin-proof'),
      '--config', config], { cwd: repo, env: { ...process.env, ANCHOR_MODEL_SCRIPT: script }, timeout: 30000 });
    const runs = await (await request.get(`${base}/runs`)).json();
    const run = runs.runs[0];
    expect(run.status).toBe('finished');
    const detail = await (await request.get(`${base}/runs/${run.run}`)).json();
    expect(detail.plugins.research[0].id).toBe('academic-research');
    await page.reload();
    await page.getByRole('button', { name: '运行记录', exact: true }).click();
    await page.locator('.react-flow__node').filter({ hasText: 'research' }).click();
    await page.getByRole('button', { name: 'Plugin', exact: true }).click();
    await expect(page.locator('.plugins-panel')).toContainText('学术调研');
    await expect(page.locator('.plugins-panel')).toContainText('内容摘要');
    await page.screenshot({ path: test.info().outputPath('plugin-run.png'), fullPage: true });
  } finally {
    if (server && server.exitCode === null) {
      const exited = new Promise<void>(resolve => server!.once('exit', () => resolve()));
      server.kill();
      await exited;
    }
    await rm(root, { recursive: true, force: true });
  }
});
