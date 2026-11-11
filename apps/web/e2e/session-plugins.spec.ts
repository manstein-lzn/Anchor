import { expect, test, type Page } from '@playwright/test';
import type { SessionPlugin } from '../src/SessionPlugins';

const initialPlugins: SessionPlugin[] = [
  { id: 'academic-research', name: 'Academic Research',
    description: '检索学术论文、阅读全文，并整理可追溯的研究证据。',
    skills: ['literature-review', 'paper-reading', 'evidence-synthesis'],
    mcpServerNames: ['academic-research'], on: true, available: true },
  { id: 'docmost', name: 'Docmost', description: '读取和整理团队知识库，将研究结果写入文档。',
    skills: ['docmost'], mcpServerNames: ['docmost'], on: false, available: true },
  { id: 'wecom', name: '企业微信', description: '通过企业微信接收消息、发送回复与文件。',
    skills: ['wecom'], mcpServerNames: ['wecom'], on: false, available: true },
];
const unexpectedRequests = new WeakMap<Page, string[]>();

test.beforeEach(async ({ page }) => {
  const unexpected: string[] = [];
  unexpectedRequests.set(page, unexpected);
  await page.route(/^https?:\/\/[^/]+\/(?:graph-relations|channel-sessions|channels|settings|sessions|plugins|graphs|runs|timeline|schedules|trigger)(?:[/?]|$)/, route => {
    const request = route.request();
    const description = `${request.method()} ${new URL(request.url()).pathname}`;
    unexpected.push(description);
    return route.fulfill({ status: 500, json: { error: `Unexpected fixture request: ${description}` } });
  });
});

test.afterEach(async ({ page }) => {
  expect(unexpectedRequests.get(page) ?? [], 'Every management API request must be mocked by this fixture').toEqual([]);
});

async function openPilot(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem('anchor:view', 'pilot');
    localStorage.setItem('pilot:selected', 'design-session');
  });
  const session = { id: 'design-session', title: '调研多 Agent 协作中的上下文管理', status: 'active',
    updated_at: '2026-10-11T10:00:00Z', approval: null };
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [] } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [] } }));
  await page.route('**/timeline*', route => route.fulfill({ json: { runs: [], scheduled: [], schedules: [] } }));
  await page.route('**/sessions', route => route.fulfill({ json: { sessions: [session] } }));
  await page.route('**/sessions/*/messages', route => route.fulfill({ json: { messages: [
    { role: 'user', text: '帮我调研多 Agent 协作中的上下文管理，重点看信息如何在不同角色之间交接。' },
    { role: 'assistant', text: '可以。我们可以从三个角度展开：\n\n- **共享与隔离**：哪些信息需要共享，哪些只保留在当前角色的上下文里。\n- **交接方式**：直接传递历史，还是用结构化产物交接。\n- **证据质量**：如何检查摘要遗漏和信息失真。\n\n你可以在插件面板中挂载学术检索能力，再告诉我希望覆盖的时间范围。' },
  ] } }));
  await page.route('**/sessions/*/turns', route => route.fulfill({ json: { turns: [] } }));
  await page.route('**/sessions/*/credential-requests', route => route.fulfill({ json: { requests: [] } }));
  await page.route(/^https?:\/\/[^/]+\/plugins\/[^/?]+$/, route => {
    const id = decodeURIComponent(new URL(route.request().url()).pathname.split('/').at(-1)!);
    const plugin = initialPlugins.find(item => item.id === id);
    return route.fulfill({ json: { ...plugin, instructions: '插件的完整使用说明。',
      mcpServers: Object.fromEntries((plugin?.mcpServerNames ?? []).map(name => [name, { transport: 'stdio' }])) } });
  });
  await page.route('**/plugins/*/configuration', route => route.fulfill({ json: {
    plugin: decodeURIComponent(new URL(route.request().url()).pathname.split('/')[2]),
    authorization_digest: 'fixture-authorization', revision: 1, fields: [], targets: [],
  } }));
  await page.route('**/plugins/*/credentials', route => route.fulfill({ json: {
    plugin: decodeURIComponent(new URL(route.request().url()).pathname.split('/')[2]), targets: [],
  } }));
  await page.goto('/');
  await expect(page.getByLabel('发送给 Anchor Pilot')).toBeEnabled();
}

test('one network policy controls dependency activation and global denial for the whole Plugin list', async ({ page }) => {
  let mode: 'auto' | 'enabled' | 'denied' = 'auto';
  let research = false;
  const writes: string[] = [];
  const direct = () => mode === 'enabled';
  const permission = () => mode !== 'denied';
  const rows = (): SessionPlugin[] => [
    { id: 'academic-research', name: '学术调研', on: research, explicitOn: research,
      requiresNetwork: true, available: permission() || !research, networkAllowed: permission() && (direct() || research),
      error: permission() || !research ? null : '该插件需要联网：当前任务已禁止联网' },
    { id: 'network', name: '联网', description: '启用任务工具的网络访问。', networkProvider: true,
      on: permission() && (direct() || research), explicitOn: direct(), enabledBy: research ? ['academic-research'] : [],
      networkMode: mode, networkAllowed: permission() && (direct() || research), available: permission(),
      error: permission() ? null : '当前任务已禁止联网' },
  ];
  await page.route('**/sessions/*/plugins', route => route.fulfill({ json: { plugins: rows() } }));
  await page.route('**/sessions/*/plugins/*', route => {
    const id = new URL(route.request().url()).pathname.split('/').at(-1)!;
    writes.push(id);
    research = route.request().postDataJSON().on;
    return route.fulfill({ json: { plugin: rows().find(row => row.id === id), plugins: rows() } });
  });
  await page.route('**/sessions/*/network', route => {
    mode = route.request().postDataJSON().mode;
    return route.fulfill({ json: { plugins: rows() } });
  });
  await openPilot(page);
  await page.getByRole('button', { name: '展开插件面板', exact: true }).click();
  const rail = page.getByRole('complementary', { name: '会话插件挂载' });
  await rail.getByRole('switch', { name: '挂载 学术调研', exact: true }).click();
  const policy = rail.getByRole('combobox', { name: '联网策略', exact: true });
  await expect(rail.locator('.plugin-network-policy')).toContainText('由学术调研依赖启用');
  await expect(policy).toHaveValue('auto');
  await expect(rail.getByRole('switch', { name: /联网/ })).toHaveCount(0);
  await policy.selectOption('enabled');
  await expect(rail.locator('.plugin-network-policy')).toContainText('主动启用联网');
  await policy.selectOption('auto');
  await expect(rail.locator('.plugin-network-policy')).toContainText('由学术调研依赖启用');
  await policy.selectOption('denied');
  await expect(rail.getByText('不可用：该插件需要联网：当前任务已禁止联网')).toBeVisible();
  await expect(rail.locator('.plugin-network-policy')).toContainText('插件依赖不能重新启用');
  await policy.selectOption('auto');
  await rail.getByRole('switch', { name: '取消挂载 学术调研', exact: true }).click();
  await expect(rail.locator('.plugin-network-policy')).toContainText('当前网络访问未启用');
  expect(writes).toEqual(['academic-research', 'academic-research']);
  await page.screenshot({ path: test.info().outputPath('network-plugin.png') });
});

test('the capability rail leaves room to read, searches resources and syncs new installations independently', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  let installed = structuredClone(initialPlugins);
  await page.route('**/sessions/*/plugins', route => route.fulfill({ json: { plugins: installed } }));
  await page.route('**/sessions/*/plugins/*', route => {
    const id = decodeURIComponent(new URL(route.request().url()).pathname.split('/').at(-1)!);
    installed = installed.map(plugin => plugin.id === id ? { ...plugin, on: route.request().postDataJSON().on } : plugin);
    return route.fulfill({ json: { plugin: installed.find(plugin => plugin.id === id) } });
  });
  await openPilot(page);
  await page.getByRole('button', { name: '展开插件面板', exact: true }).click();
  const rail = page.getByRole('complementary', { name: '会话插件挂载' });
  await expect(rail.locator('.plugin-card')).toHaveCount(3);
  await expect(rail.locator('.pilot-plugins-intro')).toHaveCount(0);
  await expect(rail.locator('.pilot-plugins-summary')).toContainText('1 已挂载');
  const chatBox = await page.getByLabel('Pilot 对话', { exact: true }).boundingBox();
  const railBox = await rail.boundingBox();
  const messagesBox = await page.getByRole('log', { name: '对话消息' }).boundingBox();
  expect(chatBox && railBox && chatBox.x + chatBox.width <= railBox.x).toBeTruthy();
  expect(messagesBox!.height).toBeGreaterThan(page.viewportSize()!.height * .55);
  await page.screenshot({ path: test.info().outputPath('plugins-desktop.png') });

  await rail.getByRole('button', { name: '查看 Academic Research 的详情', exact: true }).click();
  const details = page.getByRole('dialog', { name: 'Academic Research · 插件详情', exact: true });
  await expect(details).toContainText('evidence-synthesis');
  await details.getByRole('button', { name: '关闭', exact: true }).click();
  const filters = rail.getByRole('group', { name: '插件筛选' });
  await filters.getByRole('button', { name: /^未挂载/ }).click();
  await expect(rail.locator('.plugin-card')).toHaveCount(2);
  await filters.getByRole('button', { name: /^全部/ }).click();
  await rail.getByLabel('搜索插件').fill('evidence-synthesis');
  await expect(rail.locator('.plugin-card')).toHaveCount(1);
  await expect(rail.locator('.plugin-card')).toContainText('Academic Research');
  await rail.getByLabel('搜索插件').fill('no-such-plugin');
  await expect(rail.getByText('没有匹配的插件')).toBeVisible();
  await rail.getByRole('button', { name: '查看全部插件' }).click();
  await rail.getByRole('switch', { name: '挂载 Docmost', exact: true }).click();
  await expect(rail.getByRole('switch', { name: '取消挂载 Docmost', exact: true })).toHaveAttribute('aria-checked', 'true');
  await expect(rail.getByRole('status')).toContainText('已保存，下次发消息时使用新设置');
  await expect(rail.locator('.pilot-plugins-summary')).toContainText('2 已挂载');

  // A mid-session installation needs no reload. A long list scrolls separately from the transcript
  // and must not move the composer out of the viewport.
  installed.push(...Array.from({ length: 18 }, (_, index) => ({ id: `fixture-${index}`, name: `测试能力 ${index}`,
    description: '用于检查插件列表独立滚动的浏览器夹具。', on: false, available: true })));
  await expect(rail.locator('.plugin-card')).toHaveCount(21, { timeout: 7000 });
  await rail.locator('.plugin-card').last().scrollIntoViewIfNeeded();
  await expect(page.getByLabel('发送给 Anchor Pilot')).toBeInViewport();
  await expect(page.getByRole('log', { name: '对话消息' })).toBeInViewport();
  const scrollBox = await rail.locator('.pilot-plugins-body').evaluate(element => ({ height: element.clientHeight, scroll: element.scrollHeight }));
  expect(scrollBox.scroll).toBeGreaterThan(scrollBox.height);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy();
  await rail.getByRole('button', { name: '收起插件面板' }).click();
  const expandedChat = await page.getByLabel('Pilot 对话', { exact: true }).boundingBox();
  expect(expandedChat!.width).toBeGreaterThan(chatBox!.width);
  await rail.getByRole('button', { name: '展开插件面板' }).click();
  await expect(rail.locator('.plugin-card')).toHaveCount(21);
  expect(errors).toEqual([]);
});

test('mobile keeps the conversation visible and provides a keyboard accessible plugin drawer with honest errors', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const installed = initialPlugins.map(plugin => ({ ...plugin, required: plugin.id === 'academic-research' }));
  await page.route('**/sessions/*/plugins', route => route.fulfill({ json: { plugins: installed } }));
  await page.route('**/sessions/*/plugins/*', route => route.fulfill({ status: 409, json: { error: '插件依赖尚未准备完成，请稍后重试。' } }));
  await openPilot(page);
  const input = page.getByLabel('发送给 Anchor Pilot');
  await input.fill('请重点关注近两年的论文');
  await expect(input).toBeInViewport();
  await expect(page.getByRole('log', { name: '对话消息' })).toBeVisible();
  const trigger = page.getByRole('button', { name: '展开插件面板' });
  await expect(trigger).toHaveAttribute('aria-expanded', 'false');
  await page.screenshot({ path: test.info().outputPath('plugins-mobile-chat.png') });
  await trigger.click();
  const drawer = page.getByRole('dialog', { name: '会话插件' });
  await expect(drawer).toBeVisible();
  await expect(drawer.getByRole('switch', { name: '取消挂载 Academic Research' })).toBeDisabled();
  await expect(drawer.getByText('必需 · 工作流需要此插件，保持挂载。')).toBeVisible();
  await drawer.getByRole('switch', { name: '挂载 Docmost', exact: true }).click();
  await expect(drawer.getByRole('alert')).toHaveText('插件依赖尚未准备完成，请稍后重试。');
  await expect(drawer.getByRole('switch', { name: '挂载 Docmost', exact: true })).toHaveAttribute('aria-checked', 'false');
  await page.screenshot({ path: test.info().outputPath('plugins-mobile-drawer.png') });
  const drawerBox = await drawer.boundingBox();
  expect(drawerBox!.x).toBeGreaterThanOrEqual(0);
  expect(drawerBox!.x + drawerBox!.width).toBeLessThanOrEqual(390);
  // The native modal isolates background controls. Chromium can pass through browser chrome at
  // the tab boundary; the next Shift+Tab returns to the last control, never the conversation.
  await drawer.getByRole('button', { name: '关闭', exact: true }).focus();
  await page.keyboard.press('Shift+Tab');
  await page.keyboard.press('Shift+Tab');
  expect(await drawer.evaluate(element => element.contains(document.activeElement))).toBeTruthy();
  await page.keyboard.press('Escape');
  await expect(drawer).not.toBeVisible();
  await expect(trigger).toBeFocused();
  await expect(input).toHaveValue('请重点关注近两年的论文');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy();
  for (const width of [320, 768, 1024, 1100, 1101, 1280]) {
    await page.setViewportSize({ width, height: 900 });
    await expect(input).toBeInViewport();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy();
  }
});
