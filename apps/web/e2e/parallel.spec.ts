import { test, expect, type Page } from '@playwright/test';
import { addParallelRegion } from '../src/parallel';
import type { OurGraph, OurRun, OurRunDetail } from '../src/model';

// UI contract fixtures. Scheduler/provider acceptance is tested separately against the real service.
const graph = addParallelRegion({ entry: 'finish', nodes: [{ id: 'finish', op: 'finish' }], edges: [], ops: { finish: { run: 'true' } } }).graph;
const started = new Date().toISOString();
const run: OurRun = { run: 'parallel-run', graph: 'parallel', status: 'running', running: true, started, updated: started, executed: ['fanout'], objective: '并行研究' };
const detail: OurRunDetail = { run: run.run, graph: run.graph, nodes: graph.nodes.map(node => node.id), traces: {}, state: {
  objective: run.objective, started, updated: started, status: 'running', cursor: null,
  active: { 'branch-a': { node: 'branch-a', pass: 1, run: 1, dir: 'a' }, 'branch-b': { node: 'branch-b', pass: 1, run: 1, dir: 'b' } },
  parallel: { fanout: 'fanout', join: 'join', invocation: 1 }, passes: { 'branch-a': 1, 'branch-b': 1 },
  decided: { 'fanout|branch-a': [true, 1], 'fanout|branch-b': [true, 1] }, nodes: {}, executed: ['fanout'], skipped: [], error: '',
} };
async function fixture(page: Page, running = false) {
  let current = structuredClone(graph);
  const saved: OurGraph[] = [];
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [{ graph: 'parallel', running: running ? run.run : null, active_runs: running ? [run.run] : [] }] } }));
  await page.route('**/graphs/*', route => {
    if (route.request().method() === 'PUT') { current = route.request().postDataJSON().definition; saved.push(current); return route.fulfill({ json: { saved: true } }); }
    return route.fulfill({ json: { definition: current } });
  });
  await page.route('**/runs', route => route.fulfill({ json: { runs: running ? [run] : [] } }));
  await page.route('**/runs/*', route => route.fulfill({ json: detail }));
  await page.route('**/timeline*', route => route.fulfill({ json: { runs: running ? [run] : [], schedules: [], scheduled: [] } }));
  await page.goto('/');
  await expect(page.locator('[data-id="fanout"]')).toBeVisible();
  return saved;
}

test('mocked: add paired parallel region, edit only fanout pairing, rename join and save', async ({ page }) => {
  const saved = await fixture(page);
  await page.getByRole('button', { name: '添加节点', exact: true }).click();
  await page.getByRole('button', { name: '并行分支 在入口前' }).click();
  const editor = page.getByRole('region', { name: '并行区域设置' });
  await expect(editor.getByLabel('配对收束节点')).toHaveValue('join2');
  await expect(page.getByLabel('命令', { exact: true })).toHaveCount(0);
  await expect(page.locator('[data-id="fanout2"]')).toContainText('并行展开');
  await expect(page.locator('[data-id="join2"]')).toContainText('展开自 fanout2');
  await page.locator('[data-id="join2"]').click();
  await expect(editor).toContainText('配对展开节点：fanout2');
  await expect(editor.getByRole('combobox')).toHaveCount(0);
  await page.getByLabel('节点 ID', { exact: true }).fill('collect');
  await page.locator('[data-id="fanout2"]').click();
  await expect(editor.getByLabel('配对收束节点')).toHaveValue('collect');
  await page.getByRole('button', { name: '保存', exact: true }).click();
  expect(saved[0].ops?.fanout2.fanout?.join).toBe('collect');
  expect(saved[0].ops?.join2).toEqual({ join: {} });
  expect(saved[0].edges).toContainEqual({ from: 'collect', to: 'fanout' });
  await page.screenshot({ path: 'test-results/parallel-editor.png' });
});

test('mocked: show simultaneous branch execution and pending join in one Run', async ({ page }) => {
  await fixture(page, true);
  await page.getByRole('button', { name: '查看当前运行', exact: true }).click();
  await expect(page.getByLabel('活动节点', { exact: true })).toContainText('branch-a（第 1 轮）、branch-b（第 1 轮）');
  await expect(page.locator('.execution-node.state-running')).toHaveCount(2);
  await expect(page.locator('.react-flow__edge.animated')).toHaveCount(2);
  await expect(page.locator('[data-id="join"]')).toContainText('尚未执行');
  await page.locator('[data-id="branch-b"]').click();
  await expect(page.locator('.node-summary .pill')).toHaveText('执行中');
  await page.screenshot({ path: 'test-results/parallel-run.png' });
});
