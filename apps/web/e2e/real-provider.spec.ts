import { expect, test } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { freePort, gooseEnvironment, nativeHost, repo, stop } from './fixtures/native-host';

/** The real-provider, real-browser acceptance for continuing a Session after the process died.
 *
 * Opt in: `ANCHOR_REAL_PROVIDER=1 npx playwright test e2e/real-provider.spec.ts`.
 * It uses explicitly exported model settings and pinned Goose, starts a native Rust Host on an
 * isolated data root, and kills that process with SIGKILL while a reply is streaming.
 */

test('a killed server is restarted and the same Session continues in the browser', async ({ page, request }) => {
  test.skip(process.env.ANCHOR_REAL_PROVIDER !== '1', 'set ANCHOR_REAL_PROVIDER=1 to run this acceptance');
  test.setTimeout(300000);
  const root = await mkdtemp(join(tmpdir(), 'anchor-real-'));
  const apiPort = await freePort();
  const webPort = await freePort();
  const base = `http://127.0.0.1:${webPort}`;
  let server: ChildProcess | null = null;
  let frontend: ChildProcess | undefined;
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    const host = await nativeHost(root, { port: apiPort, environment: {
      ...await gooseEnvironment(),
      ANCHOR_MODEL_URL: process.env.ANCHOR_MODEL_URL,
      ANCHOR_MODEL_API_KEY: process.env.ANCHOR_MODEL_API_KEY,
      ANCHOR_MODEL_NAME: process.env.ANCHOR_MODEL_NAME,
      ANCHOR_MODEL_WIRE_API: process.env.ANCHOR_MODEL_WIRE_API,
      ANCHOR_MODEL_ALIASES: process.env.ANCHOR_MODEL_ALIASES,
    } });
    server = host.start();
    frontend = spawn(join(repo, 'apps/web/node_modules/.bin/vite'), ['--host', '127.0.0.1', '--port', String(webPort), '--strictPort'], {
      cwd: join(repo, 'apps/web'), stdio: 'ignore',
      env: { ...process.env, ANCHOR_WEB_API_URL: `http://127.0.0.1:${apiPort}` },
    });
    await expect(async () => {
      const response = await request.get(`${base}/sessions`);
      expect(response.ok()).toBeTruthy();
    }).toPass({ timeout: 30000 });
    // A Graph the reply can name, created through the product's own API.
    const seeded = await request.post(`${base}/graphs`, { data: { name: 'real-acceptance', definition: {
      entry: 'note', objective: 'real browser acceptance',
      ops: { note: { run: "printf 'browser-ok\\n' > note.txt", writes: ['note.txt'] } },
      nodes: [{ id: 'note', op: 'note' }], edges: [] } } });
    expect(seeded.ok()).toBeTruthy();

    await page.goto(base);
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    await page.getByRole('button', { name: '新建对话', exact: true }).click();
    const input = page.getByLabel('发送给 Anchor Pilot');
    await expect(input).toBeEnabled();
    // The reply is long on purpose: it leaves a window in which the process can be killed mid-output.
    await input.fill('请用大约 700 字详细说明 Anchor 的产品方向：Graph、Plugin、Run 和 Session 各自负责什么。'
      + '可以直接回答，不要调用工具。');
    await input.press('Enter');
    const partial = page.locator('.pilot-turn .pilot-message.assistant');
    await expect(partial).toContainText(/.{40,}/s, { timeout: 120000 });
    const sessionId = await page.locator('.pilot-chat-heading strong').getAttribute('title');
    const before = (await (await request.get(`${base}/sessions/${sessionId}/turns`)).json()).turns[0];
    expect(before.status).toBe('running');

    // Kill the real server mid-answer, as a crash or a machine restart would.
    server.kill('SIGKILL');
    await new Promise<void>(resolve => server!.once('exit', () => resolve()));
    server = null;

    // The browser survives the outage and says so, then the service comes back on the same root.
    await expect(page.getByText('服务未连接').first()).toBeVisible({ timeout: 30000 });
    server = host.start();
    await expect(async () => {
      const response = await request.get(`${base}/sessions`);
      expect(response.ok()).toBeTruthy();
    }).toPass({ timeout: 30000 });

    await page.reload();
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    await page.locator('.pilot-session').filter({ hasText: '说明 Anchor 的产品方向' }).first().click();
    // The old gate refused a new message here; a Session interrupted by a crash must accept one.
    await expect(input).toBeEnabled({ timeout: 30000 });
    await expect(page.getByText('回复已中断').first()).toBeVisible();
    const turns = (await (await request.get(`${base}/sessions/${sessionId}/turns`)).json()).turns;
    expect(turns[0].status).toBe('interrupted');
    await page.screenshot({ path: test.info().outputPath('real-provider-interrupted.png'), fullPage: true });

    await input.fill('上一个进程在输出中被终止了，你没有看到它的结尾。请继续：用一句话说明 Graph 负责什么，'
      + '并在句子里用 Markdown 链接引用 Graph real-acceptance（#anchor/graph/real-acceptance）。');
    await input.press('Enter');
    const answer = page.locator('article.pilot-message.assistant').last();
    await expect(answer).toContainText(/Graph/i, { timeout: 180000 });
    await expect(answer).toContainText('real-acceptance', { timeout: 180000 });
    // The reply's own reference opens the object page and comes back to this Session.
    await answer.getByRole('link', { name: 'real-acceptance' }).click();
    await expect(page.locator('select[aria-label="当前工作流"]')).toHaveValue('real-acceptance');
    await page.getByRole('button', { name: '返回会话' }).click();
    await expect(page.locator('.pilot-chat-heading strong')).toHaveAttribute('title', sessionId!);
    await expect(input).toBeEnabled();
    expect(errors).toEqual([]);
    await page.screenshot({ path: test.info().outputPath('real-provider-continued.png'), fullPage: true });
    console.log(`Real acceptance root: ${root}, session ${sessionId}`);
  } finally {
    await stop(frontend); await stop(server);
    if (process.env.ANCHOR_REAL_PROVIDER_KEEP !== '1') await rm(root, { recursive: true, force: true });
    else console.log(`Kept evidence at ${root}`);
  }
});
