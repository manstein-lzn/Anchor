import { expect, test } from '@playwright/test';
import type { ChildProcess } from 'node:child_process';
import { mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { gooseEnvironment, nativeHost, stop } from './fixtures/native-host';
import { loopbackProvider, toolResultField } from './fixtures/loopback-provider';

test('Goose Pilot controls one Rust Run and reads its artifact after the Host restarts', async ({ page, request }) => {
  test.skip(!process.env.ANCHOR_GOOSE_BINARY, 'pinned Goose binary required');
  test.setTimeout(120000);
  const root = await mkdtemp(join(tmpdir(), 'anchor-pilot-rust-browser-'));
  let acceptedRun = '';
  const provider = await loopbackProvider((body, position) => {
    if (position === 1) return { tool: { name: 'graph_list', arguments: {} } };
    if (position === 2) return { tool: { name: 'graph_read', arguments: { graph: 'work' } } };
    if (position === 3) {
      const definition = toolResultField(body, 'definition');
      expect(definition).toBeTruthy();
      return { tool: { name: 'graph_validate', arguments: { definition } } };
    }
    if (position === 4) {
      expect(toolResultField(body, 'valid')).toBe(true);
      return { tool: { name: 'graph_run', arguments: { graph: 'work' } } };
    }
    if (position === 5) {
      const observed = toolResultField(body, 'run');
      expect(typeof observed).toBe('string'); acceptedRun = observed as string;
      return { tool: { name: 'run_pause', arguments: { run: acceptedRun } } };
    }
    if (position === 6) return { text: `已发起并请求暂停 [本次运行](#anchor/run/${acceptedRun})。` };
    if (position === 7 || position === 10) return { tool: { name: 'session_wait', arguments: {} } };
    if (position === 8) return { tool: { name: 'run_resume', arguments: { run: acceptedRun } } };
    if (position === 9) return { text: `已继续 [本次运行](#anchor/run/${acceptedRun})。` };
    if (position === 11) return { tool: { name: 'artifact_read', arguments: { run: acceptedRun, node: 'last', path: 'result.txt' } } };
    if (position === 12) {
      expect(toolResultField(body, 'text')).toBe('pilot-rust-marker');
      return { text: `重开后已核查原产物：[结果文件](#anchor/artifact/${acceptedRun}/last/result.txt)。` };
    }
    throw new Error('unexpected Provider request');
  });
  const graph = { objective: 'Native Pilot Run control acceptance', entry: 'first',
    ops: { first: { run: 'printf pilot-rust-marker > first.txt; while [ ! -f release ]; do sleep 0.1; done' },
      last: { run: 'cat /in/first/first.txt > result.txt' } },
    nodes: [{ id: 'first', op: 'first' }, { id: 'last', op: 'last' }], edges: [{ from: 'first', to: 'last' }] };
  const host = await nativeHost(root, { graphName: 'work', definition: graph, environment: await gooseEnvironment(provider.url) });
  const base = host.base;
  let backend: ChildProcess | undefined;
  const ready = async () => expect(async () => {
    expect((await request.get(`${base}/graphs`)).ok(), host.logs).toBeTruthy();
  }).toPass({ timeout: 20000 });
  const send = async (message: string) => {
    const input = page.getByLabel('发送给 Anchor Pilot');
    await expect(input).toBeEnabled(); await input.fill(message); await input.press('Enter');
  };
  try {
    backend = host.start(); await ready(); expect(provider.calls).toHaveLength(0);
    await page.goto(base); await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    await send('启动并暂停');
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('已发起并请求暂停', { timeout: 45000 });
    const session = (await (await request.get(`${base}/sessions`)).json()).sessions[0].id;
    const runId = (await (await request.get(`${base}/runs`)).json()).runs[0].run;
    expect(runId).toBe(acceptedRun);
    const runFile = join(root, 'state/runs', `${runId}.json`);
    await expect.poll(async () => (await request.get(`${base}/runs/${runId}`).then(response => response.json())).control_requested).toBe('pause');
    let gate = '';
    await expect(async () => {
      const first = (await readdir(join(root, 'workspaces'), { recursive: true })).find(path => path.endsWith('/first.txt'));
      expect(first).toBeTruthy(); gate = join(dirname(join(root, 'workspaces', first!)), 'release');
    }).toPass();
    await writeFile(gate, '');
    await expect.poll(async () => JSON.parse(await readFile(runFile, 'utf8')).status).toBe('paused');
    const paused = JSON.parse(await readFile(runFile, 'utf8'));
    expect(paused.results.first).toHaveLength(1); expect(paused.results.last).toBeUndefined();
    await page.getByRole('link', { name: '本次运行', exact: true }).click();
    await expect(page.locator('.canvas-head .pill')).toHaveText('已暂停');
    await page.screenshot({ path: join(root, 'paused-run.png') });
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    await send('继续运行');
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('已继续', { timeout: 30000 });
    await expect.poll(async () => JSON.parse(await readFile(runFile, 'utf8')).status).toBe('completed');
    expect(JSON.parse(await readFile(runFile, 'utf8')).results.first).toEqual(paused.results.first);
    await expect(page.getByLabel('发送给 Anchor Pilot')).toBeEnabled();
    expect(provider.calls).toHaveLength(9);
    await stop(backend); backend = host.start(); await ready(); await page.reload();
    expect(provider.calls).toHaveLength(9);
    await expect(page.locator('.pilot-chat-heading strong')).toHaveAttribute('title', session);
    await expect(page.locator('.pilot-message.user')).toHaveCount(2);
    await send('重开后读取产物');
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('重开后已核查原产物', { timeout: 30000 });
    await expect(page.getByLabel('发送给 Anchor Pilot')).toBeEnabled();
    await page.getByRole('link', { name: '结果文件', exact: true }).click();
    await expect(page.locator('.file-preview')).toHaveText('pilot-rust-marker');
    await expect(page.locator('.canvas-head .pill')).toHaveText('已完成');
    await page.screenshot({ path: join(root, 'artifact.png') });
    const runs = (await (await request.get(`${base}/runs`)).json()).runs;
    expect(runs).toHaveLength(1); expect(runs[0].run).toBe(runId);
    const details = (await (await request.get(`${base}/sessions/${session}`)).json()).session;
    expect(details.run_ids).toEqual([runId]);
    expect(provider.calls).toHaveLength(12); expect(provider.failures).toEqual([]);
    await writeFile(join(root, 'evidence.json'), JSON.stringify({ status: 'passed', session, run: runId,
      real_model_calls: 0, actual_host: true, actual_goose: true, provider_requests: provider.calls.length,
      scope: 'Native Graph validation, Run admission/pause/resume, UI links, Host restart and artifact read' }, null, 2));
  } finally {
    await stop(backend); await provider.close();
    await writeFile(join(root, 'host.log'), host.logs);
    console.log(`Native Pilot Run-control browser fixture: ${root}`);
  }
});
