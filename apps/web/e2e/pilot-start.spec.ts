import { expect, test, type Page } from '@playwright/test';

type SavedTurn = {
  id: string; session: string; status: string; prompt: string | null;
  request_id: string; error: string; created_at: string;
};

async function fixture(page: Page, options: { failFirstTurn?: boolean; firstTurnError?: string; modelConfigured?: boolean; modelStatusFailure?: boolean } = {}) {
  const session = { id: 'start-session', title: '', status: 'active', waiting_reason: '',
    updated_at: '2026-10-14T03:00:00Z', approval: null };
  const sessions: typeof session[] = [];
  const messages: { role: string; text: string }[] = [];
  const turns: SavedTurn[] = [];
  const emptyFields = { endpoint: '', wire_api: 'chat', flash: '', pro: '' };
  const model = {
    providers: {
      default: { fields: options.modelConfigured === false ? { ...emptyFields } : { ...emptyFields, endpoint: 'https://provider.example/v1', flash: 'fixture-flash' },
        api_key_present: options.modelConfigured !== false, configured: false, source: 'host' },
      backup: { fields: { ...emptyFields }, api_key_present: false, configured: false, source: 'empty' },
    },
    execution: { configured: options.modelConfigured !== false }, retained_providers: [],
  };
  const facts = { sessionWrites: 0, turnWrites: [] as { message?: string; request_id: string }[],
    pluginReads: 0, modelWrites: 0, unexpected: [] as string[] };
  await page.addInitScript(() => localStorage.setItem('anchor:view', 'pilot'));
  await page.route('**/*', async route => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const method = request.method();
    if (!/^\/(sessions|graphs|runs|timeline|plugins|settings)(\/|$)/.test(path)) return route.continue();
    if (path === '/settings/model') return route.fulfill(options.modelStatusFailure
      ? { status: 500, json: { error: 'Cannot read model settings' } } : { json: model });
    if (path === '/settings/model/providers/default' && method === 'PUT') {
      const body = request.postDataJSON();
      facts.modelWrites++;
      model.providers.default.fields = { endpoint: body.endpoint, wire_api: body.wire_api, flash: body.flash, pro: body.pro };
      model.providers.default.api_key_present = body.api_key.action === 'set';
      model.execution.configured = true;
      return route.fulfill({ json: model });
    }
    if (path === '/settings/runtime') return route.fulfill({ json: { runtime: { python_configuration_owner: 'host',
      rust_configuration_owner: 'host', python_default_version: '3.12', rust_default_toolchain: 'stable' }, operation: null } });
    if (path === '/graphs') return route.fulfill({ json: { graphs: [] } });
    if (path === '/runs') return route.fulfill({ json: { runs: [] } });
    if (path === '/timeline') return route.fulfill({ json: { runs: [], scheduled: [], schedules: [] } });
    if (path === '/sessions') {
      if (method === 'POST') {
        facts.sessionWrites++;
        sessions.push(session);
        return route.fulfill({ status: 201, json: { session } });
      }
      return route.fulfill({ json: { sessions } });
    }
    if (path === '/sessions/start-session/messages') return route.fulfill({ json: { messages } });
    if (path === '/sessions/start-session/credential-requests') return route.fulfill({ json: { requests: [] } });
    if (path === '/sessions/start-session/plugins') {
      facts.pluginReads++;
      return route.fulfill({ json: { plugins: [{ id: 'network', name: '联网', on: true,
        explicitOn: true, available: true, networkProvider: true, networkAllowed: true, networkMode: 'enabled' }] } });
    }
    if (path === '/sessions/start-session/turns') {
      if (method !== 'POST') return route.fulfill({ json: { turns } });
      const body = request.postDataJSON();
      facts.turnWrites.push(body);
      if (!model.execution.configured) return route.fulfill({ status: 503, json: { error: 'Pilot provider is not configured or unavailable' } });
      if (options.failFirstTurn && facts.turnWrites.length === 1) return route.fulfill({ status: 503,
        json: { error: options.firstTurnError ?? '暂时无法接收任务，请保留原内容重试。' } });
      messages.push({ role: 'user', text: body.message });
      const turn = { id: 'turn-1', session: session.id, status: 'running', prompt: body.message,
        request_id: body.request_id, error: '', created_at: session.updated_at };
      turns.unshift(turn);
      return route.fulfill({ status: 202, json: { turn } });
    }
    if (path === '/sessions/start-session/turns/turn-1/events') {
      turns[0] = { ...turns[0], status: 'completed' };
      if (!messages.some(message => message.role === 'assistant')) messages.push({ role: 'assistant', text: '已收到任务，我们开始。' });
      return route.fulfill({ contentType: 'text/event-stream',
        body: `event: turn\ndata: ${JSON.stringify(turns[0])}\n\n` });
    }
    if (path === '/sessions/start-session/turns/turn-1') return route.fulfill({
      json: { turn: turns[0], continuation: { status: 'none' } },
    });
    if (path === '/sessions/start-session/turns/turn-1/questions') return route.fulfill({ json: { questions: [] } });
    facts.unexpected.push(`${method} ${path}`);
    return route.fulfill({ status: 404, json: { error: 'Unmocked API in Pilot start fixture' } });
  });
  return facts;
}

test('an empty installation presents one task composer and examples only edit a saved draft', async ({ page }) => {
  const facts = await fixture(page);
  await page.goto('/');
  const heading = page.getByRole('heading', { name: '你想完成什么？', exact: true });
  const input = page.getByLabel('发送给 Anchor Pilot');
  const examples = page.locator('.pilot-starters button');
  await expect(heading).toBeVisible();
  await expect(input).toBeEnabled();
  await expect(page.locator('.pilot-composer')).toHaveCount(1);
  await expect(page.getByRole('log', { name: '对话消息' }).getByRole('heading')).toHaveCount(0);
  await expect(examples).toHaveCount(3);
  await expect(page.getByRole('button', { name: '新建对话', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '展开插件面板', exact: true })).toHaveAttribute('aria-expanded', 'false');
  await expect(page.locator('.plugin-card')).toHaveCount(0);
  for (const [width, height] of [[1440, 1000], [768, 900], [390, 844], [320, 740], [1280, 600]]) {
    await page.setViewportSize({ width, height });
    await expect(heading).toBeInViewport();
    await expect(input).toBeInViewport();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy();
    await page.screenshot({ path: test.info().outputPath(`pilot-start-${width}.png`), fullPage: true });
  }
  for (const example of await examples.all()) {
    await example.click();
    await expect(input).toBeFocused();
    expect((await input.inputValue()).length).toBeGreaterThan(15);
  }
  expect(facts.sessionWrites).toBe(0);
  expect(facts.turnWrites).toHaveLength(0);
  expect(facts.pluginReads).toBe(0);
  const draft = await input.inputValue();
  await page.reload();
  await expect(input).toHaveValue(draft);
  await expect(heading).toBeVisible();
  expect(facts.sessionWrites).toBe(0);
  expect(facts.unexpected).toEqual([]);
});

test('the first task creates one session and moves the same composer into the conversation', async ({ page }) => {
  const facts = await fixture(page);
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto('/');
  const input = page.getByLabel('发送给 Anchor Pilot');
  await expect(input).toBeEnabled();
  await input.evaluate(element => { element.dataset.composerIdentity = 'retained'; });
  const prompt = '帮我开发一个整理 CSV 数据的小工具，先确认输入和输出格式。';
  await input.fill(prompt);
  await input.evaluate(element => element.dispatchEvent(new KeyboardEvent('keydown', {
    key: 'Enter', isComposing: true, bubbles: true,
  })));
  expect(facts.turnWrites).toHaveLength(0);
  await input.press('Enter');
  await expect(page.locator('article.pilot-message.assistant')).toContainText('已收到任务，我们开始。');
  await expect(page.locator('article.pilot-message.user')).toHaveCount(1);
  await expect(page.getByRole('heading', { name: '你想完成什么？', exact: true })).toHaveCount(0);
  await expect(page.locator('.pilot-starters')).toHaveCount(0);
  await expect(input).toHaveAttribute('data-composer-identity', 'retained');
  await expect(input).toHaveValue('');
  await expect(input).toBeEnabled();
  expect(facts.sessionWrites).toBe(1);
  expect(facts.turnWrites).toHaveLength(1);
  expect(facts.turnWrites[0].message).toBe(prompt);
  await expect(page.getByRole('button', { name: '展开插件面板', exact: true })).toHaveAttribute('aria-expanded', 'false');
  await page.getByRole('button', { name: '展开插件面板', exact: true }).click();
  await expect(page.getByRole('combobox', { name: '联网策略', exact: true })).toHaveValue('enabled');
  await input.fill('后续草稿保留');
  await page.getByRole('button', { name: '收起插件面板', exact: true }).click();
  await expect(input).toHaveValue('后续草稿保留');
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(input).toBeInViewport();
  await page.screenshot({ path: test.info().outputPath('pilot-task-mobile.png'), fullPage: true });
  expect(facts.unexpected).toEqual([]);
  expect(errors).toEqual([]);
});

test('a failed first submission keeps the task and reuses its request identity on retry', async ({ page }) => {
  const facts = await fixture(page, { failFirstTurn: true });
  await page.goto('/');
  const input = page.getByLabel('发送给 Anchor Pilot');
  await expect(input).toBeEnabled();
  const prompt = '检查这个插件需要哪些配置，先不要安装。';
  await input.fill(prompt);
  await input.press('Enter');
  await expect(page.locator('.pilot-chat').getByRole('alert')).toContainText('暂时无法接收任务');
  await expect(input).toHaveValue(prompt);
  await expect(input).toBeEnabled();
  await input.press('Enter');
  await expect(page.locator('article.pilot-message.assistant')).toContainText('已收到任务，我们开始。');
  await expect(page.locator('article.pilot-message.user')).toHaveCount(1);
  expect(facts.sessionWrites).toBe(1);
  expect(facts.turnWrites).toHaveLength(2);
  expect(facts.turnWrites[0]).toEqual(facts.turnWrites[1]);
  expect(facts.unexpected).toEqual([]);
});

test('missing default Flash settings guide users to Settings and keep the task and attachments for retry', async ({ page }) => {
  const facts = await fixture(page, { modelConfigured: false });
  await page.goto('/');
  const input = page.getByLabel('发送给 Anchor Pilot');
  const prompt = '请检查这个 CSV。';
  await expect(input).toBeEnabled();
  await input.fill(prompt);
  await page.getByLabel('选择附件').setInputFiles({ name: 'input.csv', mimeType: 'text/csv', buffer: Buffer.from('value\n1\n') });
  await expect(page.locator('.pilot-composer')).toContainText('input.csv');
  await input.press('Enter');
  await expect(page.locator('.pilot-chat').getByRole('alert')).toContainText('请先在「设置 → 模型服务」配置默认服务');
  await expect(input).toHaveValue(prompt);
  await expect(page.locator('.pilot-composer')).toContainText('input.csv');
  await expect(page.locator('article.pilot-message.assistant')).toHaveCount(0);
  await page.getByRole('button', { name: '前往模型设置', exact: true }).click();
  const provider = page.getByRole('group', { name: 'default', exact: true });
  await provider.getByLabel('服务地址').fill('https://provider.example/v1');
  await provider.getByLabel('Flash 模型', { exact: true }).fill('fixture-flash');
  await provider.getByLabel('模型服务 API key').fill('fixture-only-key');
  await provider.getByRole('button', { name: '保存配置', exact: true }).click();
  await expect(provider).toContainText('配置已保存，下一轮使用新设置');
  await expect(provider.getByLabel('Pro 模型', { exact: true })).toHaveValue('');
  await page.getByRole('button', { name: 'Pilot', exact: true }).click();
  await expect(input).toHaveValue(prompt);
  await expect(page.locator('.pilot-composer')).toContainText('input.csv');
  await expect(input).toBeEnabled();
  await input.press('Enter');
  await expect(page.locator('article.pilot-message.assistant')).toContainText('已收到任务，我们开始。');
  await expect(input).toHaveValue('');
  await expect(page.locator('.pilot-composer')).not.toContainText('input.csv');
  expect(facts.sessionWrites).toBe(1);
  expect(facts.modelWrites).toBe(1);
  expect(facts.turnWrites).toHaveLength(2);
  expect(facts.turnWrites[1]).toEqual(facts.turnWrites[0]);
  expect(facts.unexpected).toEqual([]);
});

for (const modelStatusFailure of [false, true]) {
  test(`an unavailable provider keeps its actual error when settings ${modelStatusFailure ? 'cannot be read' : 'have an executable Flash model'}`, async ({ page }) => {
    const error = 'Pilot provider is not configured or unavailable';
    const facts = await fixture(page, { failFirstTurn: true, firstTurnError: error, modelStatusFailure });
    await page.goto('/');
    const input = page.getByLabel('发送给 Anchor Pilot');
    await expect(input).toBeEnabled();
    await input.fill('保留这条草稿');
    await input.press('Enter');
    await expect(page.locator('.pilot-chat').getByRole('alert')).toContainText(error);
    await expect(page.getByRole('button', { name: '前往模型设置', exact: true })).toHaveCount(0);
    await expect(input).toHaveValue('保留这条草稿');
    expect(facts.unexpected).toEqual([]);
  });
}
