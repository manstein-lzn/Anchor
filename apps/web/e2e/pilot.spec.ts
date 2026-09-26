import { expect, test } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

type Turn = { id: string; session: string; status: string; error: string; created_at: string; prompt: string | null };

/** A body `EventSource` reads the same way `anchor serve` writes it. */
function stream(parts: Array<[number, unknown]>, turn?: Turn, retry?: number) {
  const events: string[][] = [];
  if (retry) events.push([`retry: ${retry}`]);
  for (const [id, data] of parts) events.push([`id: ${id}`, `data: ${JSON.stringify(data)}`]);
  if (turn) events.push(['event: turn', `data: ${JSON.stringify(turn)}`]);
  // Every event needs its own blank line; without it EventSource discards the last one at close.
  return events.map(event => `${event.join('\n')}\n\n`).join('');
}

const inProgress = (id: string, session: string, prompt: string): Turn => ({
  id, session, status: 'running', error: '', created_at: '2026-09-26T07:00:00Z', prompt,
});

async function freePort() {
  const server = createServer();
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('no test port');
  await new Promise<void>(resolve => server.close(() => resolve()));
  return address.port;
}

test('Pilot opens, creates and reopens sessions through the real Vite proxy, and shows failures', async ({ page, request }) => {
  test.setTimeout(60000);
  const repo = fileURLToPath(new URL('../../../', import.meta.url));
  const root = await mkdtemp(join(tmpdir(), 'anchor-pilot-browser-'));
  const processes: ChildProcess[] = [];
  const apiPort = await freePort();
  const webPort = await freePort();
  const base = `http://127.0.0.1:${webPort}`;
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    const config = join(root, 'runtime.json');
    await writeFile(config, JSON.stringify({ models: [] }));
    processes.push(spawn(join(repo, '.venv/bin/python'), ['-m', 'anchor', '--root', root,
      '--config', config, '--host', '127.0.0.1', '--port', String(apiPort)], { cwd: repo, stdio: 'ignore' }));
    processes.push(spawn(join(repo, 'apps/web/node_modules/.bin/vite'),
      ['--host', '127.0.0.1', '--port', String(webPort)], {
        cwd: join(repo, 'apps/web'), stdio: 'ignore',
        env: { ...process.env, ANCHOR_WEB_API_URL: `http://127.0.0.1:${apiPort}` },
      }));
    // Requesting the real proxy is essential: mocked routes hid the missing /sessions mapping.
    await expect(async () => {
      const response = await request.get(`${base}/sessions`);
      expect(response.ok()).toBeTruthy();
      expect(await response.json()).toEqual({ sessions: [] });
    }).toPass({ timeout: 15000 });
    await page.goto(base);
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    await expect(page.getByRole('heading', { name: '从一个问题，开始探索' })).toBeVisible();
    await page.getByRole('button', { name: '新建对话', exact: true }).click();
    await expect(page.getByLabel('发送给 Anchor Pilot')).toBeEnabled();
    const { sessions } = await (await request.get(`${base}/sessions`)).json();
    expect(sessions).toHaveLength(1);
    const id = sessions[0].id;
    await page.reload();
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    await page.locator(`.pilot-session[title="${id}"]`).click();
    await expect(page.locator('.pilot-chat-heading strong')).toHaveAttribute('title', id);
    await expect(page.getByLabel('发送给 Anchor Pilot')).toBeEnabled();
    await page.screenshot({ path: test.info().outputPath('pilot-open.png') });

    // Both errors must be visible even before a conversation is selected.
    await page.route('**/sessions', route => route.fulfill({
      status: 200, contentType: 'text/html', body: '<html>wrong upstream</html>',
    }));
    await page.reload();
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    await expect(page.locator('.pilot-chat').getByRole('alert')).toContainText('未返回有效 JSON');
    await page.unroute('**/sessions');
    await page.route('**/sessions', route => route.request().method() === 'POST'
      ? route.fulfill({ status: 503, json: { error: '会话服务暂不可用' } }) : route.continue());
    await page.reload();
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    await page.getByRole('button', { name: '新对话', exact: true }).click();
    await expect(page.locator('.pilot-chat').getByRole('alert')).toHaveText('会话服务暂不可用');
    expect(errors).toEqual([]);
  } finally {
    for (const process of processes.reverse()) {
      if (process.exitCode === null && process.signalCode === null) {
        const exited = new Promise<void>(resolve => process.once('exit', () => resolve()));
        process.kill();
        await exited;
      }
    }
    await rm(root, { recursive: true, force: true });
  }
});

test('Pilot renders rich replies, restores drafts and selection, and handles Chinese input on mobile', async ({ page }) => {
  const sessions = [
    { id: 'research', title: '梳理 RAG 的研究进展', status: 'active', updated_at: '2026-09-26T07:00:00Z', approval: null },
    { id: 'other', title: '另一项研究', status: 'active', updated_at: '2026-09-25T07:00:00Z', approval: null },
  ];
  const text = '## 研究进展\n\n**证据优先**，查看[论文](https://example.com)。\n\n| 方法 | 结果 |\n| --- | --- |\n| RAG | 有效 |\n\n```python\nprint("evidence")\n```\n\n<script>alert("unsafe")</script>';
  const messages = [{ role: 'user', text: '梳理 RAG 的研究进展' }, { role: 'assistant', text }];
  let turns: Turn[] = [];
  let sent = 0;
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [] } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [] } }));
  await page.route('**/sessions', route => route.fulfill({ json: { sessions } }));
  await page.route('**/sessions/*/messages', route => route.fulfill({ json: { messages } }));
  await page.route('**/sessions/*/turns', async route => {
    if (route.request().method() !== 'POST') return route.fulfill({ json: { turns } });
    sent++;
    const prompt = route.request().postDataJSON().message as string;
    const turn = inProgress(`turn-${sent}`, 'research', prompt);
    turns = [turn, ...turns];
    messages.push({ role: 'user', text: prompt }, { role: 'assistant', text: '已收到你的补充。' });
    await route.fulfill({ status: 202, json: { turn } });
  });
  await page.route('**/sessions/*/turns/*/events', route => route.fulfill({
    status: 200, contentType: 'text/event-stream',
    body: stream([[1, { type: 'text-delta', id: 'answer', delta: '已收到你的补充。' }]], { ...turns[0], status: 'completed' }),
  }));
  await page.goto('/');
  await page.getByRole('button', { name: 'Pilot', exact: true }).click();
  await page.locator('.pilot-session').filter({ hasText: sessions[0].title }).click();
  await expect(page.getByRole('heading', { name: '研究进展' })).toBeVisible();
  await expect(page.locator('.pilot-message.assistant strong')).toHaveText('证据优先');
  await expect(page.locator('.pilot-message table')).toContainText('有效');
  await expect(page.locator('.pilot-message pre code')).toContainText('print("evidence")');
  await expect(page.locator('.pilot-message script')).toHaveCount(0);
  await page.getByLabel('搜索对话').fill('RAG');
  await expect(page.locator('.pilot-session')).toHaveCount(1);
  const input = page.getByLabel('发送给 Anchor Pilot');
  await input.fill('请补充对比');
  await page.reload();
  await page.getByRole('button', { name: 'Pilot', exact: true }).click();
  await expect(input).toHaveValue('请补充对比');
  await expect(page.locator('.pilot-chat-heading strong')).toHaveText(sessions[0].title);
  await input.evaluate(el => el.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', isComposing: true, bubbles: true })));
  await expect(input).toHaveValue('请补充对比');
  expect(sent).toBe(0);
  await input.press('Shift+Enter');
  await expect(input).toHaveValue('请补充对比\n');
  await input.press('Enter');
  await expect(page.locator('.pilot-message.assistant').last()).toContainText('已收到你的补充');
  expect(sent).toBe(1);
  await page.screenshot({ path: test.info().outputPath('pilot-conversation-desktop.png'), fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(input).toBeVisible();
  await expect(page.getByRole('button', { name: '发送', exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy();
  await page.screenshot({ path: test.info().outputPath('pilot-conversation-mobile.png'), fullPage: true });
});

test('Pilot streams text and tool activity, and resumes the stream from the last delivered event', async ({ page }) => {
  test.setTimeout(60000);
  const session = { id: 'stream', title: '流式研究', status: 'active', updated_at: '2026-09-26T07:00:00Z', approval: null };
  const messages: Array<{ role: string; text: string }> = [];
  let turns: Turn[] = [];
  let reconnects = 0;
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [] } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [] } }));
  await page.route('**/sessions', route => route.fulfill({ json: { sessions: [session] } }));
  await page.route('**/sessions/*/messages', route => route.fulfill({ json: { messages } }));
  await page.route('**/sessions/*/turns', async route => {
    if (route.request().method() !== 'POST') return route.fulfill({ json: { turns } });
    const prompt = route.request().postDataJSON().message as string;
    messages.push({ role: 'user', text: prompt });
    turns = [inProgress('turn-1', 'stream', prompt)];
    await route.fulfill({ status: 202, json: { turn: turns[0] } });
  });
  // Playwright's fulfilled responses do not carry Last-Event-ID back into the reconnect request, so
  // this covers the client's own cursor dedupe; the server's header handling lives in test_pilot_turns.py.
  await page.route('**/sessions/*/turns/*/events', route => {
    const header = { status: 200, contentType: 'text/event-stream' };
    reconnects += 1;
    if (reconnects === 1) return route.fulfill({ ...header, body: stream([
      [1, { type: 'text-start', id: 'answer' }],
      [2, { type: 'text-delta', id: 'answer', delta: '你好' }],
      [3, { type: 'tool-input-available', toolCallId: 'call-1', toolName: 'search', input: { query: 'anchor' } }],
      [4, { type: 'tool-output-available', toolCallId: 'call-1', output: { hits: 2 } }],
    ], undefined, 300) });
    if (reconnects === 2) return route.fulfill({ ...header, body: stream([
      [5, { type: 'text-delta', id: 'answer', delta: '，已读取。' }],
    ], undefined, 3000) });
    messages.push({ role: 'assistant', text: '你好，已读取。' });
    return route.fulfill({ ...header, body: stream([], { ...turns[0], status: 'completed' }) });
  });
  await page.goto('/');
  await page.getByRole('button', { name: 'Pilot', exact: true }).click();
  await page.locator('.pilot-session').filter({ hasText: session.title }).click();
  const input = page.getByLabel('发送给 Anchor Pilot');
  await input.fill('读一下状态');
  await input.press('Enter');
  await expect(page.locator('.pilot-tools > summary')).toContainText('工具活动 · 1 项');
  await expect(page.locator('.pilot-tool summary')).toContainText('search');
  await expect(page.locator('.pilot-tool summary')).toContainText('已返回');
  // Only events after the delivered cursor arrive, so the delta is not replayed into the answer.
  await expect(page.locator('.pilot-turn .pilot-message.assistant p')).toHaveText('你好，已读取。');
  await expect(page.locator('.pilot-turn-status')).toHaveText('回复完成');
  await expect(page.getByRole('button', { name: '停止', exact: true })).toHaveCount(0);
  await expect(input).toBeEnabled();
  // The finished turn leaves one saved message behind, so the streamed copy is gone rather than duplicated.
  await expect(page.locator('.pilot-turn .pilot-message.assistant')).toHaveCount(0);
  await expect(page.locator('article.pilot-message.assistant:has(.pilot-copy)').last()).toContainText('你好，已读取。');
  expect(reconnects).toBe(3);
  await page.screenshot({ path: test.info().outputPath('pilot-stream-reconnect.png'), fullPage: true });
});

test('Pilot confirms a deletion before it runs and continues the paused run after the decision', async ({ page }) => {
  const base = { id: 'gated', title: '删除研究', status: 'active', updated_at: '2026-09-26T07:00:00Z', approval: null };
  const messages: Array<{ role: string; text: string }> = [];
  const calls: string[] = [];
  let resumed = false;
  const pending = {
    tool_call_id: 'call-1', key: 'call-1', action: 'graph_delete', target: 'demo',
    proposal: { graph: 'demo' },
  };
  const session = () => ({
    ...base,
    status: resumed ? 'active' : pending ? 'waiting_user' : 'active',
    approval: resumed ? null : { ...pending, status: 'requested' },
  });
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [] } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [] } }));
  await page.route('**/sessions', route => route.fulfill({ json: { sessions: [session()] } }));
  await page.route('**/sessions/*/messages', route => route.fulfill({ json: { messages } }));
  await page.route('**/sessions/*/turns/*/events', route => route.fulfill({
    status: 200, contentType: 'text/event-stream',
    body: stream([[1, { type: 'tool-approval-request', approvalId: 'call-1', toolCallId: 'call-1' }]],
      { ...inProgress('turn-1', 'gated', ''), status: resumed ? 'completed' : 'waiting_approval' }),
  }));
  await page.route('**/sessions/*/turns', route => {
    if (route.request().method() !== 'POST') return route.fulfill({ json: { turns: [] } });
    const body = route.request().postDataJSON();
    calls.push(body.resume ? 'resume' : body.message);
    if (body.resume) {
      resumed = true;
      messages.push({ role: 'assistant', text: '已删除 demo。' });
    } else messages.push({ role: 'user', text: body.message });
    return route.fulfill({ status: 202, json: { turn: inProgress('turn-1', 'gated', body.message ?? null) } });
  });
  // The decision is recorded on its own; running the tool is a separate resume the UI must trigger.
  await page.route('**/sessions/*/confirm', route => route.fulfill({ json: { confirmed: true } }));
  await page.goto('/');
  await page.getByRole('button', { name: 'Pilot', exact: true }).click();
  await page.locator('.pilot-session').filter({ hasText: base.title }).click();
  await page.reload();
  await page.getByRole('button', { name: 'Pilot', exact: true }).click();
  const banner = page.getByRole('region', { name: '待确认操作' });
  await expect(banner).toContainText('demo');
  await expect(page.getByLabel('发送给 Anchor Pilot')).toBeDisabled();
  await banner.getByRole('button', { name: '确认并继续' }).click();
  await expect(page.locator('.pilot-message.assistant').last()).toContainText('已删除 demo。');
  await expect(page.getByRole('region', { name: '待确认操作' })).toHaveCount(0);
  expect(calls).toEqual(['resume']);
  await page.screenshot({ path: test.info().outputPath('pilot-approval.png'), fullPage: true });
});

test('a reply opens the Graph, the Run and the file it links, and returns to the same session', async ({ page }) => {
  test.setTimeout(60000);
  const run = '20260926T104613';
  const session = { id: 'linked', title: '对象跳转', status: 'active', updated_at: '2026-09-26T07:00:00Z',
                    approval: null, waiting_reason: '' };
  const definition = { entry: 'write', objective: '写一个文件', ops: { write: { run: 'true', writes: ['result.txt'] } },
                       nodes: [{ id: 'write', op: 'write' }], edges: [] };
  const reply = `运行 [${run}](#anchor/run/${run}) 已完成，产物在 [result.txt](#anchor/artifact/${run}/write/result.txt)，`
    + '图谱见 [demo](#anchor/graph/demo)。';
  await page.route('**/graphs', route => route.fulfill({ json: { graphs: [{ graph: 'demo', running: null }] } }));
  await page.route('**/graphs/demo', route => route.fulfill({ json: { definition } }));
  await page.route('**/runs', route => route.fulfill({ json: { runs: [{ run, graph: 'demo', status: 'finished',
    running: false, started: '2026-09-26T10:46:13Z', updated: '2026-09-26T10:47:00Z', executed: ['write'],
    objective: '写一个文件' }] } }));
  await page.route(`**/runs/${run}/files/write`, route => route.fulfill({ json: { files: [{ path: 'out/result.txt', size: 12, binary: false }] } }));
  await page.route(`**/runs/${run}/files/write/out/result.txt`, route => route.fulfill({ json: {
    path: 'out/result.txt', size: 12, binary: false, text: 'artifact-opened', truncated: false,
  } }));
  await page.route('**/runs/*', route => {
    if (route.request().url().includes('/files/write/out/result.txt')) return route.fulfill({ json: {
      path: 'out/result.txt', size: 12, binary: false, text: 'artifact-opened', truncated: false,
    } });
    if (route.request().url().endsWith('/files/write')) return route.fulfill({ json: {
      files: [{ path: 'out/result.txt', size: 12, binary: false }],
    } });
    return route.fulfill({ json: { graph: 'demo', run,
    state: { status: 'finished', objective: '写一个文件', cursor: null, reason: '', nodes: {},
             passes: {}, skipped: [], executed: ['write'], started: '2026-09-26T10:46:13Z',
             updated: '2026-09-26T10:47:00Z' },
    traces: {}, nodes: ['write'] } });
  });
  await page.route('**/sessions', route => route.fulfill({ json: { sessions: [session] } }));
  await page.route('**/sessions/*/messages', route => route.fulfill({
    json: { messages: [{ role: 'user', text: '看看结果' }, { role: 'assistant', text: reply }] } }));
  await page.route('**/sessions/*/turns', route => route.fulfill({ json: { turns: [] } }));
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto('/');
  await page.getByRole('button', { name: 'Pilot', exact: true }).click();
  await page.locator('.pilot-session').filter({ hasText: session.title }).click();
  await expect(page.getByRole('link', { name: run })).toBeVisible();
  await page.screenshot({ path: test.info().outputPath('pilot-references.png'), fullPage: true });

  // The Run link opens the run page the workbench already has, without a second object view.
  await page.getByRole('link', { name: run }).click();
  await expect(page.getByRole('button', { name: '返回会话' })).toBeVisible();
  await expect(page.locator('select[aria-label="当前工作流"]')).toHaveValue('demo');
  await expect(page.locator('.run-row.chosen')).toContainText(run);

  // The way back is the session it was opened from, not a new one and not the session list.
  await page.getByRole('button', { name: '返回会话' }).click();
  await expect(page.locator('.pilot-chat-heading strong')).toHaveAttribute('title', session.id);

  // The file link opens that Run at that node and expands the linked artifact.
  await page.getByRole('link', { name: 'result.txt' }).click();
  await expect(page.locator('aside.inspector h3')).toContainText('write');
  await expect(page.locator('.file-preview')).toContainText('artifact-opened');
  await page.getByRole('button', { name: '返回会话' }).click();
  await expect(page.locator('.pilot-chat-heading strong')).toHaveAttribute('title', session.id);

  // The Graph link opens the graph page, and every page keeps the way back.
  await page.getByRole('link', { name: 'demo' }).click();
  await expect(page.getByRole('button', { name: '图编排', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('select[aria-label="当前工作流"]')).toHaveValue('demo');
  await page.getByRole('button', { name: '返回会话' }).click();
  await expect(page.locator('.pilot-chat-heading strong')).toHaveAttribute('title', session.id);
  await expect(page.locator('article.pilot-message.assistant').last()).toContainText(run);
  expect(errors).toEqual([]);
  await page.screenshot({ path: test.info().outputPath('pilot-returned.png'), fullPage: true });
});
