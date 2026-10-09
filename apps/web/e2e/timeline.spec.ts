import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

test('timeline fills the workspace and renders runs and plans on the same time scale', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-09-28T12:00:00'));
  const graph = JSON.parse(readFileSync('../../examples/graphs/academic-gated.json', 'utf8'));
  const run = { run: 'short-run', graph: 'research-review', status: 'finished', running: false,
    started: '2026-09-28T09:00:00', updated: '2026-09-28T09:02:00', executed: [], objective: '整理本周研究进展' };
  const runs = [run, { ...run, run: 'active-run', graph: 'academic-survey', status: 'running', running: true,
    started: '2026-09-28T10:00:00' }, { ...run, run: 'overlap-run', graph: 'weekly-report',
    started: '2026-09-28T11:00:00', updated: '2026-09-28T11:30:00' },
    { ...run, run: 'next-short-run', started: '2026-09-28T09:10:00', updated: '2026-09-28T09:12:00' }];
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [
    { graph: 'academic-survey', running: 'active-run' }, { graph: 'research-review', running: null },
  ] } }));
  await page.route('**/graphs/*', route => route.fulfill({ json: { definition: graph } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs } }));
  // Keep polling isolated from any authenticated development backend.
  await page.route('**/runs/*', route => route.fulfill({ json: { state: { ...run, input: {} } } }));
  await page.route('**/runs/short-run', route => route.fulfill({ json: { state: { ...run, input: {} } } }));
  await page.route('**/timeline*', route => route.fulfill({ json: {
    runs, schedules: [{ id: 'evening', graph: 'research-review', enabled: true,
      rule: { type: 'daily', time: '18:00' }, next_at: '2026-09-28T18:00:00' }],
    scheduled: [{ schedule: 'evening', graph: 'research-review', scheduled_at: '2026-09-28T18:00:00', status: 'planned' },
      { schedule: 'morning', graph: 'research-review', scheduled_at: '2026-09-28T07:00:00', status: 'missed_busy' }],
  } }));
  await page.goto('/');
  await page.getByRole('button', { name: '运行看板', exact: true }).click();
  const surface = page.getByLabel('可滚动的每日时间线');
  const plan = page.locator('.timeline-plan.planned');
  await expect(page.getByRole('tooltip')).not.toBeVisible();
  await plan.locator('.timeline-point').hover();
  await expect(page.getByRole('tooltip')).toContainText('已计划');
  await expect(page.locator('.timeline-legend')).toHaveText('实际运行计划时点已错过');
  const plannedMark = await plan.locator('i').evaluate(el => ({ color: getComputedStyle(el).borderColor, fill: getComputedStyle(el).backgroundImage }));
  const missed = page.locator('.timeline-plan.missed_busy');
  const missedMark = await missed.locator('i').evaluate(el => ({ color: getComputedStyle(el).borderColor, fill: getComputedStyle(el).backgroundImage }));
  expect(plannedMark.color).toBe(missedMark.color);
  expect(plannedMark.fill).toBe('none');
  expect(missedMark.fill).toContain('linear-gradient');
  await missed.hover();
  await expect(page.getByRole('tooltip')).toContainText('忙碌错过');
  // Graphs use stable colors; details stay out of the way until hover/focus.
  const colors = await page.locator('.timeline-duration').evaluateAll(items => items.map(item => getComputedStyle(item).backgroundColor));
  expect(new Set(colors).size).toBeGreaterThan(1);
  expect(await page.locator('.timeline-day.today').getAttribute('style')).toContain('min-height: 126px');
  const active = page.locator('.timeline-run[aria-label^="academic-survey，"]');
  const overlap = page.locator('.timeline-run[aria-label^="weekly-report，"]');
  await active.hover();
  await expect(page.getByRole('tooltip')).toBeVisible();
  expect(await active.evaluate(element => getComputedStyle(element).zIndex)).toBe('5');
  await overlap.hover();
  await expect(page.getByRole('tooltip')).toBeVisible();
  expect(await overlap.evaluate(element => getComputedStyle(element).zIndex)).toBe('5');
  const positions = await plan.evaluate(entry => {
    const track = entry.parentElement!.getBoundingClientRect();
    const point = entry.querySelector('.timeline-point')!.getBoundingClientRect();
    const tick = document.querySelectorAll('.timeline-scale b')[3].getBoundingClientRect();
    return { point: (point.x + point.width / 2 - track.x) / track.width,
      tickDelta: Math.abs(point.x + point.width / 2 - tick.x - tick.width / 2) };
  });
  expect(positions.point).toBeCloseTo(.75, 2);
  expect(positions.tickDelta).toBeLessThan(1);
  const shortBar = page.locator('.timeline-run.finished').first().locator('.timeline-duration');
  const bar = (await shortBar.boundingBox())!;
  expect(bar.width).toBeGreaterThan(0);
  expect(bar.width).toBeLessThan(10);
  // Exercise real pointer coordinates: empty space around a mark must not act as a Run.
  for (const entry of await page.locator('.timeline-entry').all()) {
    const mark = (await entry.locator('i').boundingBox())!;
    const button = (await entry.boundingBox())!;
    expect(button).toEqual(mark);
    await entry.hover();
    await expect(page.getByRole('tooltip')).toBeVisible();
    expect(await entry.evaluate(el => getComputedStyle(el).backgroundColor)).toBe('rgba(0, 0, 0, 0)');
    for (const [x, y] of [[mark.x - 2, mark.y + mark.height / 2],
      [mark.x + mark.width + 2, mark.y + mark.height / 2],
      [mark.x + mark.width / 2, mark.y - 8], [mark.x + mark.width / 2, mark.y + mark.height + 8]]) {
      await page.mouse.move(0, 0);
      await page.mouse.move(x, y);
      await expect(page.getByRole('tooltip')).not.toBeVisible();
      await page.mouse.click(x, y);
      await expect(page.getByRole('dialog')).not.toBeVisible();
    }
  }
  for (const time of ['09:00', '09:10', '09:00']) {
    await page.getByRole('button', { name: `${run.graph}，已完成，${time}`, exact: true }).hover();
    await expect(page.getByRole('tooltip')).toContainText(time);
  }
  await page.screenshot({ path: 'test-results/timeline-desktop.png' });

  await plan.click();
  const preview = page.getByRole('dialog', { name: '计划详情' });
  await expect(preview).toContainText('每天 · 18:00');
  await expect(preview).toContainText('这是计划开始的时点');
  await page.screenshot({ path: 'test-results/timeline-plan-preview.png' });
  await page.keyboard.press('Escape');
  await page.getByLabel('筛选状态').selectOption('attention');
  await expect(page.locator('.timeline-entry')).toHaveCount(1);
  await page.locator('.timeline-entry').click();
  await expect(preview).toContainText('本次未执行，也不会补跑');
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: '清除筛选' }).click();
  await expect(page.locator('.timeline-entry')).toHaveCount(6);

  for (const [width, height] of [[1440, 1000], [768, 900], [390, 844], [320, 640]]) {
    await page.setViewportSize({ width, height });
    const board = (await page.locator('.timeline-board').boundingBox())!;
    const area = (await surface.boundingBox())!;
    const footer = (await page.locator('.timeline-footer').boundingBox())!;
    expect(board.width).toBe(width);
    expect(board.y + board.height).toBeLessThanOrEqual(height + 1);
    expect(area.height).toBeGreaterThanOrEqual(120);
    expect(footer.y + footer.height).toBeLessThanOrEqual(height + 1);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    for (const entry of await page.locator('.timeline-entry').all()) {
      expect(await entry.boundingBox()).toEqual(await entry.locator('i').boundingBox());
    }
    await expect(page.getByLabel('最大化详情面板')).not.toBeVisible();
    await surface.evaluate(el => { el.scrollLeft = el.scrollWidth; });
    await plan.hover();
    await expect(page.getByRole('tooltip')).toBeInViewport();
    if (width === 390) await page.screenshot({ path: 'test-results/timeline-mobile.png' });
  }
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole('button', { name: '管理计划' }).click();
  const schedules = page.getByRole('dialog', { name: '定时计划' });
  await expect(schedules.getByRole('button', { name: '保存计划' })).toBeInViewport();
  await page.screenshot({ path: 'test-results/timeline-schedules-mobile.png' });
});

test('timeline hints fit the viewport at both edges and dismiss without blocking runs', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-09-28T12:00:00'));
  const longName = 'edge-workflow-with-a-long-unbroken-name-'.repeat(5);
  const graph = JSON.parse(readFileSync('../../examples/graphs/academic-gated.json', 'utf8'));
  const runs = [
    { run: 'left', graph: longName, started: '2026-09-27T00:01:00', updated: '2026-09-27T00:15:00' },
    { run: 'right', graph: longName, started: '2026-09-27T23:45:00', updated: '2026-09-27T23:59:00' },
  ].map(run => ({ ...run, status: 'finished', running: false, executed: [] }));
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [{ graph: longName, running: null }] } }));
  await page.route('**/graphs/*', route => route.fulfill({ json: { definition: graph } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs } }));
  await page.route('**/runs/*', route => route.fulfill({ json: { state: { ...runs[0], input: {} } } }));
  await page.route('**/runs/right', route => route.fulfill({ json: { state: { ...runs[1], input: {} } } }));
  await page.route('**/timeline*', route => route.fulfill({ json: { runs, schedules: [], scheduled: [
    { schedule: 'late', graph: longName, scheduled_at: '2026-09-28T23:59:00', status: 'planned' },
  ] } }));
  await page.goto('/');
  await page.getByRole('button', { name: '运行看板', exact: true }).click();
  const surface = page.getByLabel('可滚动的每日时间线');
  const tooltip = page.getByRole('tooltip');
  const entries = page.locator('.timeline-entry');
  const assertFits = async () => {
    await expect(tooltip).toHaveCount(1);
    await expect(tooltip).toBeVisible();
    await expect(tooltip.locator('strong')).toHaveText(longName);
    const box = (await tooltip.boundingBox())!;
    const viewport = page.viewportSize()!;
    expect(box.x).toBeGreaterThanOrEqual(8);
    expect(box.y).toBeGreaterThanOrEqual(8);
    expect(box.x + box.width).toBeLessThanOrEqual(viewport.width - 8);
    expect(box.y + box.height).toBeLessThanOrEqual(viewport.height - 8);
    expect(await tooltip.evaluate(el => el.scrollWidth <= el.clientWidth && el.scrollHeight <= el.clientHeight)).toBe(true);
    // A fully visible box is not enough if its text is still truncated.
    expect(await tooltip.locator('strong').evaluate(el => {
      const range = document.createRange(); range.selectNodeContents(el);
      const text = range.getBoundingClientRect();
      const box = el.getBoundingClientRect();
      return text.left >= box.left && text.right <= box.right + 1 && text.bottom <= box.bottom + 1;
    })).toBe(true);
  };
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: 844 });
    for (const index of [0, 1, 2]) {
      await surface.evaluate((el, right) => { el.scrollLeft = right ? el.scrollWidth : 0; }, index > 0);
      const entry = entries.nth(index);
      await entry.locator('i').hover();
      await assertFits();
      await expect(entry).not.toHaveAttribute('title');
      await expect(entry).toHaveAttribute('aria-describedby', await tooltip.getAttribute('id') as string);
      // Moving onto the hint keeps long text readable.
      await tooltip.hover();
      await assertFits();
      if (index === 1) await page.screenshot({ path: `test-results/timeline-edge-${width}.png` });
      await page.keyboard.press('Escape');
      await expect(tooltip).not.toBeVisible();
      await entry.focus();
      await assertFits();
      await page.keyboard.press('Escape');
      await expect(tooltip).not.toBeVisible();
    }
    await entries.nth(1).hover();
    await assertFits();
    await surface.evaluate(el => { el.scrollTop += 20; });
    await expect(tooltip).not.toBeVisible();
    await page.mouse.move(0, 0);
    await entries.nth(1).hover();
    await assertFits();
    await page.setViewportSize({ width: width - 1, height: 844 });
    await expect(tooltip).not.toBeVisible();
  }
  await entries.nth(1).click();
  await expect(tooltip).not.toBeVisible();
  await expect(page.getByRole('dialog', { name: '运行详情' })).toContainText('Run · right');
});

test('a resident Run draws only the windows it executed, not its idle lifetime', async ({ page }) => {
  await page.clock.setFixedTime(new Date('2026-09-30T12:00:00'));
  const graph = JSON.parse(readFileSync('../../examples/graphs/academic-gated.json', 'utf8'));
  const resident = {
    run: 'assistant-1', graph: 'wecom-persistent-assistant', status: 'running', running: true,
    started: '2026-09-28T08:00:00', updated: '2026-09-30T11:30:00', executed: ['wait_input', 'assistant'],
    objective: '常驻助手', trigger: { source: 'channel' },
    activity: [
      { start: '2026-09-28T09:00:00', end: '2026-09-28T09:20:00' },
      { start: '2026-09-30T10:00:00', end: '2026-09-30T10:10:00' },
      { start: '2026-09-30T11:30:00', end: '2026-09-30T11:30:00', running: true },
    ],
  };
  // An instance admitted but not yet given a Turn: it must be marked where it started, never
  // stretched across the day it was waiting in.
  const idle = { ...resident, run: 'assistant-idle', started: '2026-09-30T08:00:00', updated: '2026-09-30T08:00:00', activity: [] };
  const runs = [resident, idle];
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [{ graph: resident.graph, running: resident.run }] } }));
  await page.route('**/graphs/*', route => route.fulfill({ json: { definition: graph } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs } }));
  await page.route('**/runs/*', route => route.fulfill({ json: { state: { ...resident, input: {} } } }));
  await page.route('**/timeline*', route => route.fulfill({ json: { runs, schedules: [], scheduled: [] } }));
  await page.goto('/');
  await page.getByRole('button', { name: '运行看板', exact: true }).click();
  await expect(page.locator('.timeline-run')).toHaveCount(4);
  const today = page.locator('.timeline-day.today');
  await expect(today.locator('.timeline-run')).toHaveCount(3);
  const placed = await today.locator('.timeline-run').evaluateAll(entries => entries.map(entry => {
    const track = entry.parentElement!.getBoundingClientRect();
    const bar = entry.querySelector('i')!.getBoundingClientRect();
    return { left: (bar.x - track.x) / track.width, right: (bar.x + bar.width - track.x) / track.width };
  }));
  expect(placed[0].left).toBeCloseTo(8 / 24, 2);
  expect(placed[1].left).toBeCloseTo(10 / 24, 2);
  expect(placed[1].right).toBeCloseTo((10 * 60 + 10) / (24 * 60), 2);
  expect(placed[2].left).toBeCloseTo(11.5 / 24, 2);
  expect(placed[2].right).toBeCloseTo(12 / 24, 2);
  // The idle instance stays a marker at its start instead of covering the day.
  const idleBar = (await today.locator('.timeline-run[data-run-id="assistant-idle"] i').boundingBox())!;
  expect(idleBar.width).toBeLessThan(12);
  // The hours between the turns stay empty: no bar reaches back into idle waiting.
  expect(placed[1].left).toBeGreaterThan(0.4);
  expect(placed[2].left - placed[1].right).toBeGreaterThan(0.04);
  await page.screenshot({ path: 'test-results/timeline-resident-windows.png' });
  await today.locator('.timeline-run[data-run-id="assistant-idle"]').click();
  await expect(page.getByRole('dialog', { name: '运行详情' })).toContainText('等待下一轮输入');
  await page.keyboard.press('Escape');
  await today.locator('.timeline-run[data-run-id="assistant-1"]').first().click();
  const preview = page.getByRole('dialog', { name: '运行详情' });
  await expect(preview).toContainText('活动时段');
  await expect(preview).toContainText('3 段');
  await expect(preview).toContainText('等待期间不算执行');
});
