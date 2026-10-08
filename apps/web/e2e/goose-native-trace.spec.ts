import { expect, test } from '@playwright/test';
import type { ChildProcess } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { OurGraph } from '../src/model';
import { fixturePlugin, gooseEnvironment, gooseSha256, nativeHost, nativeHostBinary, stop } from './fixtures/native-host';
import { loopbackProvider, toolResultField } from './fixtures/loopback-provider';

test.use({ launchOptions: { executablePath: process.env.ANCHOR_BROWSER_BINARY } });

test('real Rust Host and Goose retain active trace, canonical completion and frozen Plugin history after refresh', async ({ page, request }) => {
  test.skip(!process.env.ANCHOR_GOOSE_BINARY, 'pinned Goose binary required');
  test.setTimeout(90000);
  const root = await mkdtemp(join(tmpdir(), 'anchor-goose-trace-browser-'));
  const graph: OurGraph = { objective: 'Isolated native trace browser fixture', entry: 'worker',
    agents: { worker: { model: 'models.default', instructions: 'Read the Plugin Skill and workspace, then final_result with route verify.' } },
    ops: { verify: { run: "sh -c 'cat /in/worker/evidence.txt > verified.txt'" } },
    nodes: [{ id: 'worker', agent: 'worker' }, { id: 'verify', op: 'verify' }], edges: [{ from: 'worker', to: 'verify' }] };
  const pageErrors: string[] = [];
  page.on('pageerror', error => pageErrors.push(error.message));
  let release = () => {};
  const gate = new Promise<void>(resolve => { release = resolve; });
  const provider = await loopbackProvider(async (body, position) => {
    if (position === 1) return { text: 'Native browser draft, not a completed reply.', tool: { name: 'anchor_run',
      arguments: { command: ['sh', '-c', 'set -eu; test ! -e evidence.txt; cat /plugins/browser-plugin/skills/evidence/SKILL.md > notes.md; cat /plugins/browser-plugin/resources/input.txt > plugin-resource.txt; printf once > evidence.txt; cat evidence.txt'] } } };
    if (position === 2) {
      const observed = toolResultField(body, 'anchor_receipt');
      expect(typeof observed).toBe('string'); await gate;
      return { tool: { name: 'final_result', arguments: { summary: 'Canonical native reply', route: 'verify', observed_receipt: observed } } };
    }
    if (position === 3) return { text: 'Native node result was submitted.' };
    throw new Error('unexpected local Provider request');
  });
  const host = await nativeHost(root, { graphName: 'native-trace', definition: graph, environment: await gooseEnvironment(provider.url) });
  await fixturePlugin(host.environment.ANCHOR_RUNNER_LIBRARY_ROOT!);
  const base = host.base;
  let backend: ChildProcess | undefined;
  try {
    backend = host.start();
    await expect(async () => expect((await request.get(`${base}/health`)).ok(), host.logs).toBeTruthy()).toPass({ timeout: 20000 });
    expect(provider.calls).toHaveLength(0);
    graph.nodes[0].plugins = ['browser-plugin'];
    const mounted = await request.put(`${base}/graphs/native-trace`, { data: { definition: graph } });
    expect(mounted.ok(), await mounted.text()).toBeTruthy();
    const response = await request.post(`${base}/trigger`, { data: { graph: 'native-trace' } });
    expect(response.status(), await response.text()).toBe(202);
    const run = (await response.json()).run;
    await expect.poll(() => provider.calls.length).toBe(2);
    const during = await (await request.get(`${base}/runs/${run}`)).json();
    expect(during.state.status).toBe('running');
    expect(during.state.nodes.worker).toBeUndefined();
    await page.goto(base);
    await page.getByRole('button', { name: '查看当前运行', exact: true }).click();
    await page.locator('.execution-node').filter({ has: page.locator('strong', { hasText: /^worker$/ }) }).click();
    await expect(page.locator('.inspector .said')).toContainText('Native browser draft');
    const call = page.locator('.inspector details.call').filter({ hasText: 'evidence.txt' }).first();
    await call.locator('summary').click();
    await expect(call).toContainText('once');
    await page.screenshot({ path: join(root, 'live-desktop.png') });
    release();
    await expect.poll(async () => (await (await request.get(`${base}/runs/${run}`)).json()).state.status).toBe('completed');
    await expect(page.locator('.inspector .node-summary')).toContainText('Canonical native reply');
    const saved = JSON.parse(await readFile(join(root, 'state/runs', `${run}.json`), 'utf8'));
    const binding = saved.plugin_bindings['browser-plugin'];
    expect(binding.id).toBe('browser-plugin');
    const detail = await (await request.get(`${base}/runs/${run}`)).json();
    expect(detail.plugins.worker[0]).toMatchObject({ id: binding.id, digest: binding.digest });
    for (const [node, file] of [['worker', 'evidence.txt'], ['verify', 'verified.txt']]) {
      const artifact = await readFile(join(root, 'state/artifacts', saved.results[node][0].commit.id, 'files', file), 'utf8');
      expect(artifact).toBe('once');
    }
    const pluginRead = await (await request.get(`${base}/runs/${run}/files/worker/plugin-resource.txt`)).json();
    expect(pluginRead.text).toBe('Native Plugin evidence');
    await writeFile(join(root, 'library/plugins/browser-plugin/resources/input.txt'), 'Changed after Run');
    await page.reload();
    await page.getByRole('button', { name: /^native-trace，已完成，/ }).click();
    await page.getByRole('dialog').getByRole('button', { name: '查看节点与产物' }).click();
    await page.locator('.execution-node').filter({ has: page.locator('strong', { hasText: /^worker$/ }) }).click();
    await expect(page.locator('.inspector .said').first()).toContainText('Native browser draft');
    await expect(page.locator('.inspector .node-summary')).toContainText('Canonical native reply');
    await page.getByRole('button', { name: 'Plugin', exact: true }).click();
    await expect(page.locator('.plugins-panel')).toContainText('browser-plugin');
    await expect(page.locator('.plugins-panel')).toContainText(binding.digest.slice(0, 12));
    expect((await (await request.get(`${base}/runs/${run}`)).json()).plugins.worker[0].digest).toBe(binding.digest);
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.locator('.inspector').evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
    await page.screenshot({ path: join(root, 'settled-mobile.png'), fullPage: true });
    expect(pageErrors).toEqual([]); expect(provider.failures).toEqual([]); expect(provider.calls).toHaveLength(3);
    await writeFile(join(root, 'evidence.json'), JSON.stringify({ status: 'passed', provider: 'deterministic loopback',
      real_model_calls: 0, actual_host: true, actual_goose: true, provider_requests: provider.calls.length,
      host_sha256: createHash('sha256').update(await readFile(nativeHostBinary())).digest('hex'),
      goose_sha256: gooseSha256, run, during, frozen_plugin: binding }));
    console.log(`Native trace browser evidence: ${root}`);
  } finally {
    release(); await stop(backend); await provider.close();
    await writeFile(join(root, 'host.log'), host.logs);
  }
});
