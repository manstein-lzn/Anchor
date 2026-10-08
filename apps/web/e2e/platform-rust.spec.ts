import { expect, test } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { freePort, nativeHost, repo, stop } from './fixtures/native-host';

test('native Rust Host owns manual and scheduled Runs, restart recovery, files and control', async ({ page, request }) => {
  test.setTimeout(120000);
  const root = await mkdtemp(join(tmpdir(), 'anchor-platform-rust-'));
  const graph = { objective: 'Native platform browser acceptance', entry: 'first',
    ops: { first: { run: 'printf first > first.txt; sleep 8' }, last: { run: 'cat /in/first/first.txt > result.txt' } },
    nodes: [{ id: 'first', op: 'first' }, { id: 'last', op: 'last' }], edges: [{ from: 'first', to: 'last' }] };
  const host = await nativeHost(root, { graphName: 'work', definition: graph });
  const webPort = await freePort(), base = `http://127.0.0.1:${webPort}`;
  let backend: ChildProcess | undefined, frontend: ChildProcess | undefined;
  const ready = async () => expect(async () => {
    expect((await request.get(`${base}/graphs`)).ok(), host.logs).toBeTruthy();
  }).toPass({ timeout: 20000 });
  try {
    backend = host.start();
    frontend = spawn(join(repo, 'apps/web/node_modules/.bin/vite'), ['--host', '127.0.0.1', '--port', String(webPort), '--strictPort'],
      { cwd: join(repo, 'apps/web'), stdio: 'ignore', env: { ...process.env, ANCHOR_WEB_API_URL: host.base } });
    await ready();
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
    await stop(backend); backend = host.start(); await ready(); await page.reload();
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
    await page.screenshot({ path: join(root, 'manual-file.png') });

    const at = new Date(Date.now() + 6000).toISOString().slice(0, 19);
    const scheduled = await request.post(`${base}/schedules`, { data: { graph: 'work', rule: { type: 'once', at } } });
    expect(scheduled.ok(), await scheduled.text()).toBeTruthy();
    const storedSchedules = host.environment.ANCHOR_RUNNER_SCHEDULES_PATH!;
    expect(JSON.parse(await readFile(storedSchedules, 'utf8'))[0].graph).toBe('work');
    await expect.poll(async () => (await (await request.get(`${base}/runs`)).json()).runs.length, { timeout: 20000 }).toBe(2);
    let scheduledRun: Record<string, unknown> | undefined;
    await expect(async () => {
      const runs = (await (await request.get(`${base}/runs`)).json()).runs;
      scheduledRun = runs.find((run: { trigger?: { source: string } }) => run.trigger?.source === 'schedule');
      expect(scheduledRun?.status).toBe('completed');
    }).toPass({ timeout: 15000 });
    expect(scheduledRun!.trigger).toMatchObject({ source: 'schedule', scheduled_at: at });
    expect((await (await request.get(`${base}/timeline`)).json()).capabilities.scheduling).toBe(true);
    const downtimeAt = new Date(Date.now() + 4000).toISOString().slice(0, 19);
    const downtimeSchedule = await request.post(`${base}/schedules`, {
      data: { graph: 'work', rule: { type: 'once', at: downtimeAt } },
    });
    expect(downtimeSchedule.ok(), await downtimeSchedule.text()).toBeTruthy();
    const downtimeId = (await downtimeSchedule.json()).schedule.id as string;
    const beforeDowntime = (await (await request.get(`${base}/runs`)).json()).runs.length;
    await stop(backend);
    await page.waitForTimeout(5000);
    backend = host.start(); await ready();
    await expect.poll(async () => JSON.parse(await readFile(storedSchedules, 'utf8'))
      .find((item: { id: string }) => item.id === downtimeId)?.enabled,
    { timeout: 10000 }).toBe(false);
    expect((await (await request.get(`${base}/runs`)).json()).runs).toHaveLength(beforeDowntime);
    const recoveredTimeline = (await (await request.get(`${base}/timeline`)).json()).scheduled;
    expect(recoveredTimeline.find((item: { schedule: string }) => item.schedule === downtimeId)?.status).toBe('missed_downtime');
    await stop(backend);
    expect((await request.post(`${base}/trigger`, { data: { graph: 'work' } })).status()).toBeGreaterThanOrEqual(500);
    await writeFile(join(root, 'evidence.json'), JSON.stringify({ status: 'passed', manual_run: runId,
      scheduled_run: scheduledRun!.run, source: 'native Rust Host + Bubblewrap + browser', real_model_calls: 0,
      scope: 'Manual/scheduled admission, schedule CRUD/tick, UTC timeline, downtime restart skip, pause/resume and files/download' }, null, 2));
  } finally {
    await stop(frontend); await stop(backend);
    await writeFile(join(root, 'host.log'), host.logs);
    console.log(`Native platform browser fixture: ${root}`);
  }
});
