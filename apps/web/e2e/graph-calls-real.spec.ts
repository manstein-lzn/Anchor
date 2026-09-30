import { expect, test, type APIRequestContext } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { OurGraph, OurRunDetail } from '../src/model';

async function freePort() {
  const probe = createServer();
  await new Promise<void>(resolve => probe.listen(0, '127.0.0.1', resolve));
  const address = probe.address();
  if (!address || typeof address === 'string') throw new Error('No test port');
  await new Promise<void>(resolve => probe.close(() => resolve()));
  return address.port;
}
async function stop(child: ChildProcess | undefined) {
  if (!child || child.exitCode !== null) return;
  const exited = new Promise<void>(resolve => child.once('exit', () => resolve()));
  child.kill('SIGINT');
  await exited;
}
async function json(request: APIRequestContext, url: string) {
  const response = await request.get(url);
  expect(response.ok(), await response.text()).toBeTruthy();
  return response.json();
}

/** No HTTP mocks or model substitutions: browser, Vite proxy, API, command sandbox, Git and call
 * persistence are real. Uses no model provider or external channel and cleans up its own data root. */
test('real backend: author wait and detach calls, follow results and concurrent run chains', async ({ page, request }) => {
  test.setTimeout(100000);
  const repo = fileURLToPath(new URL('../../../', import.meta.url));
  const root = await mkdtemp(join(tmpdir(), 'anchor-call-browser-'));
  const apiPort = await freePort();
  const webPort = await freePort();
  const apiBase = `http://127.0.0.1:${apiPort}`;
  const base = `http://127.0.0.1:${webPort}`;
  let server: ChildProcess | undefined;
  let vite: ChildProcess | undefined;
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  page.setDefaultTimeout(12000);
  let serverLog = '';
  const evidence: Record<string, unknown> = { kind: 'real backend and browser, deterministic commands, no external provider or channel' };
  const target: OurGraph = { entry: 'work', nodes: [{ id: 'work', op: 'work' }], edges: [],
    input: { default: 'kept' }, ops: { work: { run: 'sleep 6; cat /in/call/report.txt > answer.txt', writes: ['answer.txt'] } } };
  const source: OurGraph = { entry: 'prepare', nodes: [{ id: 'prepare', op: 'prepare' }], edges: [],
    input: { request: { code: 'BROWSER-47' }, unrelated: 'not-forwarded' },
    ops: { prepare: { run: "printf 'browser report' > report.txt", writes: ['report.txt'] } } };
  try {
    const config = join(root, 'runtime.json');
    await writeFile(config, JSON.stringify({ models: [] }));
    server = spawn(join(repo, '.venv/bin/python'), ['-m', 'anchor', '--root', root, '--config', config,
      '--host', '127.0.0.1', '--port', String(apiPort)], {
      cwd: repo, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, ANCHOR_API_KEYS: '', ANCHOR_API_KEY: '' },
    });
    server.stdout?.on('data', data => { serverLog += data.toString(); });
    server.stderr?.on('data', data => { serverLog += data.toString(); });
    vite = spawn(join(repo, 'apps/web/node_modules/.bin/vite'), ['--host', '127.0.0.1', '--port', String(webPort)], {
      cwd: join(repo, 'apps/web'), stdio: 'ignore', env: { ...process.env, ANCHOR_WEB_API_URL: apiBase },
    });
    await expect(async () => { expect((await request.get(`${base}/graphs`)).ok()).toBeTruthy(); }).toPass({ timeout: 15000 });
    for (const [name, definition] of [['target', target], ['source', source]] as const) {
      const response = await request.post(`${base}/graphs`, { data: { name, definition } });
      expect(response.ok(), await response.text()).toBeTruthy();
    }
    const schedule = await request.post(`${base}/schedules`, { data: { graph: 'source', rule: { type: 'interval', seconds: 3600 } } });
    expect(schedule.ok(), await schedule.text()).toBeTruthy();
    await page.goto(base);
    await page.getByLabel('当前工作流').selectOption('source');
    await page.getByRole('button', { name: '添加节点', exact: true }).click();
    await page.getByRole('button', { name: '调用工作流 启动独立运行' }).click();
    const editor = page.getByRole('region', { name: '调用工作流设置' });
    await editor.getByLabel('目标工作流').selectOption('target');
    await editor.getByRole('button', { name: '编辑输入常量' }).click();
    await page.getByRole('textbox', { name: '输入常量' }).fill('{"constant":"browser"}');
    await page.getByRole('button', { name: '应用更改' }).click();
    await editor.getByRole('button', { name: '添加输入映射' }).click();
    await editor.getByLabel('来源 JSON Pointer').fill('/request/code');
    await editor.getByLabel('目标参数').fill('code');
    await editor.getByRole('button', { name: '添加文件', exact: true }).click();
    await editor.getByLabel('来源节点').selectOption('prepare');
    await editor.getByLabel('文件路径', { exact: true }).fill('report.txt');
    await editor.getByLabel('目标文件名').fill('report.txt');
    await editor.getByLabel('目标输出节点').selectOption('work');
    await editor.getByLabel('返回文件').fill('answer.txt');
    await page.getByRole('combobox', { name: '起点', exact: true }).selectOption('prepare');
    await page.getByRole('combobox', { name: '终点', exact: true }).selectOption('call');
    await page.getByRole('button', { name: '添加连线', exact: true }).click();
    await page.getByRole('button', { name: '保存', exact: true }).click();
    await expect(page.getByText('已保存。', { exact: true })).toBeVisible();
    const saved = await json(request, `${base}/graphs/source`);
    expect(saved.definition.ops.call.call).toEqual({ graph: 'target', mode: 'wait', input: { constant: 'browser' },
      input_map: { code: '/request/code' }, files: [{ node: 'prepare', path: 'report.txt', as: 'report.txt' }],
      result: { node: 'work', files: ['answer.txt'] } });
    evidence.waitDefinition = saved.definition;
    await page.locator('.inspector').evaluate(el => { el.scrollTop = 0; });
    await page.screenshot({ path: test.info().outputPath('real-call-editor.png'), fullPage: true });
    page.on('dialog', dialog => dialog.accept('{}'));
    const startWait = page.waitForResponse(response => response.url().endsWith('/trigger') && response.request().method() === 'POST');
    await page.getByRole('button', { name: '运行工作流', exact: true }).click();
    const waitRun = (await (await startWait).json()).run as string;
    await expect(async () => {
      const detail = await json(request, `${base}/runs/${waitRun}`);
      expect(detail.calls).toHaveLength(1);
      expect(detail.state.status).toBe('running');
      expect(detail.calls[0].status).toBe('running');
    }).toPass({ timeout: 10000 });
    await page.locator('[data-id="call"]').click();
    const records = page.getByRole('region', { name: '工作流调用记录' });
    await expect(records).toContainText('等待目标结果');
    await records.getByRole('button', { name: '查看目标运行' }).click();
    await expect(page.locator('.run-origin')).toContainText('source / call');
    await page.getByRole('button', { name: '返回来源运行' }).click();
    await expect(records).toContainText('本节点：已完成', { timeout: 20000 });
    const waitDetail: OurRunDetail = await json(request, `${base}/runs/${waitRun}`);
    expect(waitDetail.state.status).toBe('finished');
    const childDetail = await json(request, `${base}/runs/${waitDetail.calls![0].run}`);
    expect(childDetail.state.input).toEqual({ default: 'kept', constant: 'browser', code: 'BROWSER-47' });
    const returned = await json(request, `${base}/runs/${waitRun}/files/call/result/answer.txt`);
    expect(returned.text).toBe('browser report');
    evidence.waitRun = waitDetail; evidence.waitChild = childDetail; evidence.returnedFile = returned;
    await page.screenshot({ path: test.info().outputPath('real-call-wait-result.png'), fullPage: true });

    // A longer real command leaves enough time to inspect both independent children concurrently.
    target.ops!.work.run = 'sleep 18; cat /in/call/report.txt > answer.txt';
    const update = await request.put(`${base}/graphs/target`, { data: { definition: target } });
    expect(update.ok(), await update.text()).toBeTruthy();
    await page.getByRole('button', { name: '图编排', exact: true }).click();
    await page.locator('[data-id="call"]').click();
    await editor.getByLabel('执行模式').selectOption('detach');
    await page.getByRole('button', { name: '保存', exact: true }).click();
    await expect(page.getByText('已保存。', { exact: true })).toBeVisible();
    const detached: OurRunDetail[] = [];
    for (let index = 0; index < 2; index++) {
      const response = page.waitForResponse(response => response.url().endsWith('/trigger') && response.request().method() === 'POST');
      await page.getByRole('button', { name: '运行工作流', exact: true }).click();
      const run = (await (await response).json()).run as string;
      await expect(async () => {
        const detail = await json(request, `${base}/runs/${run}`);
        expect(detail.state.status).toBe('finished');
        expect(detail.calls).toHaveLength(1);
        expect(detail.calls[0].status).toBe('running');
      }).toPass({ timeout: 10000 });
      detached.push(await json(request, `${base}/runs/${run}`));
    }
    expect(detached[0].calls![0].run).not.toBe(detached[1].calls![0].run);
    await page.locator('[data-id="call"]').click();
    await expect(records).toContainText('本节点：已完成');
    await expect(records).toContainText('执行中');
    await page.screenshot({ path: test.info().outputPath('real-call-detached.png'), fullPage: true });
    await page.getByRole('button', { name: '图编排', exact: true }).click();
    await page.getByLabel('当前工作流').selectOption('target');
    await expect(page.getByLabel('活动运行')).toContainText('2 个活动运行', { timeout: 8000 });
    const graphs = await json(request, `${base}/graphs`);
    expect(graphs.graphs.find((graph: { graph: string }) => graph.graph === 'target').active_runs).toHaveLength(2);
    evidence.concurrentRuns = graphs;
    await page.screenshot({ path: test.info().outputPath('real-call-concurrent.png'), fullPage: true });
    await page.getByRole('button', { name: '工作流关系', exact: true }).click();
    const relations = page.getByRole('dialog', { name: '工作流关系' });
    await expect(relations).toContainText('定时计划 1');
    await expect(relations).toContainText('启动后继续');
    evidence.relations = await json(request, `${base}/graph-relations`);
    await page.screenshot({ path: test.info().outputPath('real-call-relations.png'), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(relations).toBeInViewport();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy();
    await page.screenshot({ path: test.info().outputPath('real-call-relations-mobile.png'), fullPage: true });
    await relations.getByRole('button', { name: '定位 source 的调用节点 call' }).click();
    await expect(editor.getByLabel('执行模式')).toHaveValue('detach');
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.getByRole('button', { name: '运行看板', exact: true }).click();
    await page.getByLabel('筛选调用链').selectOption(detached[1].run);
    await expect(page.locator('.timeline-entry')).toHaveCount(2);
    await expect(async () => {
      for (const detail of detached) expect((await json(request, `${base}/runs/${detail.calls![0].run}`)).state.status).toBe('finished');
    }).toPass({ timeout: 25000 });
    evidence.detachedRuns = await Promise.all(detached.map(detail => json(request, `${base}/runs/${detail.run}`)));
    evidence.browserErrors = errors;
    expect(errors).toEqual([]);
  } finally {
    await writeFile(test.info().outputPath('real-call-evidence.json'), JSON.stringify(evidence, null, 2));
    await writeFile(test.info().outputPath('service.log'), serverLog);
    await stop(vite); await stop(server);
    await rm(root, { recursive: true, force: true });
  }
});
