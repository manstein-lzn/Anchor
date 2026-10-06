import { expect, test } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { createServer } from 'node:net';
import { cp, mkdtemp, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
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
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise<void>(resolve => child.once('exit', () => resolve()));
  child.kill('SIGINT');
  await exited;
}

test('existing platform uses Rust for manual and scheduled Runs, files and control', async ({ page, request }) => {
  test.setTimeout(120000);
  const repo = fileURLToPath(new URL('../../../', import.meta.url));
  const root = await mkdtemp(join(repo, '.local/platform-rust-'));
  const platform = join(root, 'platform'), catalog = join(root, 'catalog'), bundle = join(catalog, 'work');
  await mkdir(bundle, { recursive: true });
  await mkdir(join(platform, 'library/plugins'), { recursive: true });
  await cp(join(repo, 'plugins/academic-research'), join(platform, 'library/plugins/academic-research'), { recursive: true });
  const config = join(root, 'runtime.json');
  await writeFile(config, '{"models":[]}');
  const graph = { objective: 'Existing platform, Rust execution', entry: 'first',
    ops: { first: { run: 'printf first > first.txt; sleep 8' }, last: { run: 'cat /in/first/first.txt > result.txt' } },
    nodes: [{ id: 'first', op: 'first' }, { id: 'last', op: 'last' }], edges: [{ from: 'first', to: 'last' }] };
  await writeFile(join(bundle, 'graph.json'), JSON.stringify(graph));
  await writeFile(join(bundle, 'manifest.json'), '{"format":1,"graph":"graph.json","plugins":[]}');
  const rustPort = await freePort(), platformPort = await freePort();
  const upstream = `http://127.0.0.1:${rustPort}`, base = `http://127.0.0.1:${platformPort}`;
  let rust: ChildProcess | undefined, web: ChildProcess | undefined, logs = '';
  const launch = (binary: string, args: string[], env: NodeJS.ProcessEnv) => {
    const process = spawn(binary, args, { cwd: repo, stdio: ['ignore', 'pipe', 'pipe'], env });
    process.stdout?.on('data', data => { logs += data.toString(); });
    process.stderr?.on('data', data => { logs += data.toString(); });
    return process;
  };
  const startRust = () => launch(join(repo, 'rust/target/release/anchor-runner-host'), ['serve'], {
    PATH: '/usr/bin:/bin', TZ: 'UTC',
    ANCHOR_RUNNER_BUNDLE_ROOT: bundle, ANCHOR_RUNNER_CATALOG_ROOT: catalog, ANCHOR_RUNNER_GRAPH_NAME: 'work',
    ANCHOR_RUNNER_STATE_ROOT: join(root, 'state'), ANCHOR_RUNNER_WORKSPACE_ROOT: join(root, 'workspaces'),
    ANCHOR_RUNNER_SCHEDULES_PATH: join(platform, 'state/schedules.json'),
    ANCHOR_RUNNER_LIBRARY_ROOT: join(platform, 'library'), ANCHOR_RUNNER_LISTEN: `127.0.0.1:${rustPort}`,
    ANCHOR_RUNNER_ALLOWED_COMMANDS: 'sh',
  });
  const startPlatform = () => launch(join(repo, '.venv/bin/python'), ['-m', 'anchor', '--root', platform,
    '--config', config, '--host', '127.0.0.1', '--port', String(platformPort)], {
    PATH: '/usr/bin:/bin', TZ: 'UTC', PYTHONPATH: join(repo, 'src'),
    ANCHOR_MODEL_URL: '', ANCHOR_MODEL_API_KEY: '', ANCHOR_MODEL_NAME: '',
    ANCHOR_API_KEYS: '', ANCHOR_API_KEY: '',
    ANCHOR_RUNTIME_BACKEND: 'rust', ANCHOR_RUNTIME_URL: upstream,
  });
  const ready = async () => expect(async () => {
    expect((await request.get(`${base}/graphs`)).ok(), logs).toBeTruthy();
  }).toPass({ timeout: 20000 });
  try {
    rust = startRust(); web = startPlatform(); await ready();
    await page.goto(base);
    await expect(page.locator('[data-id="first"]')).toBeVisible();
    page.on('dialog', dialog => dialog.accept('{}'));
    await page.getByRole('button', { name: '运行工作流', exact: true }).click();
    await expect(page.getByRole('button', { name: '暂停', exact: true })).toBeVisible();
    const pauseResponse = page.waitForResponse(response => response.url().endsWith('/pause'));
    await page.getByRole('button', { name: '暂停', exact: true }).click();
    const pause = await pauseResponse;
    expect(pause.status(), await pause.text()).toBe(202);
    await expect(page.locator('.canvas-head .pill')).toHaveText('已暂停', { timeout: 15000 });
    const runId = (await (await request.get(`${base}/runs`)).json()).runs[0].run;
    const runFile = join(root, 'state/runs', `${runId}.json`);
    const paused = JSON.parse(await readFile(runFile, 'utf8'));
    expect(paused.status).toBe('paused'); expect(paused.results.first).toHaveLength(1);
    expect((await request.post(`${base}/trigger`, { data: { graph: 'work' } })).status()).toBe(409);
    await stop(web); await stop(rust);
    rust = startRust(); web = startPlatform(); await ready(); await page.reload();
    await page.getByRole('button', { name: '图编排', exact: true }).click();
    await page.getByRole('button', { name: '查看最近运行', exact: true }).click();
    await expect(page.getByRole('button', { name: '继续', exact: true })).toBeVisible();
    await page.getByRole('button', { name: '继续', exact: true }).click();
    await expect.poll(async () => JSON.parse(await readFile(runFile, 'utf8')).status, { timeout: 15000 }).toBe('completed');
    const final = JSON.parse(await readFile(runFile, 'utf8'));
    expect(final.results.first).toEqual(paused.results.first);
    const download = await request.get(`${base}/runs/${runId}/files/last/result.txt?download=1`);
    expect(download.ok()).toBeTruthy(); expect(await download.text()).toBe('first');
    expect(download.headers()['content-disposition']).toContain('attachment');
    await page.locator('[data-id="last"]').click();
    await page.getByRole('button', { name: '文件', exact: true }).click();
    await page.getByRole('button', { name: /result\.txt/ }).click();
    await expect(page.locator('.file-preview')).toHaveText('first');
    await expect(page.locator('.canvas-head .pill')).toHaveText('已完成');
    await page.screenshot({ path: join(root, 'manual-file.png') });
    // Python retains scheduling; the due time is sent to the same Rust admission.
    const at = new Date(Date.now() + 6000).toISOString().slice(0, 19);
    const scheduled = await request.post(`${base}/schedules`, { data: { graph: 'work', rule: { type: 'once', at } } });
    expect(scheduled.ok(), await scheduled.text()).toBeTruthy();
    const storedSchedules = join(platform, 'state/schedules.json');
    expect(JSON.parse(await readFile(storedSchedules, 'utf8'))[0].graph).toBe('work');
    await expect.poll(async () => (await (await request.get(`${base}/runs`)).json()).runs.length, { timeout: 20000 }).toBe(2);
    let scheduledRun: Record<string, unknown> | undefined;
    await expect(async () => {
      const runs = (await (await request.get(`${base}/runs`)).json()).runs;
      scheduledRun = runs.find((run: { trigger?: { source: string } }) => run.trigger?.source === 'schedule');
      expect(scheduledRun?.status).toBe('completed');
    }).toPass({ timeout: 15000 });
    expect(scheduledRun!.trigger).toMatchObject({ source: 'schedule', scheduled_at: at });
    expect(scheduledRun!.updated).toBeTruthy();
    expect((await (await request.get(`${base}/timeline`)).json()).capabilities.scheduling).toBe(true);
    // Existing Library and editor remain the source of Plugin definitions.
    const pluginGraph = { entry: 'research', agents: { worker: { model: 'models.default', instructions: 'Read the Skill.' } },
      nodes: [{ id: 'research', agent: 'worker' }], edges: [] };
    const created = await request.post(`${base}/graphs`, { data: { name: 'plugin-proof', definition: pluginGraph } });
    expect(created.ok(), await created.text()).toBeTruthy();
    await page.getByRole('button', { name: '图编排', exact: true }).click();
    await page.reload();
    await page.getByRole('combobox', { name: '当前工作流' }).selectOption('plugin-proof');
    await page.locator('[data-id="research"]').click();
    const catalogResponse = await request.get(`${base}/plugins`);
    expect(catalogResponse.ok(), await catalogResponse.text()).toBeTruthy();
    expect((await catalogResponse.json()).plugins.find((item: { id: string }) => item.id === 'academic-research')
      ?.available).toBe(true);
    const skill = await request.get(`${base}/plugins/academic-research/files/skills/academic-research/SKILL.md`);
    expect(skill.ok(), await skill.text()).toBeTruthy();
    expect(await skill.text()).toContain('/tools/scholarly/run');
    expect(skill.headers()['x-content-type-options']).toBe('nosniff');
    await page.getByRole('checkbox', { name: '学术调研' }).check();
    await page.getByRole('button', { name: '查看说明', exact: true }).click();
    await expect(page.locator('.plugin-instructions')).toContainText('/tools/scholarly/run');
    await page.getByRole('button', { name: '保存', exact: true }).click();
    await expect(async () => {
      const definition = (await (await request.get(`${upstream}/graphs/plugin-proof`)).json()).definition;
      expect(definition.nodes[0].plugins).toEqual(['academic-research']);
    }).toPass();
    expect(await readFile(join(catalog, 'plugin-proof/plugins/academic-research/plugin.json'), 'utf8'))
      .toBe(await readFile(join(platform, 'library/plugins/academic-research/plugin.json'), 'utf8'));
    await page.screenshot({ path: join(root, 'plugin-editor.png') });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByRole('checkbox', { name: '学术调研' })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy();
    await page.screenshot({ path: join(root, 'plugin-editor-mobile.png'), fullPage: true });
    expect(await readdir(join(platform, 'workspaces')).catch(() => [])).toEqual([]);
    const downtimeAt = new Date(Date.now() + 4000).toISOString().slice(0, 19);
    const downtimeSchedule = await request.post(`${base}/schedules`, {
      data: { graph: 'work', rule: { type: 'once', at: downtimeAt } },
    });
    expect(downtimeSchedule.ok(), await downtimeSchedule.text()).toBeTruthy();
    const downtimeId = (await downtimeSchedule.json()).schedule.id as string;
    const beforeDowntime = (await (await request.get(`${base}/runs`)).json()).runs.length;
    await stop(rust);
    await page.waitForTimeout(5000);
    rust = startRust(); await ready();
    await expect.poll(async () => JSON.parse(await readFile(storedSchedules, 'utf8'))
      .find((item: { id: string }) => item.id === downtimeId)?.enabled,
    { timeout: 10000 }).toBe(false);
    expect((await (await request.get(`${base}/runs`)).json()).runs).toHaveLength(beforeDowntime);
    const recoveredTimeline = (await (await request.get(`${base}/timeline`)).json()).scheduled;
    expect(recoveredTimeline.find((item: { schedule: string }) =>
      item.schedule === downtimeId)?.status).toBe('missed_downtime');
    await stop(rust);
    expect((await request.post(`${base}/trigger`, { data: { graph: 'work' } })).status()).toBe(503);
    expect(await readdir(join(platform, 'workspaces')).catch(() => [])).toEqual([]);
    await writeFile(join(root, 'evidence.json'), JSON.stringify({ status: 'passed', manual_run: runId,
      scheduled_run: scheduledRun!.run, source: 'real Python platform + real Rust HTTP + Bubblewrap + browser',
      scope: 'Manual/scheduled admission, shared schedule CRUD/tick, local-time timeline, downtime restart skip, pause/resume, files/download, Library/editor Plugin binding and no fallback; no live model' }, null, 2));
  } finally {
    await stop(web); await stop(rust);
    await writeFile(join(root, 'services.log'), logs);
    console.log(`Platform Rust acceptance: ${root}`);
  }
});
