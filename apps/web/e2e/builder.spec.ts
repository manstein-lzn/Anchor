import { test, expect, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';

const token = 'anchor-browser-tests-only-not-a-real-secret';
const headers = { Authorization: `Bearer ${token}` };

async function login(page: Page) {
  await page.goto('/');
  await page.getByLabel('API Token').fill(token);
  await page.getByRole('button', { name: '连接', exact: true }).click();
  await expect(page.getByLabel('API Token')).toBeHidden();
}
async function mobileTab(page: Page, name: string) {
  const nav = page.getByRole('navigation', { name: '工作区面板' });
  if (await nav.isVisible()) await nav.getByRole('button', { name, exact: true }).click();
}
async function create(page: Page, id: string) {
  await mobileTab(page, '属性');
  await page.getByLabel('工作流名称', { exact: true }).fill('持续研究与证据交付');
  await page.getByLabel('Graph ID', { exact: true }).fill(id);
}
async function add(page: Page, kind: string, name: string, reference?: string) {
  await mobileTab(page, '画布');
  await page.getByRole('button', { name: '添加节点', exact: true }).first().click();
  await page.getByRole('button', { name: `添加 ${kind} 节点`, exact: true }).click();
  await page.getByLabel('名称', { exact: true }).fill(name);
  if (reference) await page.getByLabel(kind === 'Agent' ? 'Agent 引用' : '验证器引用', { exact: true }).fill(reference);
}
async function connect(page: Page, source: string, target: string) {
  await page.getByLabel('连接起点').selectOption({ label: source });
  await page.getByLabel('连接终点').selectOption({ label: target });
  await page.getByRole('button', { name: '添加连线', exact: true }).click();
}
async function save(page: Page) {
  await mobileTab(page, '画布');
  await page.getByRole('button', { name: '保存', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('草稿已保存');
}
async function noHorizontalOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
}

test('real API lifecycle, layout and mapping round-trip, immutable versions', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await login(page);
  await create(page, 'browser-lifecycle');
  await add(page, 'Agent', '研究员', 'agents.researcher');
  await add(page, 'Agent', '交叉审查', 'agents.reviewer');
  await add(page, '验证', '证据验证', 'verifiers.evidence');
  await add(page, '产物', '研究报告');
  await connect(page, '研究员', '交叉审查');
  await page.getByRole('button', { name: '连线 JSON / 输入映射' }).click();
  const mappingEditor = page.getByRole('textbox', { name: '连线 JSON', exact: true });
  const edge = JSON.parse(await mappingEditor.inputValue());
  await mappingEditor.fill(JSON.stringify({ ...edge, condition: 'evidence_available', input_mapping: { evidence: 'research.artifact' } }));
  await page.getByRole('button', { name: '应用', exact: true }).click();
  await connect(page, '交叉审查', '证据验证');
  await connect(page, '证据验证', '研究报告');
  await page.getByRole('button', { name: '校验', exact: true }).click();
  await expect(page.getByText('结构校验通过', { exact: true })).toBeVisible();
  const node = page.locator('.graph-node').first();
  const beforeDrag = await node.boundingBox();
  await page.mouse.move(beforeDrag!.x + 50, beforeDrag!.y + 35);
  await page.mouse.down();
  await page.mouse.move(beforeDrag!.x + 50, beforeDrag!.y - 15, { steps: 8 });
  await page.mouse.up();
  await expect.poll(async () => (await node.boundingBox())!.y).toBeLessThan(beforeDrag!.y - 30);
  // Regression: dragging must never drop connections. Edge types are a
  // stable module constant so high-frequency drag renders cannot unregister them.
  await expect(page.locator('.react-flow__edge')).toHaveCount(3);
  await page.getByRole('button', { name: '撤销', exact: true }).click();
  await expect.poll(async () => Math.abs((await node.boundingBox())!.y - beforeDrag!.y)).toBeLessThan(1);
  await page.getByRole('button', { name: '重做', exact: true }).click();
  await expect.poll(async () => (await node.boundingBox())!.y).toBeLessThan(beforeDrag!.y - 30);
  await save(page);
  const stored = await (await page.request.get('/api/graphs/browser-lifecycle/draft', { headers })).json();
  expect(stored.definition.edges[0].input_mapping).toEqual({ evidence: 'research.artifact' });
  expect(Object.keys(stored.layout.positions)).toHaveLength(4);
  expect(stored.definition.nodes[0]).not.toHaveProperty('position');
  await page.getByRole('button', { name: '发布', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('已发布 v1');
  await page.getByRole('button', { name: '工作流属性', exact: true }).click();
  await page.getByLabel('工作流名称').fill('下一版草稿');
  await page.getByRole('button', { name: '版本', exact: true }).click();
  await page.getByRole('button', { name: /v1 · 持续研究与证据交付/ }).click();
  await expect(page.getByRole('button', { name: '保存', exact: true })).toBeDisabled();
  await expect(page.getByRole('heading', { name: '持续研究与证据交付', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '返回草稿', exact: true }).click();
  await expect(page.getByLabel('工作流名称')).toHaveValue('下一版草稿');
  await page.getByRole('button', { name: '撤销', exact: true }).click();
  await expect(page.getByLabel('工作流名称')).toHaveValue('持续研究与证据交付');
  await page.getByRole('button', { name: '适应画布', exact: true }).click();
  await noHorizontalOverflow(page);
  await page.screenshot({ path: 'test-results/builder-desktop.png' });
  await page.reload();
  await page.getByRole('button', { name: /持续研究与证据交付 browser-lifecycle/ }).click();
  await expect(page.locator('.graph-node')).toHaveCount(4);
  const reloaded = await (await page.request.get('/api/graphs/browser-lifecycle/draft', { headers })).json();
  expect(reloaded.layout).toEqual(stored.layout);
  expect(reloaded.definition.edges).toEqual(stored.definition.edges);
  expect(errors).toEqual([]);
});

test('stale revisions never overwrite remote changes', async ({ page }) => {
  await login(page); await create(page, 'browser-conflict');
  await add(page, '产物', '本地产物'); await save(page);
  const remote = await (await page.request.get('/api/graphs/browser-conflict/draft', { headers })).json();
  const response = await page.request.put('/api/graphs/browser-conflict/draft', { headers, data: {
    expected_revision: remote.revision, definition: { ...remote.definition, name: '远端已修改' }, layout: remote.layout,
  } });
  expect(response.ok()).toBe(true);
  await page.getByLabel('名称', { exact: true }).fill('未保存本地修改');
  await page.getByRole('button', { name: '保存', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('修订冲突');
  await expect(page.getByLabel('名称', { exact: true })).toHaveValue('未保存本地修改');
  await expect(page.getByRole('button', { name: '发布', exact: true })).toBeDisabled();
  const current = await (await page.request.get('/api/graphs/browser-conflict/draft', { headers })).json();
  expect(current.definition.name).toBe('远端已修改');
  page.once('dialog', dialog => void dialog.accept());
  await page.getByRole('button', { name: '重新载入服务器草稿', exact: true }).click();
  await expect(page.getByLabel('工作流名称')).toHaveValue('远端已修改');
});

test('schema errors, undo and redo, deletion, and invalid import preserve editing', async ({ page }) => {
  await login(page); await create(page, 'browser-validation');
  await add(page, 'Agent', '未配置引用');
  await page.getByRole('button', { name: '校验', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('requires agent_ref');
  await page.getByRole('button', { name: '复制节点', exact: true }).click();
  await expect(page.locator('.graph-node')).toHaveCount(2);
  await page.getByRole('button', { name: '删除节点', exact: true }).click();
  await expect(page.locator('.graph-node')).toHaveCount(1);
  await page.getByRole('button', { name: '撤销', exact: true }).click();
  await expect(page.locator('.graph-node')).toHaveCount(2);
  await page.getByRole('button', { name: '重做', exact: true }).click();
  await expect(page.locator('.graph-node')).toHaveCount(1);
  await page.locator("input[aria-label='导入 Graph JSON']").setInputFiles({ name: 'invalid.json', mimeType: 'application/json', buffer: Buffer.from('{"nodes":null}') });
  await expect(page.getByRole('alert')).toContainText('Graph 需要');
  await expect(page.locator('.graph-node')).toHaveCount(1);
  page.once('dialog', dialog => void dialog.dismiss());
  await page.getByRole('button', { name: '新建工作流', exact: true }).click();
  await expect(page.locator('.graph-node')).toHaveCount(1);
});

test('a lost save acknowledgement recovers through revision conflict without duplicates', async ({ page }) => {
  await login(page); await create(page, 'browser-lost-ack'); await add(page, '产物', '可靠落盘');
  await page.route('**/api/graphs/browser-lost-ack/draft', async route => {
    if (route.request().method() === 'PUT') { await route.fetch(); await route.abort('failed'); }
    else await route.continue();
  });
  await page.getByRole('button', { name: '保存', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('无法连接 API');
  await page.unroute('**/api/graphs/browser-lost-ack/draft');
  await page.getByRole('button', { name: '保存', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('修订冲突');
  const draft = await (await page.request.get('/api/graphs/browser-lost-ack/draft', { headers })).json();
  expect(draft.revision).toBe(1);
  expect(draft.definition.nodes).toHaveLength(1);
});

test('mobile graph authoring, configuration and publication', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page); await create(page, 'browser-mobile');
  await add(page, 'Agent', '长期研究 Agent', 'agents.researcher');
  await add(page, '产物', '研究成果');
  await connect(page, '长期研究 Agent', '研究成果');
  await save(page);
  await page.getByRole('button', { name: '发布', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('已发布 v1');
  await page.getByRole('button', { name: '适应画布', exact: true }).click();
  await noHorizontalOverflow(page);
  await page.screenshot({ path: 'test-results/builder-mobile.png' });
  await mobileTab(page, '属性');
  await noHorizontalOverflow(page);
  await page.screenshot({ path: 'test-results/inspector-mobile.png' });
  await mobileTab(page, '工作流');
  await expect(page.getByRole('button', { name: /持续研究与证据交付 browser-mobile/ })).toBeVisible();
  await noHorizontalOverflow(page);
});

test('unauthenticated API is not bypassed by the development proxy', async ({ page }) => {
  const response = await page.request.get('/api/graphs');
  expect(response.status()).toBe(401);
  await page.goto('/');
  await page.getByLabel('API Token').fill('invalid');
  await page.getByRole('button', { name: '连接', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('认证失效');
  expect(await page.evaluate(() => sessionStorage.getItem('anchor-token'))).toBeNull();
});

test('run console shows only persisted run, node, event and operation state', async ({ page }) => {
  const graphId = 'browser-run-console';
  const definition = { graph_id: graphId, name: '长期研究运行图', nodes: [
    { id: 'research', name: '研究 Agent', type: 'agent', agent_ref: 'agents.researcher' },
    { id: 'report', name: '报告产物', type: 'artifact' },
  ], edges: [{ source: 'research', target: 'report' }] };
  expect((await page.request.put(`/api/graphs/${graphId}/draft`, { headers, data: {
    expected_revision: 0, definition, layout: {},
  }})).ok()).toBe(true);
  const version = await (await page.request.post(`/api/graphs/${graphId}/publish`, { headers, data: { expected_revision: 1 } })).json();
  const trigger = crypto.randomUUID();
  expect((await page.request.put(`/api/triggers/${trigger}`, { headers, data: {
    graph_version_id: version.graph_version_id, type: 'manual', enabled: true,
  }})).ok()).toBe(true);
  const admitted = await page.request.post(`/api/triggers/${trigger}/runs`, {
    headers: { ...headers, 'Idempotency-Key': 'browser-console-occurrence' },
    data: { objective: '追踪多月研究任务的证据状态', inputs: { topic: 'agent reliability' } },
  });
  expect(admitted.status()).toBe(202);
  await login(page);
  await page.getByLabel('产品视图').getByRole('button', { name: '运行', exact: true }).click();
  await expect(page.getByRole('heading', { name: '追踪多月研究任务的证据状态' })).toBeVisible();
  await expect(page.locator('.run-header').getByText(/已接纳|已排队/)).toBeVisible();
  await expect(page.locator('.node-state-row')).toHaveCount(2);
  // The disposable API accepts the dispatch, so the entry node is ready while
  // its downstream still waits for dependencies.
  await expect(page.locator('.node-state-row').filter({ hasText: '研究 Agent' })).toContainText('等待执行');
  await expect(page.locator('.node-state-row').filter({ hasText: '报告产物' })).toContainText('等待依赖');
  await expect(page.locator('.event-row').first()).toContainText('run.requested');
  await expect(page.getByText('尚无工具副作用操作')).toBeVisible();
  await noHorizontalOverflow(page);
  await page.screenshot({ path: 'test-results/run-console-desktop.png' });
  await page.setViewportSize({ width: 390, height: 844 });
  await noHorizontalOverflow(page);
  await page.screenshot({ path: 'test-results/run-console-mobile.png', fullPage: true });
});

test('import and export preserve full configuration, and layouts fit small screens', async ({ page }) => {
  await login(page);
  const document = {
    definition: {
      graph_id: 'browser-import', name: '证据溯源工作流', entry_node_id: 'research', metadata: { owner: 'test' },
      nodes: [
        { id: 'research', name: '可回溯的长期研究任务', type: 'agent', agent_ref: 'agents.researcher', input_schema: 'task-v1', metadata: { skill: 'research' }, timeout_seconds: null },
        { id: 'report', name: '交付报告', type: 'artifact' },
      ],
      edges: [{ source: 'research', target: 'report', condition: 'evidence_ready_and_verified', input_mapping: { report: 'research.artifact' } }],
    },
    layout: { positions: { research: { x: 0, y: 0 }, report: { x: 320, y: 0 } }, extra: { preserved: true } },
  };
  await page.locator("input[aria-label='导入 Graph JSON']").setInputFiles({ name: 'graph.json', mimeType: 'application/json', buffer: Buffer.from(JSON.stringify(document)) });
  await expect(page.locator('.graph-node')).toHaveCount(2);
  await save(page);
  const downloadPromise = page.waitForEvent('download');
  await page.getByRole('button', { name: '导出 Graph JSON', exact: true }).click();
  const download = await downloadPromise;
  expect(JSON.parse(await readFile((await download.path())!, 'utf8'))).toEqual(document);
  for (const width of [2560, 1024, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await mobileTab(page, '画布');
    await page.getByRole('button', { name: '适应画布', exact: true }).click();
    await noHorizontalOverflow(page);
    const canvas = await page.getByTestId('canvas').boundingBox();
    for (const item of await page.locator('.graph-node').all()) {
      await expect.poll(async () => {
        const bounds = await item.boundingBox();
        return bounds!.x >= canvas!.x && bounds!.x + bounds!.width <= canvas!.x + canvas!.width + 1;
      }).toBe(true);
    }
    await page.screenshot({ path: `test-results/builder-${width}.png` });
  }
});

test('trigger management pins a published version and never shows secrets', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await login(page);
  await create(page, 'browser-triggers');
  await add(page, 'Agent', '研究员', 'agents.researcher');
  await save(page);
  await page.getByRole('button', { name: '发布', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('已发布 v1');
  await page.getByRole('button', { name: '版本', exact: true }).click();

  const manager = page.getByRole('region', { name: '触发器管理' });
  await expect(manager).toBeVisible();
  await manager.getByLabel('触发器类型').selectOption('cron');
  await manager.getByLabel('Cron 表达式').fill('0 9 * * 1');
  await manager.getByRole('button', { name: '注册触发器' }).click();
  await expect(manager).toContainText('定时 (cron)');
  await expect(manager).toContainText('0 9 * * 1');

  // A webhook trigger stores only a secret reference, never a value.
  await manager.getByLabel('触发器类型').selectOption('webhook');
  await manager.getByLabel('事件类型').fill('paper.ready');
  await manager.getByLabel('Webhook secret 引用').fill('WEBHOOK_SIGNING_KEY');
  await manager.getByRole('button', { name: '注册触发器' }).click();
  await expect(manager).toContainText('Webhook');
  await expect(manager).toContainText('paper.ready');
  await expect(manager).toContainText('secret: WEBHOOK_SIGNING_KEY');

  // Disable one trigger; the toggle is persisted through the API.
  const rows = manager.locator('.ledger-row');
  await expect(rows).toHaveCount(2);
  await rows.first().getByRole('button', { name: '停用' }).click();
  await expect(rows.first()).toContainText('已停用');
  expect(errors).toEqual([]);
});

test('run console filters and archives runs without deleting evidence', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  const graphId = 'browser-run-archive';
  const definition = { graph_id: graphId, name: '归档验证图', nodes: [
    { id: 'a', name: '研究', type: 'agent', agent_ref: 'agents.researcher' },
  ], edges: [] };
  expect((await page.request.put(`/api/graphs/${graphId}/draft`, { headers, data: { expected_revision: 0, definition, layout: {} } })).ok()).toBe(true);
  const version = await (await page.request.post(`/api/graphs/${graphId}/publish`, { headers, data: { expected_revision: 1 } })).json();
  const trigger = crypto.randomUUID();
  expect((await page.request.put(`/api/triggers/${trigger}`, { headers, data: {
    graph_version_id: version.graph_version_id, type: 'manual', enabled: true,
  }})).ok()).toBe(true);
  const first = await (await page.request.post(`/api/triggers/${trigger}/runs`, {
    headers: { ...headers, 'Idempotency-Key': 'archive-e2e-1' },
    data: { objective: '归档目标 A', inputs: {} },
  })).json();
  const second = await (await page.request.post(`/api/triggers/${trigger}/runs`, {
    headers: { ...headers, 'Idempotency-Key': 'archive-e2e-2' },
    data: { objective: '归档目标 B', inputs: {} },
  })).json();
  expect((await page.request.post(`/api/runs/${first.run_id}/stop`, { headers, data: { reason: 'e2e stop' } })).ok()).toBe(true);

  await login(page);
  await page.getByLabel('产品视图').getByRole('button', { name: '运行', exact: true }).click();
  const items = page.locator('.run-list-item');

  // The database is shared with earlier tests, so isolate our own runs by id.
  await page.getByLabel('搜索运行 ID').fill(first.run_id.slice(0, 8));
  await expect(items).toHaveCount(1);
  await expect(items.first()).toContainText('已取消');
  await page.getByLabel('按状态筛选运行').selectOption('active');
  await expect(items).toHaveCount(0);
  await page.getByLabel('按状态筛选运行').selectOption('all');
  await expect(items).toHaveCount(1);

  await items.first().click();
  await expect(page.getByRole('heading', { name: '归档目标 A' })).toBeVisible();

  // Archiving hides the run from the default list; the evidence stays readable.
  await page.getByRole('button', { name: '归档', exact: true }).click();
  await expect(items).toHaveCount(0);
  expect((await page.request.get(`/api/runs/${first.run_id}`, { headers })).ok()).toBe(true);
  await expect(page.getByRole('heading', { name: '归档目标 A' })).toBeVisible();

  await page.getByLabel('显示已归档').check();
  await expect(items).toHaveCount(1);
  await expect(items.first()).toContainText('已归档');
  await page.getByRole('button', { name: '取消归档', exact: true }).click();
  await expect(items.first()).not.toContainText('已归档');

  // The sibling run is untouched by archiving.
  await page.getByLabel('搜索运行 ID').fill(second.run_id.slice(0, 8));
  await expect(items).toHaveCount(1);
  await expect(items.first()).toContainText('已排队');
  expect(errors).toEqual([]);
});

test('storage budgets are adjustable at runtime from the web', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  const graphId = 'browser-storage-budget';
  const definition = { graph_id: graphId, name: '预算验证图', nodes: [
    { id: 'a', name: '研究', type: 'agent', agent_ref: 'agents.researcher' },
  ], edges: [] };
  expect((await page.request.put(`/api/graphs/${graphId}/draft`, { headers, data: { expected_revision: 0, definition, layout: {} } })).ok()).toBe(true);
  const version = await (await page.request.post(`/api/graphs/${graphId}/publish`, { headers, data: { expected_revision: 1 } })).json();
  const trigger = crypto.randomUUID();
  expect((await page.request.put(`/api/triggers/${trigger}`, { headers, data: {
    graph_version_id: version.graph_version_id, type: 'manual', enabled: true,
  }})).ok()).toBe(true);
  expect((await page.request.post(`/api/triggers/${trigger}/runs`, {
    headers: { ...headers, 'Idempotency-Key': 'storage-e2e-1' }, data: { objective: '预算目标', inputs: {} },
  })).status()).toBe(202);

  await login(page);
  await page.getByLabel('产品视图').getByRole('button', { name: '存储', exact: true }).click();
  await expect(page.getByRole('heading', { name: '存储预算' })).toBeVisible();
  await expect(page.locator('.storage-card').first()).toContainText('总占用');

  // Global knob: takes effect immediately and shows the usage ratio.
  await page.getByLabel('全局存储预算').fill('0.5');
  await page.getByRole('button', { name: '保存全局预算' }).click();
  await expect(page.getByRole('status')).toContainText('全局预算已更新');
  await expect(page.locator('.storage-card').filter({ hasText: '全局预算' })).toContainText('已用');
  expect((await (await page.request.get('/api/storage/budget', { headers })).json()).global_bytes).toBe(Math.round(0.5 * 1024 ** 3));

  // Per-graph knob: the graph appears once it has runs, and persists.
  const row = page.locator('.storage-row').filter({ hasText: graphId });
  await expect(row).toBeVisible();
  await row.getByLabel(`${graphId} 存储预算`).fill('0.25');
  await row.getByRole('button', { name: '保存' }).click();
  await expect(page.getByRole('status')).toContainText('预算已更新');
  const budgets = await (await page.request.get('/api/storage/budget', { headers })).json();
  expect(budgets.graphs[graphId]).toBe(Math.round(0.25 * 1024 ** 3));

  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  expect(errors).toEqual([]);
});

test('rolling cleanup previews and evicts finished runs', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  const graphId = 'browser-retention';
  const definition = { graph_id: graphId, name: '滚动清理图', nodes: [
    { id: 'a', name: '研究', type: 'agent', agent_ref: 'agents.researcher' },
  ], edges: [] };
  expect((await page.request.put(`/api/graphs/${graphId}/draft`, { headers, data: { expected_revision: 0, definition, layout: {} } })).ok()).toBe(true);
  const version = await (await page.request.post(`/api/graphs/${graphId}/publish`, { headers, data: { expected_revision: 1 } })).json();
  const trigger = crypto.randomUUID();
  expect((await page.request.put(`/api/triggers/${trigger}`, { headers, data: {
    graph_version_id: version.graph_version_id, type: 'manual', enabled: true,
  }})).ok()).toBe(true);
  const admitted = await (await page.request.post(`/api/triggers/${trigger}/runs`, {
    headers: { ...headers, 'Idempotency-Key': 'retention-e2e-1' }, data: { objective: '清理目标', inputs: {} },
  })).json();
  // Only terminal runs are evictable, so finish ours first.
  expect((await page.request.post(`/api/runs/${admitted.run_id}/stop`, { headers, data: { reason: 'e2e finish' } })).ok()).toBe(true);

  await login(page);
  await page.getByLabel('产品视图').getByRole('button', { name: '存储', exact: true }).click();
  await expect(page.getByRole('heading', { name: '存储预算' })).toBeVisible();

  // A budget below the current footprint makes the preview actionable.
  await page.getByLabel('全局存储预算').fill('0.000000001');
  await page.getByRole('button', { name: '保存全局预算' }).click();
  await expect(page.getByRole('status')).toContainText('全局预算已更新');
  await page.getByRole('button', { name: '预览清理' }).click();
  await expect(page.locator('.retention-plan')).toContainText('已超出预算');
  await expect(page.locator('.retention-plan')).toContainText('最老优先');

  // Confirming the sweep permanently removes the finished runs.
  page.once('dialog', dialog => dialog.accept());
  await page.getByRole('button', { name: '立即清理' }).click();
  await expect(page.getByRole('status')).toContainText('已清理');
  await expect(page.locator('.storage-retention .ledger-row').first()).toContainText('条运行');
  // Our finished run is gone; still-running runs stay protected.
  expect((await page.request.get(`/api/runs/${admitted.run_id}`, { headers })).status()).toBe(404);

  // Leave the shared test database without a budget so later specs start clean.
  await page.getByLabel('全局存储预算').fill('');
  await page.getByRole('button', { name: '保存全局预算' }).click();
  await expect(page.getByRole('status')).toContainText('全局预算已更新');
  expect(errors).toEqual([]);
});
