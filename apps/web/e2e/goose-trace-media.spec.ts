import { expect, test } from '@playwright/test';

test.use({ launchOptions: { executablePath: process.env.ANCHOR_BROWSER_BINARY } });

const PNG = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADUlEQVQIHWP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC';
const block = (content: unknown) => ({ type: 'content', content });

test('projected native trace images, ordering and tool identities remain usable on desktop and mobile', async ({ page }) => {
  const errors: string[] = [], remote: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('request', request => { if (request.url().includes('evil.test')) remote.push(request.url()); });
  const graph = { objective: 'Native media UI projection fixture', entry: 'worker',
    agents: { worker: { model: 'fixture' } }, nodes: [{ id: 'worker', agent: 'worker' }], edges: [] };
  const run = { graph: 'media-trace', run: 'media-run', status: 'running', running: true,
    started: '2026-10-07T01:00:00Z', updated: '2026-10-07T01:00:00Z', executed: [], objective: graph.objective };
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [{ graph: run.graph, running: run.run }] } }));
  await page.route('**/graphs/*', route => route.fulfill({ json: { definition: graph } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [run] } }));
  await page.route('**/timeline*', route => route.fulfill({ json: { runs: [run], scheduled: [], schedules: [] } }));
  await page.route('**/runs/media-run', route => route.fulfill({ json: {
    graph: run.graph, run: run.run, nodes: ['worker'], active: true,
    state: { ...run, cursor: { node: 'worker', pass: 1, dir: '' }, passes: { worker: 1 }, nodes: {}, decided: {}, skipped: [], error: '' },
    traces: { '["worker",1]': [
      { role: 'assistant', text: 'Native thought, not user-facing reply.', thinking: true },
      { role: 'assistant', text: '', commands: ['anchor_run {"command":["cat","evidence.txt"]}'], tool_call_id: 'read', status: 'completed' },
      { role: 'assistant', text: '', commands: ['fixture_media {}'], tool_call_id: 'media', status: 'completed' },
      { role: 'tool', text: 'before native image\nafter native image', tool_call_id: 'media', status: 'completed', contents: [
        block({ type: 'text', text: 'before native image' }), block({ type: 'image', mimeType: 'image/png', data: PNG }),
        block({ type: 'text', text: 'after native image' }),
        block({ type: 'image', mimeType: 'image/svg+xml', data: btoa('<svg><image href="https://evil.test/a"/></svg>') }),
      ] },
      { role: 'tool', text: 'actual read result', tool_call_id: 'read', status: 'completed' },
    ] },
  } }));
  await page.goto('/');
  await page.getByRole('button', { name: '\u67e5\u770b\u5f53\u524d\u8fd0\u884c', exact: true }).click();
  await page.locator('.execution-node').filter({ has: page.locator('strong', { hasText: /^worker$/ }) }).click();
  const media = page.locator('.inspector details.call').filter({ has: page.locator('summary', { hasText: 'fixture_media' }) });
  await media.locator('summary').click();
  await expect(media).toContainText('before native image');
  await expect(media).not.toContainText('actual read result');
  const image = media.locator('.trace-picture > img');
  await expect(image).toBeVisible();
  await expect.poll(async () => image.evaluate((element: HTMLImageElement) => element.complete && element.naturalWidth > 0)).toBe(true);
  const pixels = await image.evaluate((element: HTMLImageElement) => {
    const canvas = document.createElement('canvas'); canvas.width = 1; canvas.height = 1;
    const context = canvas.getContext('2d')!; context.drawImage(element, 0, 0, 1, 1);
    return [...context.getImageData(0, 0, 1, 1).data];
  });
  expect(pixels.some(value => value > 0)).toBe(true);
  expect(await media.locator('.trace-media').evaluate(element => [...element.children].map(child => child.tagName))).toEqual(['PRE', 'FIGURE', 'PRE', 'P']);
  await expect(media.locator('.trace-media-unavailable')).toHaveCount(1);
  await expect(page.locator('.inspector .said')).toHaveCount(0);
  await expect(page.locator('.inspector .note')).toContainText('\u601d\u8003');
  await image.scrollIntoViewIfNeeded();
  await page.screenshot({ path: test.info().outputPath('media-desktop.png'), fullPage: true });
  await media.getByRole('button', { name: '\u67e5\u770b\u539f\u56fe' }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await expect(page.getByRole('dialog').locator('img')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 844 });
  await image.scrollIntoViewIfNeeded();
  await expect(image).toBeVisible();
  const overflow = await page.locator('.inspector').evaluate(element => element.scrollWidth - element.clientWidth);
  expect(overflow).toBeLessThanOrEqual(1);
  await page.screenshot({ path: test.info().outputPath('media-mobile.png'), fullPage: true });
  expect(remote).toEqual([]); expect(errors).toEqual([]);
  console.log(JSON.stringify({ scope: 'UI projection fixture, not real Goose/backend', real_model_calls: 0,
    viewports: ['1440x1000', '390x844'], image_pixels: pixels, external_image_requests: remote.length }));
});
