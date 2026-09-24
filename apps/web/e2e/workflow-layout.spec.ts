import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';

async function geometry(page: Page) {
  await expect(page.locator('.workflow-canvas')).toHaveAttribute('aria-busy', 'false');
  await expect(page.locator('.react-flow__edge-path')).toHaveCount(12);
  return page.evaluate(() => ({
    nodes: Array.from(document.querySelectorAll<HTMLElement>('.react-flow__node')).map(n => ({
      id: n.dataset.id, transform: n.style.transform, width: n.style.width, height: n.style.height,
    })),
    paths: Array.from(document.querySelectorAll('.react-flow__edge-path')).map(e => e.getAttribute('d')),
    ports: Array.from(document.querySelectorAll<HTMLElement>('.route-port')).map(p => [p.dataset.handleid, p.style.left, p.style.top]),
  }));
}

test('one layout survives view switches, polling, dragging, arranging, undo and reload', async ({ page }) => {
  let graph = JSON.parse(readFileSync('../../examples/graphs/deep-academic-research.json', 'utf8'));
  const errors: string[] = [];
  page.on('pageerror', e => errors.push(e.message));
  const run = { run: 'research-run', graph: 'research', status: 'running', running: true,
    started: '2026-09-24T08:00:00Z', updated: '2026-09-24T08:00:00Z', objective: '研究',
    executed: ['frame', 'investigate', 'challenge', 'feedback', 'frame', 'investigate'] };
  await page.route('**/graphs', r => r.fulfill({ json: { graphs: [{ graph: 'research', running: run.run }] } }));
  await page.route('**/graphs/research', async r => {
    if (r.request().method() === 'PUT') graph = r.request().postDataJSON().definition;
    await r.fulfill({ json: { definition: graph } });
  });
  let polls = 0;
  await page.route('**/runs', r => { polls++; return r.fulfill({ json: { runs: [run] } }); });
  await page.route('**/runs/research-run', r => r.fulfill({ json: {
    graph: 'research', run: run.run, nodes: graph.nodes.map((n: { id: string }) => n.id), traces: {},
    state: { ...run, cursor: { node: 'investigate', pass: 2, dir: '' }, passes: { frame: 2, investigate: 2 },
      decided: { 'feedback|frame': [false, 2] }, nodes: {}, skipped: [], error: '' },
  } }));
  await page.goto('/');
  await expect(page.locator('.workflow-node')).toHaveCount(8);
  const initial = await geometry(page);
  await expect(page.locator('.workflow-edge-label')).toHaveCount(7);
  await page.screenshot({ path: 'test-results/layout-editor.png' });
  await page.getByRole('button', { name: '运行记录', exact: true }).click();
  await expect(page.getByTestId('execution-canvas')).toBeVisible();
  expect(await geometry(page)).toEqual(initial);
  const feedback = page.getByTestId('edge-e3-feedback-frame').locator('.react-flow__edge-path');
  await expect(feedback).toHaveCSS('stroke-dasharray', 'none');
  await expect(feedback).toHaveCSS('stroke-width', '3.5px');
  await expect(page.getByTestId('edge-e4-feedback-investigate').locator('.react-flow__edge-path')).not.toHaveCSS('stroke-dasharray', 'none');
  await page.screenshot({ path: 'test-results/layout-run.png' });
  await page.locator('.react-flow__controls-zoomin').click();
  await page.waitForTimeout(250);
  const viewport = await page.locator('.react-flow__viewport').getAttribute('style');
  const before = polls;
  await expect.poll(() => polls).toBeGreaterThan(before);
  expect(await geometry(page)).toEqual(initial);
  expect(await page.locator('.react-flow__viewport').getAttribute('style')).toBe(viewport);
  await page.getByRole('button', { name: '图编排', exact: true }).click();
  const node = page.locator('.react-flow__node[data-id="investigate"]');
  const box = (await node.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2 + 100, { steps: 8 });
  await page.mouse.up();
  await expect(page.getByText('● 未保存')).toBeVisible();
  const dragged = await geometry(page);
  expect(dragged.paths).not.toEqual(initial.paths);
  expect(dragged.nodes.find(n => n.id === 'investigate')).not.toEqual(initial.nodes.find(n => n.id === 'investigate'));
  await page.getByRole('button', { name: '运行记录', exact: true }).click();
  expect(await geometry(page)).toEqual(dragged);
  await page.getByRole('button', { name: '图编排', exact: true }).click();
  await page.getByRole('button', { name: '自动整理', exact: true }).click();
  await expect.poll(() => geometry(page)).toEqual(initial);
  await page.getByRole('button', { name: '撤销', exact: true }).click();
  await expect.poll(() => geometry(page)).toEqual(dragged);
  await page.getByRole('button', { name: '保存', exact: true }).click();
  await expect(page.getByText('所有更改已保存')).toBeVisible();
  await page.reload();
  expect(await geometry(page)).toEqual(dragged);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByTestId('graph-canvas')).toBeInViewport();
  await expect.poll(async () => page.locator('.react-flow__edge-path').evaluateAll(paths => {
    const area = document.querySelector('.workflow-canvas')!.getBoundingClientRect();
    return paths.every(p => { const b = p.getBoundingClientRect();
      return b.left >= area.left && b.right <= area.right && b.top >= area.top && b.bottom <= area.bottom;
    });
  })).toBe(true);
  expect(errors).toEqual([]);
});
