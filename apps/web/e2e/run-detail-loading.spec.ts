import { expect, test } from '@playwright/test';
import type { OurGraph, OurRun, OurRunDetail } from '../src/model';
import { blockUnmockedApi } from './fixtures/api-fallback';
import { runProjectionFixture } from './fixtures/runProjection';

const graph: OurGraph = { entry: 'work', nodes: [{ id: 'work', op: 'work' }], edges: [], ops: { work: { run: 'true' } } };
const started = new Date().toISOString();
const run: OurRun = { run: 'slow-run', graph: 'slow-graph', running: false, started, updated: started, executed: ['work'],
  objective: '验证详情加载', projection: runProjectionFixture('settled', { outcome: 'done' }) };
const detail: OurRunDetail = { ...run, definition: graph, nodes: ['work'], traces: {},
  state: { objective: run.objective, started, updated: started, cursor: null, passes: { work: 1 }, decided: {},
    nodes: { work: { node_id: 'work', pass_number: 1, submission: '完成', files: [], submitted: true, exit_status: 'completed', route: null } },
    executed: ['work'], skipped: [], error: '' } };

test('mocked: delayed Run detail survives list refresh, reports failure and retries', async ({ page }) => {
  await blockUnmockedApi(page);
  let listRequests = 0; let detailRequests = 0;
  let releaseFirst!: () => void;
  const firstDetail = new Promise<void>(resolve => { releaseFirst = resolve; });

  await page.route(/\/graphs$/, route => route.fulfill({ json: { graphs: [{ graph: 'slow-graph', running: null, active_runs: [] }] } }));
  await page.route('**/graphs/slow-graph', route => route.fulfill({ json: { definition: graph } }));
  await page.route(/\/runs$/, route => { listRequests += 1; return route.fulfill({ json: { runs: [run] } }); });
  await page.route('**/runs/slow-run', async route => {
    detailRequests += 1;
    if (detailRequests === 1) {
      await firstDetail;
      return route.fulfill({ status: 500, json: { error: 'signal timed out' } });
    }
    return route.fulfill({ json: detail });
  });

  await page.goto('/');
  await expect(page.getByLabel('当前工作流')).toHaveValue('slow-graph');
  await page.getByRole('button', { name: '查看最近运行' }).click();
  await expect(page.getByText('正在加载运行详情…')).toBeVisible();
  await expect.poll(() => listRequests, { timeout: 5000 }).toBeGreaterThan(1);
  expect(detailRequests).toBe(1);

  releaseFirst();
  const alert = page.getByRole('alert');
  await expect(alert).toContainText('运行详情加载失败：signal timed out');
  await expect(page.getByText('运行详情暂不可用。')).toBeVisible();
  await alert.getByRole('button', { name: '重试' }).click();

  await expect(page.locator('[data-id="work"]')).toBeVisible();
  await expect(page.getByText('验证详情加载')).toBeVisible();
  expect(detailRequests).toBe(2);
});
