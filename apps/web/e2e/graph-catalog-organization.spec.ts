import { test, expect, type Page } from '@playwright/test';
import type { GraphRelationsData, GraphSummary, OurGraph, OurRun, OurRunDetail } from '../src/model';
import { runProjectionFixture } from './fixtures/runProjection';

const assistant = 'catalog-assistant';
const child = 'task-market-review-20261112';
const sibling = 'task-delivery-checklist-20261112';
const producer = 'ai-morning-briefing';
const completed = 'task-review-completed-20261110';
const secondary = 'secondary-assistant';
const schedule = 'shared-morning-schedule';
const labels = {
  [assistant]: '项目助理',
  [child]: '整理市场要闻并生成简报',
  [sibling]: '复核明日交付清单',
  [producer]: 'AI 晨报：独立生产并供多个订阅者使用',
  [completed]: '已完成的交付复盘',
};
const summaries: GraphSummary[] = [
  { graph: assistant, objective: labels[assistant], running: null },
  { graph: child, objective: labels[child], task_source: assistant, running: null,
    schedule_summary: { total: 2, enabled: 1, recurring: 1 } },
  { graph: sibling, objective: labels[sibling], task_source: assistant, running: null,
    schedule_summary: { total: 1, enabled: 1, recurring: 0 } },
  { graph: producer, objective: labels[producer], running: null,
    schedule_summary: { total: 1, enabled: 1, recurring: 1 } },
  { graph: completed, objective: labels[completed], task_source: assistant, running: null, completed_task: true },
  { graph: secondary, running: null },
];
const definitions: Record<string, OurGraph> = Object.fromEntries(summaries.map(item => [item.graph, {
  objective: item.objective, entry: 'work', ops: { work: { run: 'true' } },
  nodes: [{ id: 'work', op: 'work' }], edges: [],
}]));
definitions[assistant] = { objective: labels[assistant], entry: 'work',
  ops: { work: { run: 'true' }, publish: { call: { graph: producer, mode: 'detach' } } },
  nodes: [{ id: 'work', op: 'work' }, { id: 'publish', op: 'publish' }],
  edges: [{ from: 'work', to: 'publish' }],
};
const run: OurRun = { run: 'catalog-completed-run', graph: completed, objective: labels[completed],
  running: false, started: '2026-11-10T09:32:00Z', updated: '2026-11-10T09:32:05Z', executed: ['work'],
  projection: runProjectionFixture('settled', { outcome: 'done' }),
};
const detail: OurRunDetail = { run: run.run, graph: completed, nodes: ['work'], traces: {},
  definition: definitions[completed], projection: run.projection, active: false,
  state: { objective: run.objective, started: run.started, updated: run.updated, cursor: null, passes: {},
    decided: {}, nodes: {}, executed: ['work'], skipped: [], error: '' },
};
const relations: GraphRelationsData = {
  graphs: summaries.map(item => ({ graph: item.graph, objective: item.objective, task_source: item.task_source,
    schedules: item.schedule_summary?.total ?? 0 })),
  calls: [{ graph: assistant, node: 'publish', op: 'publish', target: producer, mode: 'detach' }],
  subscriptions: [
    { graph: assistant, target: producer, schedule, active: true, authorized: true, not_before: '09:05' },
    { graph: secondary, target: producer, schedule, active: false, authorized: false, not_before: null },
  ],
};

// These are browser projection fixtures, not evidence of production execution or delivery.
async function fixture(page: Page) {
  const writes: string[] = [];
  const problems: string[] = [];
  const reads: string[] = [];
  const unhandled: string[] = [];
  page.on('pageerror', error => problems.push(error.message));
  await page.route('**/*', route => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (request.method() !== 'GET') {
      writes.push(`${request.method()} ${path}`);
      return route.fulfill({ status: 405, json: { error: 'Read-only fixture' } });
    }
    if (['fetch', 'xhr', 'eventsource'].includes(request.resourceType())) reads.push(path);
    if (path === '/graphs') return route.fulfill({ json: { graphs: summaries } });
    if (path.startsWith('/graphs/')) {
      const definition = definitions[decodeURIComponent(path.slice('/graphs/'.length))];
      if (definition) return route.fulfill({ json: { definition, plugin_activation: { mode: 'new_run', graph_digest: 'catalog-fixture' } } });
    }
    if (path === '/runs') return route.fulfill({ json: { runs: [run] } });
    if (path === `/runs/${run.run}`) return route.fulfill({ json: detail });
    if (path === '/timeline') return route.fulfill({ json: { runs: [run], schedules: [], scheduled: [] } });
    if (path === '/graph-relations') return route.fulfill({ json: relations });
    if (path === '/plugins') return route.fulfill({ json: { plugins: [] } });
    if (path === '/channel-sessions') return route.fulfill({ json: { sessions: [] } });
    if (path.endsWith('.wasm')) return route.continue();
    if (['fetch', 'xhr', 'eventsource'].includes(request.resourceType())) {
      unhandled.push(path);
      return route.fulfill({ status: 404, json: { error: 'Unmapped fixture read' } });
    }
    return route.continue();
  });
  await page.goto('/');
  await expect(page.getByLabel('当前工作流')).toHaveValue(assistant);
  await expect(page.locator('.graph-document-title h2')).toHaveText(assistant);
  return { writes, problems, reads, unhandled };
}

async function noOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1)).toBe(true);
  expect(await page.locator('.graph-catalog').evaluate(element => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
}

for (const width of [1440, 390]) {
  test(`mocked: organize created tasks without hiding searchable history at ${width}px`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 960 });
    const observed = await fixture(page);
    const main = page.getByLabel('主工作流列表');
    const rootRows = main.locator(':scope > .graph-catalog-group > .library-item');
    const childRow = main.locator(`[data-graph="${child}"]`);
    const producerRow = main.locator(`[data-graph="${producer}"]`);
    const toggle = main.getByRole('button', { name: `${assistant} 创建的任务 2`, exact: true });
    const completedToggle = page.getByRole('button', { name: /已完成的一次性任务/ });
    const search = page.getByLabel('搜索工作流');

    await expect(rootRows).toHaveCount(3);
    await expect(rootRows.locator('.library-name')).toHaveText([assistant, producer, secondary]);
    await expect(main.locator(`[data-graph="${secondary}"] .library-identifier`)).toHaveCount(0);
    await expect(page.getByLabel('当前工作流').locator(`option[value="${child}"]`)).toHaveText(child);
    await expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await expect(childRow).toBeHidden();
    await expect(main.locator(`[data-graph="${sibling}"]`)).toBeHidden();
    await expect(producerRow).toBeVisible();
    await expect(main.locator(`.graph-task-list [data-graph="${producer}"]`)).toHaveCount(0);
    await expect(completedToggle).toHaveAttribute('aria-expanded', 'false');
    await expect(main.locator(`[data-graph="${completed}"]`)).toHaveCount(0);
    await noOverflow(page);

    await toggle.click();
    await expect(childRow.locator('.library-name')).toHaveText(child);
    await expect(childRow.locator('.library-identifier')).toHaveCount(0);
    await expect(childRow).not.toContainText(labels[child]);
    await expect(childRow.getByText('周期任务', { exact: true })).toBeVisible();
    await expect(main.locator(`[data-graph="${sibling}"]`).getByText('周期任务', { exact: true })).toHaveCount(0);
    await toggle.click();
    await expect(childRow).toBeHidden();

    await search.fill(labels[child]);
    await expect(toggle).toHaveAttribute('aria-expanded', 'true');
    await expect(main.locator(`[data-graph="${assistant}"]`)).toBeVisible();
    await expect(childRow).toBeVisible();
    await expect(main.locator(`[data-graph="${sibling}"]`)).toHaveCount(0);
    await search.fill(child);
    await expect(childRow).toBeVisible();
    await childRow.locator('.library-row').click();
    await expect(page.getByLabel('当前工作流')).toHaveValue(child);
    await expect(page.locator('.graph-document-title h2')).toHaveText(child);
    await expect(page.locator('.graph-document-identifier')).toHaveCount(0);
    await expect(page.locator('.graph-document-title')).not.toContainText(labels[child]);
    expect(observed.reads).toContain(`/graphs/${child}`);
    await search.fill('');
    await expect(toggle).toHaveAttribute('aria-expanded', 'true');
    await expect(childRow.locator('.library-row')).toHaveAttribute('aria-pressed', 'true');
    await expect(childRow).toBeVisible();
    await noOverflow(page);
    await page.screenshot({ path: info.outputPath(`selected-created-task-${width}.png`), fullPage: true });

    await producerRow.locator('.library-row').click();
    await expect(page.getByLabel('当前工作流')).toHaveValue(producer);
    await expect(page.locator('.graph-document-title h2')).toHaveText(producer);
    await expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await expect(producerRow).toBeVisible();
    await search.fill(labels[completed]);
    await expect(completedToggle).toHaveAttribute('aria-expanded', 'true');
    const completedRow = page.getByLabel('已完成工作流列表').locator(`[data-graph="${completed}"]`);
    await expect(completedRow.locator('.library-name')).toHaveText(completed);
    await expect(completedRow.locator('.library-identifier')).toHaveCount(0);
    await search.fill(completed);
    await completedRow.locator('.library-row').click();
    await expect(page.getByLabel('当前工作流')).toHaveValue(completed);
    await expect(page.locator('.graph-document-title h2')).toHaveText(completed);
    await page.getByRole('button', { name: /运行历史/ }).click();
    const history = page.getByLabel('此图的运行历史');
    await expect(history).toBeVisible();
    await expect(history).toContainText(run.run);
    await expect(history.getByRole('button').filter({ hasText: run.run })).toBeEnabled();
    await noOverflow(page);
    await page.screenshot({ path: info.outputPath(`completed-task-history-${width}.png`), fullPage: true });
    expect(observed.writes).toEqual([]);
    expect(observed.unhandled).toEqual([]);
    expect(observed.problems).toEqual([]);
  });

  test(`mocked: distinguish creation, calls and result subscriptions at ${width}px`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 960 });
    const observed = await fixture(page);
    await page.getByRole('button', { name: '工作流关系', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: '工作流关系' });
    const created = dialog.getByRole('region', { name: '创建归属', exact: true });
    const calls = dialog.getByRole('region', { name: '工作流调用', exact: true });
    const subscriptions = dialog.getByRole('region', { name: '结果订阅', exact: true });
    const search = dialog.getByLabel('查找关系');
    const all = dialog.getByRole('checkbox', { name: '显示全部关系' });
    await expect(created).toBeVisible();
    await expect(calls).toBeVisible();
    await expect(subscriptions).toBeVisible();
    await expect(created.locator(`button[title="${child}"]`)).toBeVisible();
    await expect(created.locator(`button[title="${producer}"]`)).toHaveCount(0);
    await expect(calls).toContainText('启动后继续');
    await expect(subscriptions).toContainText('订阅中 · 09:05 后');
    await expect(subscriptions).toContainText(producer);
    await expect(subscriptions).not.toContainText(labels[producer]);
    await expect(dialog.getByLabel('相关工作流').getByRole('button', { name: secondary, exact: true })).toHaveCount(0);
    await expect(all).not.toBeChecked();
    await noOverflow(page);
    expect(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
    await page.screenshot({ path: info.outputPath(`mixed-graph-relations-${width}.png`), fullPage: true });

    await search.fill(labels[child]);
    await expect(created.locator(`button[title="${child}"]`)).toBeVisible();
    await expect(calls).toHaveCount(0);
    await expect(subscriptions).toHaveCount(0);
    await search.fill(child);
    await expect(created.locator(`button[title="${child}"]`)).toBeVisible();
    await search.fill(labels[producer]);
    await expect(created).toHaveCount(0);
    await expect(calls).toBeVisible();
    await expect(subscriptions).toBeVisible();
    await search.fill(schedule);
    await expect(calls).toHaveCount(0);
    await expect(subscriptions).toContainText(producer);
    await expect(subscriptions).not.toContainText(labels[producer]);
    await search.fill('');
    await all.check();
    await expect(dialog.getByLabel('相关工作流').getByRole('button', { name: secondary, exact: true })).toBeVisible();
    await expect(subscriptions).toContainText('已取消');
    await search.fill(secondary);
    await expect(created).toHaveCount(0);
    await expect(calls).toHaveCount(0);
    await expect(subscriptions.locator('li')).toHaveCount(1);
    await expect(subscriptions).toContainText(secondary);
    await search.fill('');
    await all.uncheck();
    await created.locator(`button[title="${child}"]`).click();
    await expect(dialog).toHaveCount(0);
    await expect(page.getByLabel('当前工作流')).toHaveValue(child);
    await expect(page.locator('.graph-document-title h2')).toHaveText(child);
    await expect(page.getByRole('button', { name: `${assistant} 创建的任务 2`, exact: true })).toHaveAttribute('aria-expanded', 'true');

    await page.getByRole('button', { name: '工作流关系', exact: true }).click();
    await dialog.getByLabel('中心工作流').selectOption(assistant);
    await dialog.getByRole('button', { name: `定位 ${assistant} 的调用节点 publish`, exact: true }).click();
    await expect(page.getByLabel('当前工作流')).toHaveValue(assistant);
    await expect(page.getByRole('region', { name: '调用工作流设置' }).getByLabel('目标工作流')).toHaveValue(producer);
    await page.getByRole('button', { name: '工作流关系', exact: true }).click();
    await subscriptions.locator(`button[title="${producer}"]`).click();
    await expect(page.getByLabel('当前工作流')).toHaveValue(producer);
    await expect(page.locator('.graph-document-title h2')).toHaveText(producer);
    await expect(page.getByLabel('主工作流列表').locator(`.graph-task-list [data-graph="${producer}"]`)).toHaveCount(0);
    await noOverflow(page);
    expect(observed.reads).toContain('/graph-relations');
    expect(observed.writes).toEqual([]);
    expect(observed.unhandled).toEqual([]);
    expect(observed.problems).toEqual([]);
  });
}
