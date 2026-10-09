import { test, expect } from '@playwright/test';
import type { GraphSummary } from '../src/model';

test('resident listener uses thin dashes and actual work uses thick solid outlines', async ({ page }) => {
  const graph = { objective: '事件触发助手', entry: 'assistant',
    agents: { assistant: { model: 'fixture' } },
    nodes: [{ id: 'assistant', agent: 'assistant' }], edges: [] };
  const summary: GraphSummary = { graph: 'wecom-assistant', running: null, active_runs: [], active_nodes: [],
    listener: { platform: 'wecom', status: 'authenticated' } };
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [summary] } }));
  await page.route('**/graphs/*', route => route.fulfill({ json: { definition: graph } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [] } }));
  await page.route('**/timeline*', route => route.fulfill({ json: { runs: [], scheduled: [], schedules: [] } }));
  await page.goto('/');
  const node = page.locator('.workflow-node');
  const item = page.locator('.library-item');
  await expect(node).toContainText('常驻监听');
  await expect(node).toHaveCSS('border-top-style', 'dashed');
  await expect(node).toHaveCSS('border-top-width', '1px');
  await expect(item).toHaveCSS('border-top-style', 'dashed');
  await node.click();
  await expect(node).toHaveCSS('border-top-style', 'dashed');

  summary.running = 'run-active';
  summary.active_runs = ['run-active'];
  summary.active_nodes = ['assistant'];
  await expect(node).toContainText('执行中');
  await expect(node).toHaveCSS('border-top-style', 'solid');
  await expect(node).toHaveCSS('border-top-width', '3px');
  await expect(item).toHaveCSS('border-top-width', '3px');

  summary.running = null;
  summary.active_runs = [];
  summary.active_nodes = [];
  await expect(node).toContainText('常驻监听');
  await expect(node).toHaveCSS('border-top-width', '1px');
  await expect(node).toHaveCSS('border-top-style', 'dashed');
  summary.listener!.status = 'reconnecting';
  await expect(node).toContainText('正在重连');
  await expect(node).toHaveCSS('border-top-style', 'solid');
  await expect(node).toHaveCSS('border-top-width', '1px');
  await expect(item).toContainText('正在重连');
  await page.route('**/graphs', route => route.abort());
  await expect(node).toContainText('监听不可用');
  await expect(node).toHaveCSS('border-top-style', 'solid');
});
