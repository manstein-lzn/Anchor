import { expect, test } from '@playwright/test';
import { spawn } from 'node:child_process';
import { createServer, type ServerResponse } from 'node:http';
import { fileURLToPath } from 'node:url';

test('Pilot reconciles a lost submission response and permits corrected input after explicit rejection', async ({ page }) => {
  const session = { id: 'retry', title: '重试对话', status: 'active', updated_at: new Date().toISOString(), approval: null };
  const final = { id: 'accepted', session: session.id, request_id: 'lost-response', status: 'completed',
    error: '', created_at: session.updated_at, prompt: '已经收到的问题' };
  const submitted: Array<{ request_id: string; message: string }> = [];
  await page.addInitScript(() => {
    localStorage.setItem('anchor:view', 'pilot');
    localStorage.setItem('pilot:selected', 'retry');
    localStorage.setItem('pilot:draft:retry', '已经收到的问题');
    localStorage.setItem('pilot:submission:retry', JSON.stringify({ request_id: 'lost-response', message: '已经收到的问题' }));
  });
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [] } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [] } }));
  await page.route('**/timeline*', route => route.fulfill({ json: { runs: [], scheduled: [], schedules: [] } }));
  await page.route('**/sessions', route => route.fulfill({ json: { sessions: [session] } }));
  await page.route('**/sessions/*/messages', route => route.fulfill({ json: { messages: [
    { role: 'user', text: final.prompt }, { role: 'assistant', text: '之前的回复' },
  ] } }));
  await page.route('**/sessions/*/turns', route => {
    if (route.request().method() !== 'POST') return route.fulfill({ json: { turns: [final] } });
    submitted.push(route.request().postDataJSON());
    return route.fulfill({ status: 422, json: { error: '输入需要修改' } });
  });
  await page.route(/\/sessions\/[^/]+\/turns\/[^/]+\/events/, route => route.fulfill({
    contentType: 'text/event-stream', body: `event: turn\ndata: ${JSON.stringify(final)}\n\n`,
  }));
  await page.goto('/');
  const input = page.getByLabel('发送给 Anchor Pilot');
  await expect(input).toBeEnabled();
  await expect(input).toHaveValue('');
  await expect(page.locator('.pilot-message.user')).toHaveCount(1);
  await input.fill('新的问题'); await input.press('Enter');
  await expect(page.locator('.pilot-error')).toContainText('输入需要修改');
  await expect(input).toHaveValue('新的问题');
  await input.fill('修正后的问题'); await input.press('Enter');
  await expect(input).toHaveValue('修正后的问题');
  expect(submitted.map(item => item.message)).toEqual(['新的问题', '修正后的问题']);
  expect(new Set(submitted.map(item => item.request_id)).size).toBe(2);
});

test('Pilot keeps messages, navigation and live connections correct across slow requests and reloads', async ({ page }) => {
  test.setTimeout(60000);
  const stamp = new Date().toISOString();
  const sessions = [{ id: 'other', title: '另一段对话', status: 'active', updated_at: stamp, approval: null }];
  type Message = { role: string; text: string };
  type Turn = { id: string; session: string; status: string; error: string; created_at: string; prompt: string };
  const messages = new Map<string, Message[]>([['other', [{ role: 'assistant', text: '另一段历史消息' }]]]);
  const turns = new Map<string, Turn>();
  const events = new Map<string, object[]>();
  const streams = new Map<string, Set<ServerResponse>>();
  const submitted: Array<{ id: string; request_id: string }> = [];
  let createResponse: ServerResponse | undefined;
  let heldSubmission: ServerResponse | undefined;
  let heldHistory: ServerResponse | undefined;
  let blockHistory = false;
  let failHistory = false;
  let stopped = 0;
  const json = (res: ServerResponse, data: unknown, status = 200) => {
    res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(data));
  };
  const append = (id: string, event: object) => {
    const records = events.get(id) ?? [];
    records.push(event); events.set(id, records);
    for (const res of streams.get(id) ?? []) res.write(`id: ${records.length}\ndata: ${JSON.stringify(event)}\n\n`);
  };
  // Real, deliberately unending HTTP streams expose browser connection leaks which route.fulfill
  // cannot reproduce. Only the provider's output and the persistence delay are synthetic.
  const server = createServer(async (req, res) => {
    const url = new URL(req.url!, 'http://localhost');
    const parts = url.pathname.split('/').filter(Boolean);
    const id = parts[1];
    if (parts[0] === 'graphs') return json(res, { graphs: [] });
    if (parts[0] === 'runs') return json(res, { runs: [] });
    if (parts[0] === 'timeline') return json(res, { runs: [], scheduled: [], schedules: [] });
    if (parts[0] !== 'sessions') return json(res, { error: 'not found' }, 404);
    if (parts.length === 1) {
      if (req.method === 'POST') { createResponse = res; return; }
      return json(res, { sessions });
    }
    if (parts[2] === 'messages') {
      if (id === 'new' && blockHistory) { heldHistory = res; return; }
      if (id === 'new' && failHistory) return json(res, { error: '历史读取暂时失败' }, 503);
      return json(res, { messages: messages.get(id) ?? [] });
    }
    if (parts[2] === 'stop') { stopped++; return json(res, {}); }
    if (parts[4] === 'events') {
      res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' });
      res.write(': connected\n\n');
      const after = Number(url.searchParams.get('after') ?? 0);
      (events.get(id) ?? []).forEach((event, index) => {
        if (index + 1 > after) res.write(`id: ${index + 1}\ndata: ${JSON.stringify(event)}\n\n`);
      });
      const active = streams.get(id) ?? new Set<ServerResponse>();
      active.add(res); streams.set(id, active);
      res.on('close', () => active.delete(res));
      const turn = turns.get(id)!;
      if (turn.status !== 'running') res.end(`event: turn\ndata: ${JSON.stringify(turn)}\n\n`);
      return;
    }
    if (parts[2] === 'turns' && req.method === 'POST') {
      let raw = '';
      for await (const chunk of req) raw += chunk;
      const body = JSON.parse(raw);
      submitted.push({ id, request_id: body.request_id });
      const turn: Turn = { id: `turn-${id}`, session: id, status: 'running', error: '', created_at: stamp, prompt: body.message };
      turns.set(id, turn);
      append(id, { type: 'reasoning-start', id: 'reasoning' });
      append(id, { type: 'reasoning-delta', id: 'reasoning', delta: 'private-model-reasoning' });
      if (id === 'other') { heldSubmission = res; return; }
      return json(res, { turn }, 202);
    }
    if (parts[2] === 'turns') return json(res, { turns: turns.has(id) ? [turns.get(id)] : [] });
    return json(res, { error: 'not found' }, 404);
  });
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('missing API port');
  const probe = createServer();
  await new Promise<void>(resolve => probe.listen(0, '127.0.0.1', resolve));
  const webAddress = probe.address();
  if (!webAddress || typeof webAddress === 'string') throw new Error('missing web port');
  await new Promise<void>(resolve => probe.close(() => resolve()));
  const repo = fileURLToPath(new URL('../../../', import.meta.url));
  const vite = spawn(`${repo}apps/web/node_modules/.bin/vite`, ['--host', '127.0.0.1', '--port', String(webAddress.port)], {
    cwd: `${repo}apps/web`, stdio: 'ignore', env: { ...process.env, ANCHOR_WEB_API_URL: `http://127.0.0.1:${address.port}` },
  });
  const base = `http://127.0.0.1:${webAddress.port}`;
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    await expect(async () => expect((await page.request.get(`${base}/sessions`)).ok()).toBeTruthy()).toPass();
    await page.goto(base);
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    const input = page.getByLabel('发送给 Anchor Pilot');
    await expect(input).toBeEnabled();
    await input.fill('立即显示我的问题');
    await input.press('Enter');
    await expect(page.locator('.pilot-message.user')).toHaveText(/立即显示我的问题/);
    await expect.poll(() => !!createResponse).toBe(true);
    const session = { id: 'new', title: '进行中的对话', status: 'active', updated_at: stamp, approval: null };
    sessions.push(session);
    json(createResponse!, { session });
    await expect(page.locator('.pilot-thinking')).toContainText('模型正在思考');
    await expect(page.locator('.pilot-message.user')).toHaveCount(1);
    await expect(page.locator('.pilot-message.user')).toContainText('立即显示我的问题');
    await expect(page.getByText('private-model-reasoning')).toHaveCount(0);
    await expect(page.getByRole('button', { name: '新对话', exact: true })).toBeEnabled();

    const activeStreams = () => [...streams.values()].reduce((sum, active) => sum + active.size, 0);
    const other = page.locator('.pilot-session[title="other"]');
    const current = page.locator('.pilot-session[title="new"]');
    for (let i = 0; i < 8; i++) {
      await other.click();
      await expect(page.getByText('另一段历史消息', { exact: true })).toBeVisible();
      await expect.poll(activeStreams).toBe(0);
      await current.click();
      await expect(page.locator('.pilot-thinking')).toContainText('模型正在思考');
      await expect(page.locator('.pilot-message.user')).toHaveCount(1);
      await expect.poll(activeStreams).toBe(1);
    }
    expect(stopped).toBe(0);
    expect(submitted).toHaveLength(1);
    await page.reload();
    await expect(page.getByRole('button', { name: 'Pilot', exact: true })).toHaveAttribute('aria-pressed', 'true');
    await expect(page.locator('.pilot-chat-heading strong')).toHaveAttribute('title', 'new');
    await expect(page.locator('.pilot-thinking')).toContainText('模型正在思考');
    await expect.poll(activeStreams).toBe(1);

    append('new', { type: 'reasoning-end', id: 'reasoning' });
    append('new', { type: 'tool-input-start', toolCallId: 'validate', toolName: 'graph_validate' });
    append('new', { type: 'tool-input-delta', toolCallId: 'validate', inputTextDelta: '{"definition":' });
    await expect(page.locator('.pilot-tool summary')).toContainText('准备参数');
    await expect(page.locator('.pilot-thinking')).toContainText('正在准备工具参数：graph_validate');
    append('new', { type: 'tool-input-available', toolCallId: 'validate', toolName: 'graph_validate', input: {} });
    await expect(page.locator('.pilot-tool summary')).toContainText('执行中');
    append('new', { type: 'tool-output-available', toolCallId: 'validate', output: { valid: true } });
    await expect(page.locator('.pilot-tool summary')).toContainText('已返回');

    // A late submission response must not replace the session the user has since selected.
    await other.click();
    await expect(input).toBeEnabled();
    await input.fill('后台提交的问题'); await input.press('Enter');
    await expect.poll(() => !!heldSubmission).toBe(true);
    await current.click();
    json(heldSubmission!, { turn: turns.get('other') }, 202);
    await expect(page.locator('.pilot-chat-heading strong')).toHaveAttribute('title', 'new');
    await expect(page.getByText('后台提交的问题', { exact: true })).toHaveCount(0);
    await expect(page.locator('.pilot-message.user')).toContainText('立即显示我的问题');

    // A slow history load is cancellable; its late response cannot contaminate the next session.
    await other.click();
    await expect(page.locator('.pilot-thinking')).toContainText('模型正在思考');
    blockHistory = true;
    await current.click();
    await expect.poll(() => !!heldHistory).toBe(true);
    await other.click();
    await expect(page.locator('.pilot-message.user')).toContainText('后台提交的问题');
    json(heldHistory!, { messages: [{ role: 'assistant', text: '不应出现的过期历史' }] });
    await expect(page.getByText('不应出现的过期历史')).toHaveCount(0);
    blockHistory = false;
    await current.click();
    await expect(page.locator('.pilot-tool summary')).toContainText('已返回');
    failHistory = true;
    append('new', { type: 'text-start', id: 'answer' });
    append('new', { type: 'text-delta', id: 'answer', delta: '最终回复不会因历史加载失败而消失。' });
    const final = { ...turns.get('new')!, status: 'completed' };
    turns.set('new', final);
    messages.set('new', [{ role: 'user', text: final.prompt }, { role: 'assistant', text: '最终回复不会因历史加载失败而消失。' }]);
    for (const res of streams.get('new') ?? []) res.end(`event: turn\ndata: ${JSON.stringify(final)}\n\n`);
    await expect(page.locator('.pilot-error')).toContainText('历史读取暂时失败');
    await expect(page.getByText('最终回复不会因历史加载失败而消失。', { exact: true })).toBeVisible();
    await expect(input).toBeEnabled();
    failHistory = false;
    await page.getByRole('button', { name: '重新加载对话' }).click();
    await expect(page.locator('.pilot-message.assistant')).toHaveCount(1);
    await expect(page.locator('.pilot-error')).toHaveCount(0);
    await expect.poll(activeStreams).toBe(0);
    expect(submitted).toHaveLength(2);
    expect(stopped).toBe(0);
    expect(errors).toEqual([]);
    await page.screenshot({ path: test.info().outputPath('pilot-lifecycle.png') });
  } finally {
    await page.close();
    if (vite.exitCode === null && vite.signalCode === null) {
      const exited = new Promise<void>(resolve => vite.once('exit', () => resolve()));
      vite.kill(); await exited;
    }
    server.closeAllConnections();
    await new Promise<void>(resolve => server.close(() => resolve()));
  }
});
