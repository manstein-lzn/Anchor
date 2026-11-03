import { test, expect } from '@playwright/test';
import { runProjectionFixture } from './fixtures/runProjection';

for (const width of [1440, 390]) {
  test(`a Graph's bot and its conversations use Graph and Run entries at ${width}px`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 900 });
    await page.addInitScript(() => localStorage.setItem('anchor:view', 'channels'));
    const definition = { objective: '消息触发测试', entry: 'assistant', agents: { assistant: { model: 'fixture' } },
      nodes: [{ id: 'assistant', agent: 'assistant', plugins: ['gateway'] }], edges: [] };
    const graph = 'message-workflow';
    const binding = { plugin: 'gateway', users: [] as string[], send_users: [] as string[] };
    let configuration: Record<string, unknown> = { graph, configured: false, enabled: false, listener: 'stopped', mutable: true };
    const writes: { path: string; method: string; body: unknown }[] = [];
    const problems: string[] = [];
    page.on('pageerror', error => problems.push(error.message));
    const projection = runProjectionFixture('executing', { active: true, actions: { pause: true, stop: true, resume: false, abandon: true } });
    const run = { run: 'current-run', graph, running: true, active: true, projection,
      state: { objective: definition.objective, cursor: { node: 'assistant', pass: 1, dir: '' }, passes: {}, nodes: {}, skipped: [], decided: {},
        executed: [], error: '', started: '2026-10-16T10:00:00Z', updated: '2026-10-16T10:00:00Z' },
      definition, traces: {}, nodes: ['assistant'] };
    await page.route('**/*', route => {
      const path = new URL(route.request().url()).pathname;
      const method = route.request().method();
      if (method !== 'GET') writes.push({ path, method, body: route.request().postDataJSON() });
      if (path === '/graphs') return route.fulfill({ json: { graphs: [{ graph, running: null, listener: { platform: 'wecom', status: configuration.listener } }] } });
      if (path === `/graphs/${graph}`) return route.fulfill({ json: { definition } });
      if (path === '/runs') return route.fulfill({ json: { runs: [] } });
      if (path === '/timeline') return route.fulfill({ json: { runs: [], schedules: [], scheduled: [] } });
      if (path === '/plugins') return route.fulfill({ json: { plugins: [{ id: 'gateway', name: 'gateway', description: '' }] } });
      if (path === '/plugins/gateway') return route.fulfill({ json: { id: 'gateway', name: 'gateway', description: '', channels: [{ platform: 'wecom' }] } });
      if (path === '/plugins/gateway/configuration') return route.fulfill({ status: 404, json: { error: 'no configuration' } });
      if (path === '/plugins/gateway/credentials') return route.fulfill({ json: { plugin: 'gateway', targets: [] } });
      if (path === `/graphs/${graph}/channel/credentials`) return route.fulfill({ json: { plugin: 'gateway', targets: [{ target: `channel_wecom@${graph}`, kind: 'channel', authorization_digest: 'fixture', fields: [{ field: 'WECOM_BOT_ID', state: 'configured', editable: true }] }] } });
      if (path === `/graphs/${graph}/channel`) {
        if (method === 'PUT') configuration = { ...configuration, configured: true, owner_matches: true, binding: { ...binding, ...route.request().postDataJSON() } };
        return route.fulfill({ json: configuration });
      }
      if (path === `/graphs/${graph}/channel/enabled`) {
        const { enabled, expected_binding } = route.request().postDataJSON();
        expect(expected_binding).toEqual({ ...binding, users: ['alice'], send_users: [], private_only: true });
        configuration = { ...configuration, enabled, listener: enabled ? 'authenticated' : 'stopped' };
        return route.fulfill({ json: configuration });
      }
      if (path === '/channel-sessions') return route.fulfill({ json: { sessions: [{ id: 'conversation', title: 'Alice 的对话', graph, platform: 'wecom' }] } });
      if (path === '/channel-sessions/conversation') return route.fulfill({ json: { session: { run_ids: [run.run] } } });
      if (path === '/channel-sessions/conversation/assistant') return route.fulfill({ json: { assistant: { run_id: run.run, wait_node: 'assistant', work_node: 'assistant', reply_node: 'assistant' }, needs_recovery: false } });
      if (path === '/channel-sessions/conversation/deliveries/unresolved') return route.fulfill({ json: { deliveries: [] } });
      if (path === `/runs/${run.run}`) return route.fulfill({ json: run });
      return route.continue();
    });
    await page.goto('/');
    await expect(page.getByRole('button', { name: '渠道', exact: true })).toHaveCount(0);
    await expect(page.getByLabel('当前工作流')).toHaveValue(graph);
    await expect(page.getByRole('button', { name: '消息触发', exact: true })).toHaveCount(0);
    await page.locator('[data-id="assistant"]').click();
    const details = page.getByRole('button', { name: '查看 gateway 的详情', exact: true });
    await details.click();
    const dialog = page.getByRole('dialog', { name: 'gateway · 插件详情', exact: true });
    const bot = dialog.getByRole('region', { name: '此 Graph 的机器人', exact: true });
    await expect(bot).toContainText('尚未保存成员');
    await expect(bot.getByRole('button', { name: '启用机器人', exact: true })).toBeDisabled();
    await bot.getByLabel('允许接收的成员').fill('alice');
    await bot.getByRole('checkbox', { name: /只接收私聊/ }).check();
    await bot.getByRole('button', { name: '保存成员', exact: true }).click();
    await expect(bot).toContainText('成员已保存');
    await expect(bot.getByRole('region', { name: '插件凭据', exact: true })).toContainText(`channel_wecom@${graph}`);
    await bot.getByRole('button', { name: '启用机器人', exact: true }).click();
    await expect(bot).toContainText('常驻监听');
    await expect(bot.getByLabel('允许接收的成员')).toBeDisabled();
    await expect(bot.getByRole('checkbox', { name: /只接收私聊/ })).toBeChecked();
    await expect(bot.getByRole('region', { name: '插件凭据', exact: true })).toHaveCount(0);
    expect(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
    await page.screenshot({ path: info.outputPath(`graph-bot-${width}.png`), fullPage: true });
    await page.keyboard.press('Escape');
    await expect(dialog).not.toBeVisible();
    await page.getByRole('button', { name: '查看会话', exact: true }).click();
    const conversations = page.getByRole('dialog', { name: '消息会话', exact: true });
    await expect(conversations).toContainText('Alice 的对话');
    await conversations.getByRole('button', { name: run.run, exact: true }).click();
    await expect(conversations).not.toBeVisible();
    await expect(page.getByRole('button', { name: '返回时间线', exact: true })).toBeVisible();
    await page.getByRole('button', { name: '返回时间线', exact: true }).click();
    const entry = page.getByRole('button', { name: '消息会话', exact: true });
    await entry.click();
    await expect(conversations).toContainText('等待消息');
    await expect(conversations.getByRole('button', { name: '退役实例', exact: true })).toBeDisabled();
    expect(await conversations.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
    await page.screenshot({ path: info.outputPath(`message-sessions-${width}.png`), fullPage: true });
    await page.keyboard.press('Escape');
    await expect(conversations).not.toBeVisible();
    await expect(entry).toBeFocused();
    expect(writes.map(({ path, method }) => ({ path, method }))).toEqual([
      { path: `/graphs/${graph}/channel`, method: 'PUT' }, { path: `/graphs/${graph}/channel/enabled`, method: 'POST' },
    ]);
    expect(problems).toEqual([]);
  });
}
