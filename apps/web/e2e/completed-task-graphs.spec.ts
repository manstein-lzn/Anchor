import { test, expect } from '@playwright/test';
import type { GraphSummary } from '../src/model';

for (const width of [1440, 390]) {
  test(`completed one-off Graphs leave the main list but retain searchable history at ${width}px`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 960 });
    const problems: string[] = [];
    page.on('pageerror', error => problems.push(error.message));
    const writes: string[] = [];
    const summaries: GraphSummary[] = [
      { graph: 'assistant', running: null },
      { graph: 'reminder', running: null, completed_task: true },
    ];
    const definition = { objective: '一次性提醒', entry: 'work', agents: {}, ops: { work: { run: 'true' } },
      nodes: [{ id: 'work', op: 'work' }], edges: [] };
    const run = { run: 'run-reminder', graph: 'reminder', objective: '一次性提醒', running: false,
      started: '2026-11-10T09:32:00Z', updated: '2026-11-10T09:32:05Z', executed: ['work'],
      projection: { phase: 'settled', outcome: 'done', reason: null, active: false,
        actions: { pause: false, stop: false, resume: false, abandon: false } } };
    await page.route('**/*', route => {
      const path = new URL(route.request().url()).pathname;
      if (route.request().method() !== 'GET') writes.push(path);
      if (path === '/graphs') return route.fulfill({ json: { graphs: summaries } });
      if (path === '/graphs/assistant' || path === '/graphs/reminder') return route.fulfill({ json: { definition, plugin_activation: { mode: 'new_run', graph_digest: 'fixture' } } });
      if (path === '/runs') return route.fulfill({ json: { runs: [run] } });
      if (path === '/plugins') return route.fulfill({ json: { plugins: [] } });
      if (path === '/timeline') return route.fulfill({ json: { runs: [run], schedules: [], scheduled: [] } });
      return route.continue();
    });
    await page.goto('/');
    const main = page.locator('[aria-label="主工作流列表"]');
    await expect(main.getByRole('button', { name: 'assistant', exact: true })).toBeVisible();
    await expect(main.getByRole('button', { name: 'reminder', exact: true })).toHaveCount(0);
    const toggle = page.getByRole('button', { name: /已完成的一次性任务/ });
    await expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await expect(page.getByLabel('当前工作流').locator('option')).toHaveCount(1);
    await page.getByLabel('搜索工作流').fill('reminder');
    await expect(toggle).toHaveAttribute('aria-expanded', 'true');
    await page.getByRole('button', { name: 'reminder', exact: true }).click();
    await expect(page.getByLabel('当前工作流')).toHaveValue('reminder');
    await expect(page.getByRole('button', { name: /运行历史/ })).toBeVisible();
    await page.getByRole('button', { name: /运行历史/ }).click();
    await expect(page.getByLabel('此图的运行历史')).toContainText('run-reminder');
    await page.keyboard.press('Escape');
    await page.getByLabel('搜索工作流').fill('');
    await expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await page.screenshot({ path: info.outputPath(`collected-tasks-${width}.png`), fullPage: true });
    await toggle.click();
    await page.screenshot({ path: info.outputPath(`expanded-tasks-${width}.png`), fullPage: true });
    summaries[1] = { graph: 'reminder', running: null };
    await expect(main.getByRole('button', { name: 'reminder', exact: true })).toBeVisible();
    await expect(toggle).toHaveCount(0);
    expect(await page.locator('.library').evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
    expect(writes).toEqual([]);
    expect(problems).toEqual([]);
  });
}
