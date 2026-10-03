import { test, expect, type Page } from '@playwright/test';
import type { OurGraph, OurRun, OurRunDetail } from '../src/model';

// Contract fixtures only: actual admission, isolation and provider behavior are verified by backend E2E.
const source: OurGraph = { entry: 'prepare', nodes: [{ id: 'prepare', op: 'prepare' }, { id: 'notify', op: 'notify' }],
  edges: [{ from: 'prepare', to: 'notify' }], ops: { prepare: { run: 'true' }, notify: { call: { graph: 'assistant', mode: 'detach' } } } };
const target: OurGraph = { entry: 'reply', nodes: [{ id: 'reply', op: 'reply' }], edges: [], ops: { reply: { run: 'true' } } };
const started = new Date().toISOString();
const parent: OurRun = { run: 'parent-run', graph: 'report', status: 'finished', running: false, started, updated: started, executed: ['prepare', 'notify'], objective: '发布周报' };
const child: OurRun = { ...parent, run: 'child-run', graph: 'assistant', status: 'running', running: true,
  trigger: { source: 'graph_call', graph: 'report', run: 'parent-run', node: 'notify', invocation: 2, mode: 'detach', root_run: 'parent-run' } };
const waitChild: OurRun = { ...child, run: 'wait-child', trigger: { ...child.trigger!, mode: 'wait' } };
const other: OurRun = { ...child, run: 'other-child', running: false, trigger: { ...child.trigger!, root_run: 'other-parent', run: 'other-parent' } };
const detail = (run: OurRun): OurRunDetail => ({ run: run.run, graph: run.graph, nodes: run.graph === 'report' ? ['prepare', 'notify'] : ['reply'], traces: {},
  active: run.running,
  state: { objective: run.objective, started, status: run.status, updated: started, trigger: run.trigger, cursor: null, passes: { notify: 2 }, decided: {}, nodes: run.graph === 'report' ? { notify: { node_id: 'notify', pass_number: 2, submission: '已接纳', files: ['call.json'], submitted: true, exit_status: 'completed', route: null } } : {}, executed: run.executed, skipped: [], error: '' },
  calls: run.graph === 'report' ? [{ node: 'notify', invocation: 1, graph: 'assistant', run: 'first-child', mode: 'detach', status: 'failed' },
    { node: 'notify', invocation: 2, graph: 'assistant', run: 'wait-child', mode: 'wait', status: 'running', active: true, input: { report: 'weekly' } },
    { node: 'notify', invocation: 3, graph: 'assistant', run: 'child-run', mode: 'detach', status: 'running', active: true, input: { report: 'weekly' } }] : [] });
async function fixture(page: Page) {
  const saved: OurGraph[] = [];
  let current = structuredClone(source);
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [{ graph: 'report', running: null, active_runs: [] }, { graph: 'assistant', running: 'child-run', active_runs: ['child-run', 'other-child'] }] } }));
  await page.route('**/graphs/*', route => {
    if (route.request().method() === 'PUT') { current = route.request().postDataJSON().definition; saved.push(current); return route.fulfill({ json: { saved: true } }); }
    return route.fulfill({ json: { definition: route.request().url().endsWith('/report') ? current : target } });
  });
  await page.route('**/runs', route => route.fulfill({ json: { runs: [parent, waitChild, child, other] } }));
  await page.route('**/runs/*', route => {
    const path = new URL(route.request().url()).pathname;
    const id = path.split('/')[2];
    if (route.request().method() !== 'GET') return route.fulfill({ status: 204, body: '' });
    const run = [parent, waitChild, child, other].find(item => item.run === id) ?? child;
    return route.fulfill({ json: detail(run) });
  });
  await page.route('**/timeline*', route => route.fulfill({ json: { runs: [parent, waitChild, child, other], schedules: [], scheduled: [] } }));
  await page.route('**/channel-sessions', route => route.fulfill({ json: { sessions: [{ id: 'session-one', title: '产品团队', graph: 'assistant', platform: 'wecom' }] } }));
  await page.route('**/graph-relations', route => route.fulfill({ json: { graphs: [{ graph: 'report', schedules: 1 }, { graph: 'assistant', schedules: 0 }, { graph: 'unrelated', schedules: 0 }], calls: [{ graph: 'report', node: 'notify', op: 'notify', target: 'assistant', mode: 'detach' }] } }));
  await page.goto('/');
  await page.getByLabel('当前工作流').selectOption('report');
  await expect(page.locator('[data-id="notify"]')).toBeVisible();
  return saved;
}

test('mocked: author structured calls and navigate definition relations', async ({ page }) => {
  const saved = await fixture(page);
  await page.getByRole('button', { name: '添加节点', exact: true }).click();
  await page.getByRole('button', { name: '调用工作流 启动独立运行' }).click();
  const editor = page.getByRole('region', { name: '调用工作流设置' });
  await expect(editor.getByLabel('目标工作流')).toHaveValue('assistant');
  await page.screenshot({ path: 'test-results/graph-call-editor.png' });
  await editor.getByRole('button', { name: '编辑输入常量' }).click();
  await page.getByRole('textbox', { name: '输入常量' }).fill('{"topic":"weekly"}');
  await page.getByRole('button', { name: '应用更改' }).click();
  await editor.getByRole('button', { name: '添加输入映射' }).click();
  await editor.getByLabel('来源 JSON Pointer').fill('/request/code');
  await editor.getByLabel('目标参数').fill('code');
  await editor.getByRole('button', { name: '添加文件', exact: true }).click();
  await editor.getByLabel('来源节点').selectOption('prepare');
  await editor.getByLabel('文件路径', { exact: true }).fill('report.md');
  await editor.getByLabel('目标文件名').fill('weekly.md');
  await editor.getByLabel('目标输出节点').selectOption('reply');
  await editor.getByLabel('返回文件').fill('answer.md');
  await editor.getByLabel('已有通道会话').selectOption('session-one');
  await page.getByRole('button', { name: '保存', exact: true }).click();
  expect(saved[0].ops?.call.call).toEqual({ graph: 'assistant', mode: 'wait', input: { topic: 'weekly' }, input_map: { code: '/request/code' },
    files: [{ node: 'prepare', path: 'report.md', as: 'weekly.md' }], result: { node: 'reply', files: ['answer.md'] }, session: 'session-one' });
  await editor.getByLabel('执行模式').selectOption('detach');
  await expect(editor.getByText('停止本次运行不会停止', { exact: false })).toBeVisible();
  await expect(editor.getByLabel('目标输出节点')).toHaveCount(0);
  await page.getByRole('button', { name: '保存', exact: true }).click();
  expect(saved[1].ops?.call.call?.result).toBeUndefined();
  await editor.getByRole('button', { name: '打开目标工作流' }).click();
  await expect(page.getByLabel('当前工作流')).toHaveValue('assistant');
  await expect(page.getByLabel('活动运行')).toContainText('2 个活动运行');
  await page.getByRole('button', { name: '返回来源工作流' }).click();
  await expect(editor.getByLabel('目标工作流')).toHaveValue('assistant');
  await page.getByRole('button', { name: '工作流关系', exact: true }).click();
  const relations = page.getByRole('dialog', { name: '工作流关系' });
  await expect(relations).toContainText('定时计划 1');
  await expect(relations.locator('.relation-graphs')).not.toContainText('unrelated');
  await page.screenshot({ path: 'test-results/graph-call-relations.png' });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(relations).toBeInViewport();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: 'test-results/graph-call-relations-mobile.png' });
  await page.setViewportSize({ width: 1440, height: 1000 });
  await relations.getByRole('button', { name: '定位 report 的调用节点 notify' }).click();
  await expect(editor.getByLabel('执行模式')).toHaveValue('detach');
});

test('mocked: show wait/detach child Runs, source links and active controls', async ({ page }) => {
  await fixture(page);
  await page.getByRole('button', { name: '查看最近运行', exact: true }).click();
  await page.locator('[data-id="notify"]').click();
  const records = page.getByRole('region', { name: '工作流调用记录' });
  await expect(records).toContainText('本节点：已完成');
  await expect(records.locator('.call-record')).toHaveCount(3);
  await expect(records.locator('.call-record').first()).toContainText('失败');
  await expect(records.locator('.call-record').nth(1)).toContainText('正在等待目标完成');
  await expect(records.locator('.call-record').last()).toContainText('目标正在后台执行');
  await page.screenshot({ path: 'test-results/graph-call-run.png' });
  await records.getByRole('button', { name: '查看目标运行' }).last().click();
  await expect(page.locator('.run-origin')).toContainText('report / notify · 第 2 轮');
  await expect(page.getByRole('button', { name: '暂停' })).toBeVisible();
  await expect(page.getByRole('button', { name: '停止', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '返回来源运行' }).click();
  await expect(records).toContainText('本节点：已完成');
  await page.getByRole('button', { name: '返回时间线' }).click();
  await page.getByLabel('筛选调用链').selectOption('parent-run');
  await expect(page.locator('.timeline-entry')).toHaveCount(3);
  await expect(page.getByLabel('筛选调用链')).toHaveValue('parent-run');
  await page.locator('[data-run-id="wait-child"]').click();
  const preview = page.getByRole('dialog', { name: '运行详情' });
  await expect(preview).toContainText('等待目标完成');
  await expect(preview).toContainText('report / notify · 第 2 轮');
  await preview.getByRole('button', { name: '查看来源运行' }).click();
  await expect(page.locator('.document-title h2')).toHaveText('report');
  await page.locator('[data-id="notify"]').click();
  await expect(page.getByRole('region', { name: '工作流调用记录' })).toContainText('wait-child');
  await page.getByRole('button', { name: '返回时间线' }).click();
  await page.locator('[data-run-id="child-run"]').click();
  const detachedPreview = page.getByRole('dialog', { name: '运行详情' });
  await expect(detachedPreview).toContainText('启动后独立运行');
  await expect(detachedPreview).toContainText('report / notify · 第 2 轮');
  await detachedPreview.getByRole('button', { name: '查看来源运行' }).click();
  await expect(page.locator('.document-title h2')).toHaveText('report');
  await page.getByRole('button', { name: '返回时间线' }).click();
  await page.locator('[data-run-id="other-child"]').click();
  await page.getByRole('button', { name: '查看节点与产物' }).click();
  await expect(page.getByRole('button', { name: '继续' })).toBeVisible();
  await expect(page.getByRole('button', { name: '暂停' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '停止', exact: true })).toHaveCount(0);
});
