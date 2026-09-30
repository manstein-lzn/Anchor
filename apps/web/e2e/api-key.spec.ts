import { test, expect } from '@playwright/test';

test('asks for the Anchor API key in the page and retries protected requests', async ({ page }) => {
  const authorized = (request: { headers(): Record<string, string> }) =>
    request.headers().authorization === 'Bearer test-anchor-api-key';
  await page.route('**/graphs', route => authorized(route.request())
    ? route.fulfill({ json: { graphs: [{ graph: 'test-graph', running: null }] } })
    : route.fulfill({ status: 401, json: { error: 'unauthorized' } }));
  await page.route('**/runs', route => authorized(route.request())
    ? route.fulfill({ json: { runs: [] } })
    : route.fulfill({ status: 401, json: { error: 'unauthorized' } }));
  await page.route('**/timeline*', route => authorized(route.request())
    ? route.fulfill({ json: { runs: [], scheduled: [], schedules: [] } })
    : route.fulfill({ status: 401, json: { error: 'unauthorized' } }));
  await page.route('**/graphs/test-graph', route => authorized(route.request())
    ? route.fulfill({ json: { definition: { entry: 'assistant', agents: { assistant: {} }, nodes: [], edges: [] } } })
    : route.fulfill({ status: 401, json: { error: 'unauthorized' } }));

  await page.goto('/');
  const dialog = page.getByRole('dialog', { name: '连接 Anchor 服务' });
  await expect(dialog).toBeVisible();
  await dialog.getByLabel('Anchor API key').fill('test-anchor-api-key');
  await dialog.getByRole('button', { name: '连接' }).click();
  await expect(dialog).not.toBeVisible();
  await expect(page.getByLabel('当前工作流')).toHaveValue('test-graph');
  await expect(page.getByText('后端请求失败', { exact: false })).toHaveCount(0);
});
