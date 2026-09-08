import { test, expect, type Page } from '@playwright/test';
const token = 'anchor-browser-tests-only-not-a-real-secret';
const headers = { Authorization: `Bearer ${token}` };

async function expectRenderedEdges(page: Page, count: number) {
  const paths = page.locator('.react-flow__edge-path');
  await expect(paths).toHaveCount(count);
  const rendering = await paths.evaluateAll(items => items.map(item => {
    const path = item as SVGPathElement;
    const style = getComputedStyle(path);
    return { length: path.getTotalLength(), stroke: style.stroke, opacity: Number(style.opacity) };
  }));
  expect(rendering).toHaveLength(count);
  for (const edge of rendering) {
    expect(edge.length).toBeGreaterThan(0);
    expect(edge.stroke).not.toBe('none');
    expect(edge.opacity).toBeGreaterThan(0);
  }
}

test('polling retains geometry and distinguishes attempts, preflight and previous branches', async ({ page }) => {
  const graphId = 'browser-review-outcomes';
  const definition = { graph_id: graphId, name: 'Review display regression', nodes: [
    { id: 'plan', type: 'agent', name: 'Plan', agent_ref: 'agents.researcher', metadata: { behavior_ref: 'academic.planner' } },
    { id: 'research', type: 'agent', name: 'Research', agent_ref: 'agents.researcher' },
    { id: 'review', type: 'agent', name: 'Review', agent_ref: 'agents.reviewer', metadata: { behavior_ref: 'academic.reviewer' } },
    { id: 'check', type: 'artifact', name: 'Evidence gate', metadata: { behavior_ref: 'academic.review_gate' } },
    { id: 'report', type: 'artifact', name: 'Export paper.md', metadata: { behavior_ref: 'academic.report' } },
  ], edges: [{ source: 'plan', target: 'research' }, { source: 'research', target: 'review' },
    { source: 'review', target: 'check' }, { source: 'check', target: 'report' }] };
  expect((await page.request.put(`/api/graphs/${graphId}/draft`, { headers, data: { expected_revision: 0, definition } })).ok()).toBeTruthy();
  const version = await (await page.request.post(`/api/graphs/${graphId}/publish`, { headers, data: { expected_revision: 1 } })).json();
  const triggerId = crypto.randomUUID();
  await page.request.put(`/api/triggers/${triggerId}`, { headers, data: { graph_version_id: version.graph_version_id } });
  const receipt = await (await page.request.post(`/api/triggers/${triggerId}/runs`, {
    headers: { ...headers, 'Idempotency-Key': graphId }, data: { objective: 'Review outcomes' },
  })).json();
  const run = await (await page.request.get(`/api/runs/${receipt.run_id}`, { headers })).json();
  const old = new Date(Date.now() - 60_000).toISOString();
  const now = new Date().toISOString();
  let phase: 'research' | 'blocked' | 'pass' | 'missing' = 'research';
  let polls = 0;
  const outcomeRequests: Record<string, number> = {};
  const state = (id: string, attempt: number, status: string, output?: string) => ({ id: `${id}-${attempt}`, node_id: id,
    attempt, status, output_ref: output ? `artifact://sha256/${output}` : undefined,
    created_at: ['plan', 'research'].includes(id) && attempt > 0 ? now : old,
    updated_at: ['plan', 'research'].includes(id) ? now : old });
  await page.route('**/api/**', async route => {
    const url = new URL(route.request().url());
    const base = `/api/runs/${run.id}`;
    const respond = (json: unknown) => route.fulfill({ json });
    if (url.pathname === '/api/runs' && url.searchParams.get('graph_id') === graphId) return respond([{ ...run, status: 'running' }]);
    if (url.pathname === base) return respond({ ...run, status: phase === 'pass' ? 'completed' : 'running' });
    if (url.pathname === `${base}/nodes`) {
      polls++;
      return respond([
        state('plan', 0, 'failed'), state('plan', 1, 'completed'), state('plan', 2, 'completed'),
        state('research', 0, 'completed'), state('research', 1, phase === 'research' ? 'running' : 'completed'),
        state('review', 0, 'completed', `review-${phase}`), state('check', 0, 'completed', `check-${phase}`),
        state('report', 0, phase === 'pass' ? 'completed' : 'skipped'),
      ]);
    }
    if (url.pathname.startsWith('/api/artifacts/')) {
      const ref = url.pathname.split('/').at(-1)!;
      outcomeRequests[ref] = (outcomeRequests[ref] || 0) + 1;
      if (phase === 'missing') return route.fulfill({ status: 503, json: { detail: 'unavailable' } });
      const review = { verdict: phase === 'research' ? 'revise' : phase, review_origin: phase === 'research' ? 'deterministic_preflight' : undefined,
        issues: phase === 'research' ? [{ problem: 'Missing verified citations' }] : [] };
      return respond({ content: JSON.stringify(ref.startsWith('check') ? { review } : review) });
    }
    if (url.pathname === '/api/leases/active' && url.searchParams.get('run_id') === run.id) return respond(phase === 'research' ? [
      { state: 'healthy', lease: { node_run_id: 'research-1', acquired_at: now, heartbeat_at: now } },
    ] : []);
    if ([`${base}/contexts`, `${base}/decisions`, `${base}/operations`].includes(url.pathname)) return respond([]);
    return route.continue();
  });
  await page.goto('/');
  await page.getByLabel('API Token').fill(token);
  await page.getByRole('button', { name: '连接', exact: true }).click();
  await page.getByRole('button', { name: new RegExp(graphId) }).click();
  await page.getByRole('button', { name: '执行', exact: true }).click();
  const reviewNode = page.locator('.execution-node').filter({ has: page.locator('strong', { hasText: /^Review$/ }) });
  await expect(reviewNode).toContainText('上次：预检未通过');
  await expect(reviewNode).toContainText('未调用独立学术评审');
  await expect(page.locator('.execution-node').filter({ hasText: 'Export paper.md' })).toContainText('上次分支未执行');
  await expect(page.locator('.execution-summary')).toContainText('已完成 1 轮证据检查');
  await expect(page.locator('.execution-node').filter({ hasText: 'Plan' })).toContainText('第 3 次执行');
  // Check after successive polling replacements, not only initial measurement.
  for (let count = 2; count <= 4; count++) {
    await expect.poll(() => polls, { timeout: 10_000 }).toBeGreaterThanOrEqual(count);
    await expectRenderedEdges(page, 4);
  }
  expect(outcomeRequests['review-research']).toBe(1);
  expect(outcomeRequests['check-research']).toBe(1);
  await reviewNode.click();
  await expect(page.getByLabel('执行详情', { exact: true })).toContainText('预检未通过');
  await expectRenderedEdges(page, 4);
  await page.screenshot({ path: 'test-results/review-outcomes-desktop.png' });
  phase = 'blocked';
  await expect(reviewNode).toContainText('需要人工处理', { timeout: 10_000 });
  await expectRenderedEdges(page, 4);
  phase = 'missing';
  await expect(reviewNode).toContainText('结论未获取', { timeout: 10_000 });
  await expectRenderedEdges(page, 4);
  phase = 'pass';
  await expect(reviewNode).toContainText('评审通过', { timeout: 10_000 });
  await expect(page.locator('.execution-summary')).toContainText('已完成');
  await expectRenderedEdges(page, 4);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect.poll(async () => {
    const canvas = await page.getByTestId('execution-canvas').boundingBox();
    const bounds = await page.locator('.execution-node').evaluateAll(items => items.map(item => {
      const rect = item.getBoundingClientRect();
      return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom };
    }));
    return !!canvas && bounds.every(rect => rect.left >= canvas.x && rect.right <= canvas.x + canvas.width
      && rect.top >= canvas.y && rect.bottom <= canvas.y + canvas.height);
  }).toBe(true);
  await expectRenderedEdges(page, 4);
  await page.screenshot({ path: 'test-results/review-outcomes-mobile.png', fullPage: true });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test('start and stop a real run on its pinned graph, desktop and mobile', async ({ page }) => {
  const graphId = 'browser-execution';
  const definition = { graph_id: graphId, name: 'Cost model 文献调研', nodes: [
    { id: 'plan', type: 'agent', name: 'Plan and refine', agent_ref: 'agents.researcher' },
    { id: 'research', type: 'agent', name: 'Search, read and synthesize', agent_ref: 'agents.researcher' },
    { id: 'review', type: 'agent', name: 'Independent scholarly review', agent_ref: 'agents.reviewer' },
    { id: 'report', type: 'artifact', name: 'Export paper.md' },
  ], edges: [{ source: 'plan', target: 'research' }, { source: 'research', target: 'review' }, { source: 'review', target: 'report' }], metadata: { run_timeout_seconds: '1800', max_rounds: '3' } };
  expect((await page.request.put(`/api/graphs/${graphId}/draft`, { headers, data: { expected_revision: 0, definition } })).ok()).toBeTruthy();
  const version = await (await page.request.post(`/api/graphs/${graphId}/publish`, { headers, data: { expected_revision: 1 } })).json();
  await page.goto('/');
  await page.getByLabel('API Token').fill(token);
  await page.getByRole('button', { name: '连接', exact: true }).click();
  await page.getByRole('button', { name: new RegExp(graphId) }).click();
  await page.getByRole('button', { name: '执行', exact: true }).click();
  await expect(page.locator('.execution-node')).toHaveCount(4);
  await page.getByRole('button', { name: '发起运行' }).click();
  // Opening the admission form must not keep displaying the prior Run's
  // elapsed time or execution snapshot.
  await expect(page.locator('.execution-summary')).toHaveCount(0);
  await page.getByLabel('研究主题 / 任务目标').fill('编译优化 autotune cost model 研究进展');
  await page.getByRole('button', { name: '确认运行' }).click();
  await expect(page.locator('.execution-summary')).toContainText(/已接纳|已排队/);
  const runs = await (await page.request.get(`/api/runs?graph_id=${graphId}`, { headers })).json();
  expect(runs).toHaveLength(1);
  expect(runs[0].graph_version_id).toBe(version.graph_version_id);
  await expect(page.locator('.execution-budget')).toContainText('30 分钟');
  // Operator pause stops new claims; resume returns the run to running.
  await expect(page.getByRole('button', { name: '暂停运行', exact: true })).toBeVisible();
  page.once('dialog', dialog => dialog.accept());
  await page.getByRole('button', { name: '暂停运行', exact: true }).click();
  await expect(page.locator('.execution-summary')).toContainText('已暂停');
  await page.getByRole('button', { name: '恢复运行', exact: true }).click();
  await expect(page.locator('.execution-summary')).toContainText(/执行中|已排队/);
  await page.locator('.execution-node').filter({ hasText: 'Independent scholarly review' }).click();
  await expect(page.getByLabel('执行详情', { exact: true })).toBeVisible();
  await expect(page.getByLabel('执行详情', { exact: true })).toContainText('等待依赖');
  await expect.poll(async () => {
    const canvas = await page.getByTestId('execution-canvas').boundingBox();
    const bounds = await page.locator('.execution-node').filter({ hasText: 'Independent scholarly review' }).boundingBox();
    return !!canvas && !!bounds && bounds.x + bounds.width <= canvas.x + canvas.width;
  }).toBe(true);
  await page.screenshot({ path: 'test-results/execution-desktop.png' });
  page.once('dialog', dialog => dialog.accept());
  await page.getByRole('button', { name: '停止运行', exact: true }).click();
  await expect(page.locator('.execution-summary')).toContainText('已取消');
  await expect(page.getByRole('button', { name: '停止运行', exact: true })).toHaveCount(0);
  // Terminal Runs must retain the published graph topology for postmortem
  // inspection; node state changes must not remove its connections.
  await expectRenderedEdges(page, 3);
  await page.getByRole('button', { name: '发起运行' }).click();
  await page.getByLabel('研究主题 / 任务目标').fill('第二次运行用于验证画布实例隔离');
  await page.getByRole('button', { name: '确认运行' }).click();
  await expect(page.locator('.execution-summary')).toContainText(/已接纳|已排队/);
  await expectRenderedEdges(page, 3);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByTestId('execution-canvas')).toBeVisible();
  await expect.poll(async () => {
    const bounds = await page.locator('.execution-node').filter({ hasText: 'Independent scholarly review' }).boundingBox();
    return !!bounds && bounds.x + bounds.width <= 390;
  }).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.screenshot({ path: 'test-results/execution-mobile.png', fullPage: true });
  await page.reload();
  const nav = page.getByRole('navigation', { name: '工作区面板' });
  await nav.getByRole('button', { name: '工作流', exact: true }).click();
  await page.getByRole('button', { name: new RegExp(graphId) }).click();
  await expect(page.getByTestId('execution-canvas')).toBeVisible();
  await expect(page.locator('.execution-summary')).toContainText(/已接纳|已排队/);
  await expectRenderedEdges(page, 3);
});

test('running timer uses the server clock when the browser is ahead', async ({ page }) => {
  const graphId = 'browser-clock-skew';
  const definition = { graph_id: graphId, name: 'Clock skew regression', nodes: [
    { id: 'plan', type: 'agent', name: 'Plan and refine', agent_ref: 'agents.researcher' },
    { id: 'research', type: 'agent', name: 'Research', agent_ref: 'agents.researcher' },
  ], edges: [{ source: 'plan', target: 'research' }] };
  expect((await page.request.put(`/api/graphs/${graphId}/draft`, { headers, data: { expected_revision: 0, definition } })).ok()).toBeTruthy();
  const version = await (await page.request.post(`/api/graphs/${graphId}/publish`, { headers, data: { expected_revision: 1 } })).json();
  const triggerId = crypto.randomUUID();
  expect((await page.request.put(`/api/triggers/${triggerId}`, { headers, data: { graph_version_id: version.graph_version_id } })).ok()).toBeTruthy();
  const receipt = await (await page.request.post(`/api/triggers/${triggerId}/runs`, {
    headers: { ...headers, 'Idempotency-Key': 'clock-skew-run' }, data: { objective: 'Clock test' },
  })).json();
  const sourceRun = await (await page.request.get(`/api/runs/${receipt.run_id}`, { headers })).json();
  const acquiredAt = new Date().toISOString();
  const fakeRun = { ...sourceRun, status: 'running', created_at: acquiredAt, updated_at: acquiredAt };
  let phase: 'plan' | 'research' = 'plan';

  await page.addInitScript(() => {
    const nativeNow = Date.now.bind(Date);
    Date.now = () => nativeNow() + 398_000;
  });
  await page.route('**/api/**', async route => {
    const url = new URL(route.request().url());
    const runBase = `/api/runs/${sourceRun.id}`;
    const response = async (json: unknown) => route.fulfill({ response: await route.fetch(), json });
    if (url.pathname === '/api/runs' && url.searchParams.get('graph_id') === graphId) return response([fakeRun]);
    if (url.pathname === runBase) return response(fakeRun);
    if (url.pathname === `${runBase}/nodes`) return response(phase === 'plan' ? [
      { id: 'clock-plan-node', node_id: 'plan', attempt: 0, status: 'running', created_at: acquiredAt, updated_at: acquiredAt },
    ] : [
      { id: 'clock-plan-node', node_id: 'plan', attempt: 0, status: 'completed', created_at: acquiredAt, updated_at: acquiredAt },
      { id: 'clock-research-node', node_id: 'research', attempt: 0, status: 'running', created_at: acquiredAt, updated_at: acquiredAt },
    ]);
    if (url.pathname === '/api/leases/active' && url.searchParams.get('run_id') === sourceRun.id) return response([
      { state: 'healthy', recoverable: false, reason: '', lease: { node_run_id: phase === 'plan' ? 'clock-plan-node' : 'clock-research-node', claim_id: 'clock-test-claim', acquired_at: acquiredAt, heartbeat_at: acquiredAt } },
    ]);
    if (url.pathname === `${runBase}/decisions`) return response(phase === 'plan' ? [] : [
      { edge_index: 0, source_attempt: 0, selected: true, decided_at: acquiredAt },
    ]);
    if ([`${runBase}/operations`, `${runBase}/contexts`].includes(url.pathname)) return response([]);
    return route.continue();
  });

  await page.goto('/');
  await page.getByLabel('API Token').fill(token);
  await page.getByRole('button', { name: '连接', exact: true }).click();
  await page.getByRole('button', { name: new RegExp(graphId) }).click();
  await page.getByRole('button', { name: '执行', exact: true }).click();
  await expect(page.locator('.execution-node').filter({ hasText: 'Plan and refine' }).locator('small')).toHaveText(/已执行 0:0[0-5]/);
  await expectRenderedEdges(page, 1);

  phase = 'research';
  await expect(page.locator('.execution-node').filter({ hasText: 'Research' }).locator('small')).toHaveText(/已执行 0:0[0-9]/, { timeout: 5000 });
  await expect(page.locator('.execution-node').filter({ hasText: 'Plan and refine' })).toContainText('已完成');
  await expectRenderedEdges(page, 1);
});
