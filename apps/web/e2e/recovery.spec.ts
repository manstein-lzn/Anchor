import { expect, test } from '@playwright/test';
import type { OurGraph, OurRun, OurRunDetail } from '../src/model';

test('unknown tool result resumes through the normal Agent context path', async ({ page }) => {
  const started = new Date().toISOString();
  const graph: OurGraph = { entry: 'work', nodes: [{ id: 'work', op: 'work' }], edges: [], ops: { work: { run: 'true' } } };
  const run: OurRun = { run: 'recovery-run', graph: 'recovery', status: 'waiting_recovery', running: false,
    started, updated: started, executed: ['work'], objective: '恢复 Agent 上下文' };
  let status = 'waiting_recovery';
  let resumed = false;
  const detail = (): OurRunDetail => ({ active: status === 'running', run: run.run, graph: run.graph, nodes: ['work'], traces: {}, state: {
    objective: run.objective, started, updated: started, status, cursor: { node: 'work', pass: 1, dir: 'work' },
    recovery: [{ key: { run_id: run.run, graph_digest: 'graph-sha256', node_id: 'work', invocation: 1 },
      attempt: { attempt_id: 1, step: 3, tool: 'anchor_run', started_at: started } }],
    passes: { work: 1 }, decided: {}, nodes: {}, executed: ['work'], skipped: [], error: '',
  } });

  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [{ graph: run.graph, running: null, active_runs: [] }] } }));
  await page.route('**/graphs/*', route => route.fulfill({ json: { definition: graph } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [run] } }));
  await page.route('**/timeline*', route => route.fulfill({ json: { runs: [run], schedules: [], scheduled: [] } }));
  await page.route('**/runs/**', async route => {
    const url = new URL(route.request().url());
    if (url.pathname.endsWith('/resume') && route.request().method() === 'POST') {
      resumed = true;
      status = 'running';
      return route.fulfill({ json: { run: run.run, asked: 'resume' } });
    }
    return route.fulfill({ json: detail() });
  });

  await page.goto('/');
  await page.locator('[data-id="work"]').waitFor();
  await page.getByRole('button', { name: '查看最近运行' }).click();
  await expect(page.getByText('Agent 上次工具调用的结果未记录')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Retry' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Completed' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Abort' })).toHaveCount(0);
  await page.getByRole('button', { name: '继续', exact: true }).click();
  await expect.poll(() => resumed).toBe(true);
  await expect(page.locator('.canvas-head .pill')).toHaveText('执行中');
});

test('inactive running Run can be resumed from its existing detail page', async ({ page }) => {
  const started = new Date().toISOString();
  const graph: OurGraph = { entry: 'work', nodes: [{ id: 'work', op: 'work' }], edges: [], ops: { work: { run: 'true' } } };
  const run: OurRun = { run: 'interrupted-run', graph: 'recovery', status: 'running', running: false,
    started, updated: started, executed: [], objective: '恢复已保存的 cursor' };
  let active = false;
  let resumed = false;
  const detail = (): OurRunDetail => ({ active, run: run.run, graph: run.graph, nodes: ['work'], traces: {}, state: {
    objective: run.objective, started, updated: started, status: 'running', cursor: { node: 'work', pass: 1, dir: 'work' },
    recovery: [], passes: { work: 1 }, decided: {}, nodes: {}, executed: [], skipped: [], error: '',
  } });

  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [{ graph: run.graph, running: null, active_runs: [] }] } }));
  await page.route('**/graphs/*', route => route.fulfill({ json: { definition: graph } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [run] } }));
  await page.route('**/timeline*', route => route.fulfill({ json: { runs: [run], schedules: [], scheduled: [] } }));
  await page.route('**/runs/**', async route => {
    const url = new URL(route.request().url());
    if (url.pathname.endsWith('/resume') && route.request().method() === 'POST') {
      resumed = true;
      active = true;
      return route.fulfill({ json: { run: run.run, asked: 'resume' } });
    }
    return route.fulfill({ json: detail() });
  });

  await page.goto('/');
  await page.locator('[data-id="work"]').waitFor();
  await page.getByRole('button', { name: '查看最近运行' }).click();
  await expect(page.locator('.canvas-head .pill')).toHaveText('等待接续');
  await expect(page.getByText('宿主重启后尚未接续')).toBeVisible();
  await page.getByRole('button', { name: '继续', exact: true }).click();
  await expect.poll(() => resumed).toBe(true);
  await expect(page.locator('.canvas-head .pill')).toHaveText('执行中');
});

test('timeline marks an inactive running Run as waiting to resume', async ({ page }) => {
  const started = new Date().toISOString();
  const graph: OurGraph = { entry: 'work', nodes: [{ id: 'work', op: 'work' }], edges: [], ops: { work: { run: 'true' } } };
  const run: OurRun = { run: 'waiting-run', graph: 'recovery', status: 'running', running: false,
    started, updated: '', executed: [], objective: '显示准确的接续状态' };
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [{ graph: run.graph, running: null, active_runs: [] }] } }));
  await page.route('**/graphs/*', route => route.fulfill({ json: { definition: graph } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [run] } }));
  await page.route('**/timeline*', route => route.fulfill({ json: { runs: [run], schedules: [], scheduled: [], capabilities: { scheduling: false } } }));
  await page.route('**/runs/**', route => route.fulfill({ json: { run: run.run, graph: run.graph, state: {
    objective: run.objective, started, updated: '', status: 'running', recovery: [], passes: {}, decided: {}, nodes: {},
    executed: [], skipped: [], error: '', input: {},
  }, traces: {}, nodes: [], active: false } }));

  await page.goto('/');
  await page.locator('[data-id="work"]').waitFor();
  await page.getByRole('button', { name: '运行看板' }).click();
  const entry = page.getByRole('button', { name: /recovery，等待接续/ });
  await expect(entry).toBeVisible();
  await entry.click();
  await expect(page.getByText('宿主重启后等待接续')).toBeVisible();
  await expect(page.getByText('0 秒')).toHaveCount(0);
  await expect(page.getByText('Invalid Date')).toHaveCount(0);
});
