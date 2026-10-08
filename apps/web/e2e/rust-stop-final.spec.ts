import { expect, test } from '@playwright/test';
import type { ChildProcess } from 'node:child_process';
import { mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gooseEnvironment, nativeHost, stop } from './fixtures/native-host';
import { loopbackProvider, toolResultField } from './fixtures/loopback-provider';

test('stop during the final Goose reply settles and continuation checks existing effects before completing', async ({ page, request }) => {
  test.skip(!process.env.ANCHOR_GOOSE_BINARY, 'pinned Goose binary required');
  test.setTimeout(90000);
  const root = await mkdtemp(join(tmpdir(), 'anchor-rust-stop-final-'));
  let release = () => {};
  const gate = new Promise<void>(resolve => { release = resolve; });
  const provider = await loopbackProvider(async (body, position) => {
    if (position === 1) return { tool: { name: 'anchor_run', arguments: {
      command: ['sh', '-c', 'set -eu; test ! -e effects.txt; printf once > effects.txt; cat effects.txt'],
    } } };
    if (position === 2 || position === 5) {
      const receipt = toolResultField(body, 'anchor_receipt');
      expect(typeof receipt).toBe('string');
      return { tool: { name: 'final_result', arguments: { summary: 'Checked the existing effect.', route: 'next', observed_receipt: receipt } } };
    }
    if (position === 3) { await gate; return { text: 'Final Goose reply.' }; }
    if (position === 4) return { tool: { name: 'anchor_run', arguments: { command: ['cat', 'effects.txt'] } } };
    if (position === 6) return { text: 'Resumed after checking the existing effect.' };
    throw new Error('unexpected Provider request');
  });
  const graph = { objective: 'Stop while final Goose text is pending', entry: 'answer',
    agents: { worker: { model: 'models.default', instructions: 'Inspect effects and complete with route next.' } },
    ops: { next: { run: 'printf downstream > marker.txt' } },
    nodes: [{ id: 'answer', agent: 'worker' }, { id: 'next', op: 'next' }], edges: [{ from: 'answer', to: 'next' }] };
  const host = await nativeHost(root, { graphName: 'stop-final', definition: graph, environment: await gooseEnvironment(provider.url) });
  const base = host.base;
  let backend: ChildProcess | undefined;
  try {
    backend = host.start();
    await expect(async () => expect((await request.get(`${base}/health`)).ok(), host.logs).toBeTruthy()).toPass({ timeout: 20000 });
    await page.goto(base);
    await expect(page.locator('[data-id="answer"]')).toBeVisible();
    page.on('dialog', dialog => dialog.accept('{}'));
    await page.getByRole('button', { name: '运行工作流', exact: true }).click();
    await expect.poll(() => provider.calls.length, { timeout: 30000 }).toBe(3);
    const stopping = page.waitForResponse(response => response.url().endsWith('/stop') && response.request().method() === 'POST');
    await page.getByRole('button', { name: '停止', exact: true }).click();
    expect((await stopping).status()).toBe(202);
    release();
    const runId = (await (await request.get(`${base}/runs`)).json()).runs[0].run;
    const runPath = join(root, 'state/runs', `${runId}.json`);
    await expect.poll(async () => JSON.parse(await readFile(runPath, 'utf8')).status).toBe('stopped');
    const stopped = JSON.parse(await readFile(runPath, 'utf8'));
    expect(stopped.results.next).toBeUndefined();
    expect(stopped.results.answer).toBeUndefined();
    const workspaces = host.environment.ANCHOR_RUNNER_WORKSPACE_ROOT!;
    const effects = (await readdir(workspaces, { recursive: true })).filter(path => path.endsWith('/effects.txt'));
    expect(effects).toHaveLength(1);
    expect(await readFile(join(workspaces, effects[0]), 'utf8')).toBe('once');
    await expect(page.locator('.canvas-head .pill')).toHaveText('已停止');
    await expect(page.getByRole('button', { name: '继续', exact: true })).toBeVisible();
    await page.screenshot({ path: join(root, 'stopped.png') });
    await page.getByRole('button', { name: '继续', exact: true }).click();
    await expect.poll(async () => JSON.parse(await readFile(runPath, 'utf8')).status, { timeout: 30000 }).toBe('completed');
    const completed = await readFile(runPath, 'utf8'), record = JSON.parse(completed);
    expect(record.results.answer).toHaveLength(1);
    expect(record.results.next).toHaveLength(1);
    expect(provider.calls).toHaveLength(6); expect(provider.failures).toEqual([]);
    expect(await readFile(join(workspaces, effects[0]), 'utf8')).toBe('once');
    expect(await readFile(join(root, 'state/artifacts', record.results.answer[0].commit.id, 'files/effects.txt'), 'utf8')).toBe('once');
    expect(await readFile(join(root, 'state/artifacts', record.results.next[0].commit.id, 'files/marker.txt'), 'utf8')).toBe('downstream');
    await expect.poll(async () => (await (await request.get(`${base}/runs/${runId}`)).json()).active).toBe(false);
    expect((await request.post(`${base}/runs/${runId}/stop`, { data: {} })).status()).toBe(409);
    expect(await readFile(runPath, 'utf8')).toBe(completed);
    await page.screenshot({ path: join(root, 'completed.png') });
    await writeFile(join(root, 'evidence.json'), JSON.stringify({ status: 'passed', run: runId,
      provider: 'deterministic loopback', real_model_calls: 0, actual_goose: true, provider_requests: provider.calls.length,
      scope: 'Final reply cancellation, visible stop, no downstream before continuation, read before completing and no repeated effect' }, null, 2));
  } finally {
    release(); await stop(backend); await provider.close();
    await writeFile(join(root, 'host.log'), host.logs);
    console.log(`Native stop browser fixture: ${root}`);
  }
});
