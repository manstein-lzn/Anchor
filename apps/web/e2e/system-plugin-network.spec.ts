import { expect, test, type Page } from '@playwright/test';
import type { OurGraph, Plugin } from '../src/model';
import { blockUnmockedApi } from './fixtures/api-fallback';

const network: Plugin = { id: 'network', name: '联网', description: '控制任务工具的网络访问。', available: true,
  kind: 'system', canUninstall: false, canReplace: false, updateSource: 'anchor-release' };
const extension: Plugin = { id: 'helper', name: '辅助能力', description: '可独立管理的扩展。', available: true,
  kind: 'extension', canUninstall: true, canReplace: true, updateSource: 'extension' };

async function fixture(page: Page) {
  await blockUnmockedApi(page);
  await page.addInitScript(() => localStorage.setItem('anchor:view', 'graph'));
  let definition: OurGraph = { entry: 'worker', agents: { worker: { model: 'models.default.flash', instructions: '整理结果' } },
    ops: { build: { run: 'true' } }, nodes: [
      { id: 'worker', agent: 'worker', plugins: [{ id: 'network', required: false, default: true, network: 'enabled' }] },
      { id: 'build', op: 'build' },
    ], edges: [{ from: 'worker', to: 'build' }] };
  const writes: OurGraph[] = [];
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [{ graph: 'policy', running: null }] } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [] } }));
  await page.route('**/graphs/policy', route => {
    if (route.request().method() === 'PUT') {
      definition = route.request().postDataJSON().definition;
      writes.push(definition);
    }
    return route.fulfill({ json: { definition } });
  });
  await page.route('**/plugins', route => route.fulfill({ json: { plugins: [network, extension] } }));
  await page.route('**/plugins/preflights', route => route.fulfill({ json: { reviews: [] } }));
  await page.route(/^https?:\/\/[^/]+\/plugins\/(?:network|helper)$/, route => route.fulfill({
    json: route.request().url().endsWith('/network') ? network : extension,
  }));
  await page.route('**/plugins/*/configuration', route => route.fulfill({ json: {
    plugin: new URL(route.request().url()).pathname.split('/')[2], authorization_digest: 'fixture', revision: 1, fields: [], targets: [],
  } }));
  await page.route('**/plugins/*/credentials', route => route.fulfill({ json: { targets: [] } }));
  await page.goto('/');
  await page.locator('[data-id="worker"]').click();
  return writes;
}

test('node-local networking retains explicit denial and command nodes expose no ordinary plugin mounts', async ({ page }) => {
  const writes = await fixture(page);
  const inspector = page.locator('.workspace:visible .inspector');
  const policy = inspector.getByRole('combobox', { name: '联网策略', exact: true });
  await expect(policy).toHaveValue('enabled');
  await expect(inspector.getByRole('switch', { name: /联网/ })).toHaveCount(0);
  await policy.selectOption('denied');
  await inspector.getByRole('switch', { name: '选择 辅助能力', exact: true }).click();
  expect(writes).toHaveLength(0);
  await page.getByRole('button', { name: '保存', exact: true }).click();
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0].nodes[0].plugins).toEqual([
    { id: 'network', required: false, default: true, network: 'denied' }, 'helper',
  ]);
  expect(writes[0].agents?.worker).not.toHaveProperty('network');
  await inspector.getByRole('button', { name: '编辑它的角色', exact: true }).click();
  await expect(inspector.getByRole('checkbox')).toHaveCount(0);
  await expect(policy).toHaveCount(0);

  await page.locator('[data-id="build"]').click();
  await expect(inspector.locator('.plugin-card')).toHaveCount(1);
  await expect(inspector.locator('.plugin-card')).toContainText('联网');
  await expect(inspector.getByRole('switch')).toHaveCount(0);
  await expect(policy).toHaveValue('auto');
  await policy.selectOption('enabled');
  await page.getByRole('button', { name: '保存', exact: true }).click();
  await expect.poll(() => writes.length).toBe(2);
  expect(writes[1].nodes[1].plugins).toEqual([{ id: 'network', network: 'enabled' }]);
  expect(writes[1].ops?.build).toEqual({ run: 'true' });
  for (const width of [390, 768, 1440]) {
    await page.setViewportSize({ width, height: 1000 });
    await expect(policy).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  }
});

test('system details show Anchor upgrades while only backend-authorized extension actions appear', async ({ page }) => {
  await fixture(page);
  await page.getByRole('button', { name: '插件库', exact: true }).click();
  await page.getByRole('button', { name: '查看 联网 的详情', exact: true }).click();
  const system = page.getByRole('dialog', { name: '联网 · 插件详情', exact: true });
  await expect(system).toContainText('系统插件');
  await expect(system).toContainText('随 Anchor 升级更新');
  await expect(system.getByRole('button', { name: '更新插件', exact: true })).toHaveCount(0);
  await expect(system.getByRole('button', { name: '查看卸载影响', exact: true })).toHaveCount(0);
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: '查看 辅助能力 的详情', exact: true }).click();
  const details = page.getByRole('dialog', { name: '辅助能力 · 插件详情', exact: true });
  await expect(details.getByRole('button', { name: '更新插件', exact: true })).toBeVisible();
  await expect(details.getByRole('button', { name: '查看卸载影响', exact: true })).toBeVisible();
});
