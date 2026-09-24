import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

test('manage individual workflows without disturbing the editor', async ({ page }) => {
  const graph = JSON.parse(readFileSync('../../examples/graphs/academic-gated.json', 'utf8'));
  let graphs = [
    { graph: 'academic-survey', running: null as string | null },
    { graph: 'research-review', running: null as string | null },
    { graph: 'running-research', running: 'run-active' as string | null },
  ];
  const deleted: string[] = [];
  let refuseDelete = true;
  await page.route('**/graphs', route => route.fulfill({ json: { graphs } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [] } }));
  await page.route('**/graphs/*', route => {
    if (route.request().method() !== 'DELETE') return route.fulfill({ json: { definition: graph } });
    if (refuseDelete) return route.fulfill({ status: 409, json: { error: '工作流正在运行' } });
    const name = decodeURIComponent(new URL(route.request().url()).pathname.split('/').at(-1)!);
    deleted.push(name);
    graphs = graphs.filter(item => item.graph !== name);
    return route.fulfill({ json: { graph: name, deleted: true } });
  });
  await page.goto('/');
  await expect(page.getByLabel('目标', { exact: true })).toHaveValue(graph.objective);
  await page.getByLabel('目标', { exact: true }).fill('尚未保存的研究目标');
  const manage = page.getByRole('button', { name: '管理工作流 research-review', exact: true });
  await manage.focus();
  await page.keyboard.press('Enter');
  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  await expect(page.getByLabel('当前工作流')).toHaveValue('academic-survey');
  await page.screenshot({ path: 'test-results/workflow-menu-desktop.png' });
  await page.keyboard.press('Escape');
  await expect(menu).not.toBeVisible();
  await expect(manage).toBeFocused();
  await manage.click();
  await page.getByRole('heading', { name: '工作流设置', exact: true }).click();
  await expect(menu).not.toBeVisible();
  await manage.click();
  await page.keyboard.press('ArrowDown');
  await expect(page.getByRole('menuitem', { name: '删除工作流' })).toBeFocused();
  await page.keyboard.press('Enter');
  const dialog = page.getByRole('dialog', { name: '删除工作流？' });
  await expect(dialog.getByText('research-review', { exact: true })).toBeVisible();
  await expect(dialog.getByText('所有运行的工作区文件、产物和 Git 历史')).toBeVisible();
  await expect(dialog.getByRole('button', { name: '取消' })).toBeFocused();
  await page.screenshot({ path: 'test-results/workflow-delete-desktop.png' });
  await page.keyboard.press('Escape');
  await expect(dialog).not.toBeVisible();
  await expect(manage).toBeFocused();
  expect(deleted).toEqual([]);
  await manage.click();
  await page.getByRole('menuitem', { name: '删除工作流' }).click();
  await dialog.getByRole('button', { name: '永久删除' }).click();
  await expect(dialog.getByRole('alert')).toContainText('工作流正在运行');
  await expect(page.getByLabel('目标', { exact: true })).toHaveValue('尚未保存的研究目标');
  refuseDelete = false;
  await dialog.getByRole('button', { name: '永久删除' }).click();
  await expect(dialog).not.toBeVisible();
  await expect(manage).toHaveCount(0);
  expect(deleted).toEqual(['research-review']);
  await expect(page.getByLabel('当前工作流')).toHaveValue('academic-survey');
  await expect(page.getByLabel('目标', { exact: true })).toHaveValue('尚未保存的研究目标');

  await page.getByRole('button', { name: '管理工作流 running-research', exact: true }).click();
  await expect(page.getByRole('menuitem', { name: '删除工作流' })).toBeDisabled();
  await expect(menu).toContainText('请先停止运行');
  await page.keyboard.press('Escape');

  // Touch-sized layout: actions stay visible and the popover escapes the scrolling sidebar.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.getByRole('button', { name: '管理工作流 academic-survey', exact: true }).click();
  await expect(menu).toBeInViewport();
  await page.screenshot({ path: 'test-results/workflow-menu-mobile.png', fullPage: true });
  await page.getByRole('menuitem', { name: '删除工作流' }).click();
  await expect(dialog.getByRole('button', { name: '取消' })).toBeFocused();
  await page.screenshot({ path: 'test-results/workflow-delete-mobile.png', fullPage: true });
  await dialog.getByRole('button', { name: '永久删除' }).click();
  await expect(page.getByLabel('当前工作流')).toHaveValue('running-research');
  await expect(page.getByText('● 未保存')).toHaveCount(0);
  graphs = graphs.map(item => ({ ...item, running: null }));
  await expect(async () => {
    await page.getByRole('button', { name: '管理工作流 running-research', exact: true }).click();
    await expect(page.getByRole('menuitem', { name: '删除工作流' })).toBeEnabled();
  }).toPass();
  await page.getByRole('menuitem', { name: '删除工作流' }).click();
  await dialog.getByRole('button', { name: '永久删除' }).click();
  await expect(page.getByText('从一个想法开始', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '运行工作流', exact: true })).toBeDisabled();
  expect(deleted).toEqual(['research-review', 'academic-survey', 'running-research']);
});

test('edit a graph, inspect a run and read its files across screen sizes', async ({ page }) => {
  const graph = JSON.parse(readFileSync('../../examples/graphs/academic-gated.json', 'utf8'));
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  const run = { run: 'run-demo', graph: 'academic-survey', status: 'running', running: true,
    started: '2026-09-22T08:30:00Z', updated: '2026-09-22T08:32:00Z',
    executed: ['plan', 'gather', 'write'], objective: '调研多智能体协作：从证据收集到可靠的研究结论' };
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [
    { graph: run.graph, running: run.run }, { graph: 'research-review', running: null },
    { graph: 'one-search', running: null },
  ] } }));
  await page.route('**/graphs/*', route => route.fulfill({ json: { definition: graph } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [run] } }));
  await page.route('**/runs/run-demo', route => route.fulfill({ json: {
    graph: run.graph, run: run.run, nodes: graph.nodes.map((node: { id: string }) => node.id),
    state: { ...run, cursor: { node: 'structure', pass: 1, dir: '' }, passes: { plan: 1, gather: 1, write: 1, structure: 1 },
      decided: { 'plan|gather': [true, 1], 'gather|write': [true, 2], 'write|structure': [true, 3] },
      nodes: Object.fromEntries(['plan', 'gather', 'write'].map(node => [node, {
        submitted: true, submission: '已完成该阶段的工作，产物已保存。', exit_status: 'Submitted',
      }])), skipped: [], error: '' },
    traces: { write: [{ role: 'assistant', text: '## 研究进展\n已整理 **12 篇论文**，现在汇总方法、评估和开放问题。' },
      { role: 'assistant', text: '', commands: ['cat paper.md'] },
      { role: 'tool', text: '# 多智能体协作\n\n## References\n1. A survey of agent systems' }] },
  } }));
  await page.route('**/runs/run-demo/files/write', route => route.fulfill({ json: {
    node: 'write', files: [{ path: 'paper.md', size: 1400 }, { path: 'sources/raw/evidence.md', size: 80 },
      { path: 'sources/notes.md', size: 40 }, { path: 'last.md', size: 20 }], truncated: false,
  } }));
  await page.route('**/runs/run-demo/files/write/paper.md', route => route.fulfill({ json: {
    path: 'paper.md', size: 1400, binary: false, truncated: false,
    text: ['# 多智能体协作', '', '这是本次运行生成的研究报告。', '', '## 方法比较', '',
      '| 方法 | 特征 |', '| --- | --- |', '| Plan | 规划与执行 |', '| ReAct | 迭代反馈 |', '',
      '- [x] 研究完成', '', '$E=mc^2$', '',
      '```mermaid', 'flowchart LR', ' A[计划] --> B[执行]', '```', '',
      '```mermaid', 'sequenceDiagram', ' 用户->>Agent: 开始', ' Agent-->>用户: 完成', '```', '',
      '```mermaid', 'this is not a diagram', '```'].join('\n'),
  } }));
  await page.route('**/runs/run-demo/files/write/sources/raw/evidence.md', route => route.fulfill({ json: {
    path: 'sources/raw/evidence.md', size: 80, binary: false, truncated: false, text: '这是嵌套目录中的证据。',
  } }));
  await page.goto('/');
  await expect(page.getByTestId('graph-canvas').locator('.graph-node')).toHaveCount(5);
  for (const [label, panel, delta] of [
    ['调整侧边栏宽度', '.library', 75], ['调整详情面板宽度', '.inspector', -100],
  ] as const) {
    const separator = page.getByRole('separator', { name: label });
    const before = (await page.locator(panel).boundingBox())!.width;
    const handle = (await separator.boundingBox())!;
    await page.mouse.move(handle.x + handle.width / 2, handle.y + 100);
    await page.mouse.down();
    await page.mouse.move(handle.x + handle.width / 2 + delta, handle.y + 100, { steps: 5 });
    await page.mouse.up();
    await expect.poll(async () => (await page.locator(panel).boundingBox())!.width).toBeGreaterThan(before + 50);
    await separator.focus();
    await page.keyboard.press(label.includes('侧边栏') ? 'ArrowLeft' : 'ArrowRight');
    await separator.dblclick();
    await expect.poll(async () => (await page.locator(panel).boundingBox())!.width).toBe(before);
  }
  await page.screenshot({ path: 'test-results/editor-desktop.png' });
  const wideHandle = page.getByRole('separator', { name: '调整详情面板宽度' });
  const wideBox = (await wideHandle.boundingBox())!;
  await page.mouse.move(wideBox.x + 4, wideBox.y + 100);
  await page.mouse.down();
  await page.mouse.move(500, wideBox.y + 100, { steps: 5 });
  await page.mouse.up();
  const wideWidth = (await page.locator('.inspector').boundingBox())!.width;
  expect(wideWidth).toBeGreaterThan(720);
  await page.reload();
  await expect.poll(async () => (await page.locator('.inspector').boundingBox())!.width).toBe(wideWidth);
  await page.getByRole('button', { name: '最大化详情面板', exact: true }).click();
  await expect(page.locator('.main')).not.toBeVisible();
  await expect.poll(async () => (await page.locator('.inspector').boundingBox())!.width).toBe(1440);
  await page.getByRole('button', { name: '恢复详情面板', exact: true }).click();
  await expect.poll(async () => (await page.locator('.inspector').boundingBox())!.width).toBe(wideWidth);
  await wideHandle.dblclick();
  await page.getByLabel('搜索工作流').fill('one-search');
  await expect(page.locator('.library-list .library-row')).toHaveCount(1);
  await page.getByLabel('搜索工作流').fill('');
  await page.getByRole('button', { name: '添加节点', exact: true }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.getByLabel('目标', { exact: true }).fill('新的研究目标');
  await expect(page.getByText('● 未保存')).toBeVisible();
  await expect(page.getByRole('button', { name: '运行工作流', exact: true })).toBeDisabled();
  page.once('dialog', dialog => dialog.dismiss());
  await page.getByLabel('当前工作流').selectOption('research-review');
  await expect(page.getByLabel('当前工作流')).toHaveValue('academic-survey');
  await expect(page.getByLabel('目标', { exact: true })).toHaveValue('新的研究目标');
  await page.getByRole('button', { name: '撤销', exact: true }).click();
  await expect(page.getByLabel('目标', { exact: true })).toHaveValue(graph.objective);
  await page.locator('.graph-node').filter({ has: page.locator('strong', { hasText: /^structure$/ }) }).click();
  await page.getByRole('button', { name: '复制节点', exact: true }).click();
  await expect(page.locator('.graph-node.kind-op')).toHaveCount(3);
  await page.getByRole('button', { name: '撤销', exact: true }).click();
  await expect(page.locator('.graph-node.kind-op')).toHaveCount(2);
  await page.getByRole('button', { name: '运行记录', exact: true }).click();
  await expect(page.getByTestId('execution-canvas').locator('.execution-node')).toHaveCount(5);
  await page.getByTestId('edge-e0-plan-gather').locator('path').first().hover({ force: true });
  await page.mouse.move(10, 10);
  await page.locator('.execution-node').filter({ has: page.locator('strong', { hasText: /^write$/ }) }).click();
  await expect(page.getByText('12 篇论文')).toBeVisible();
  await page.screenshot({ path: 'test-results/run-desktop.png' });
  await page.getByRole('button', { name: '文件', exact: true }).click();
  const detailHandle = page.getByRole('separator', { name: '调整详情面板宽度' });
  const detailWidth = (await page.locator('.inspector').boundingBox())!.width;
  await detailHandle.focus();
  await page.keyboard.press('ArrowLeft');
  await expect.poll(async () => (await page.locator('.inspector').boundingBox())!.width).toBe(detailWidth + 16);
  await page.getByRole('button', { name: /paper.md/ }).click();
  await expect(page.getByText('这是本次运行生成的研究报告。')).toBeVisible();
  const paper = page.locator('.file-item').filter({ has: page.getByRole('button', { name: /paper.md/ }) });
  await expect(paper.getByText('这是本次运行生成的研究报告。')).toBeVisible();
  await expect(paper.locator('table')).toHaveCount(1);
  await expect(paper.getByRole('checkbox')).toBeChecked();
  await expect(paper.locator('.katex')).toHaveCount(1);
  await expect(paper.locator('.mermaid-diagram > img')).toHaveCount(2, { timeout: 20000 });
  for (const diagram of await paper.locator('.mermaid-diagram > img').all()) {
    await expect.poll(() => diagram.evaluate(img => (img as HTMLImageElement).naturalWidth)).toBeGreaterThan(0);
  }
  await expect(paper.getByRole('alert')).toContainText('图表无法渲染');
  await paper.getByRole('button', { name: '放大图表' }).first().click();
  await expect(page.getByRole('dialog').getByRole('img', { name: 'Mermaid 图表', exact: true })).toBeVisible();
  await page.screenshot({ path: 'test-results/mermaid-expanded.png' });
  await page.keyboard.press('Escape');
  await expect(page.getByRole('button', { name: /evidence.md/ })).not.toBeVisible();
  await page.locator('.file-folder > summary').filter({ hasText: /^sources$/ }).click();
  expect(await page.locator('.file-folder > summary').first().evaluate(element =>
    getComputedStyle(element).listStyleType)).toBe('none');
  await expect(page.getByRole('button', { name: /notes.md/ })).toBeVisible();
  await page.locator('.file-folder > summary').filter({ hasText: /^raw$/ }).click();
  await page.getByRole('button', { name: /evidence.md/ }).click();
  await expect(page.getByText('这是嵌套目录中的证据。')).toBeVisible();
  await expect(paper.getByText('这是本次运行生成的研究报告。')).toBeVisible();
  await page.getByRole('button', { name: /evidence.md/ }).click();
  await expect(page.getByText('这是嵌套目录中的证据。')).not.toBeVisible();
  await page.locator('.file-folder > summary').filter({ hasText: /^sources$/ }).click();
  await page.screenshot({ path: 'test-results/files-desktop.png' });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole('button', { name: '运行工作流', exact: true })).toBeInViewport();
  await expect(page.locator('.execution-node').filter({ has: page.locator('strong', { hasText: /^publish$/ }) })).toBeInViewport();
  await page.screenshot({ path: 'test-results/run-mobile.png', fullPage: true });
  await page.locator('.file-open').scrollIntoViewIfNeeded();
  await page.screenshot({ path: 'test-results/files-mobile.png' });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  expect(errors).toEqual([]);
});
