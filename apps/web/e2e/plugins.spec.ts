import { expect, test } from '@playwright/test';
import type { ChildProcess } from 'node:child_process';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fixturePlugin, nativeHost, stop } from './fixtures/native-host';

test('native Rust Library preserves Plugin selection, lazy resources and Op artifacts in the browser', async ({ page, request }) => {
  test.setTimeout(60000);
  const root = await mkdtemp(join(tmpdir(), 'anchor-plugin-browser-'));
  const host = await nativeHost(root);
  await fixturePlugin(host.environment.ANCHOR_RUNNER_LIBRARY_ROOT!);
  const base = host.base;
  let backend: ChildProcess | undefined;
  try {
    backend = host.start();
    await expect(async () => expect((await request.get(`${base}/plugins`)).ok(), host.logs).toBeTruthy())
      .toPass({ timeout: 20000 });
    const response = await request.post(`${base}/graphs`, { data: { name: 'plugin-proof', definition: {
      entry: 'research', agents: { worker: { model: 'models.default', instructions: 'Use Plugin resources.' } },
      nodes: [{ id: 'research', agent: 'worker' }], edges: [],
    } } });
    expect(response.ok(), await response.text()).toBeTruthy();
    await page.goto(base);
    await page.getByLabel('当前工作流').selectOption('plugin-proof');
    await page.locator('[data-id="research"]').click();
    const checkbox = page.getByRole('checkbox', { name: '浏览器证据' });
    await checkbox.check();
    await page.getByRole('button', { name: '查看说明', exact: true }).click();
    await expect(page.locator('.plugin-instructions')).toContainText('/plugins/browser-plugin/resources/input.txt');
    const skill = await request.get(`${base}/plugins/browser-plugin/files/skills/evidence/SKILL.md`);
    expect(skill.ok()).toBeTruthy();
    expect(skill.headers()['x-content-type-options']).toBe('nosniff');
    const resource = await request.get(`${base}/plugins/browser-plugin/files/resources/input.txt`);
    expect(await resource.text()).toBe('Native Plugin evidence');
    await page.getByRole('button', { name: '保存', exact: true }).click();
    await expect(async () => {
      const graph = await (await request.get(`${base}/graphs/plugin-proof`)).json();
      expect(graph.definition.nodes[0].plugins).toEqual(['browser-plugin']);
    }).toPass();
    await expect(page.getByLabel('挂载 1 个 Plugin')).toBeVisible();
    const frozen = await readFile(join(root, 'catalog/plugin-proof/plugins/browser-plugin/plugin.json'), 'utf8');
    expect(frozen).toBe(await readFile(join(root, 'library/plugins/browser-plugin/plugin.json'), 'utf8'));
    await page.screenshot({ path: test.info().outputPath('plugin-editor.png'), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(checkbox).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy();
    await page.setViewportSize({ width: 1440, height: 1000 });

    const op = await request.post(`${base}/graphs`, { data: { name: 'op-proof', definition: {
      entry: 'build', ops: { build: { run: "printf 'native-plugin-management' > result.txt" } },
      nodes: [{ id: 'build', op: 'build' }], edges: [],
    } } });
    expect(op.ok(), await op.text()).toBeTruthy();
    await page.reload();
    await page.getByLabel('当前工作流').selectOption('op-proof');
    page.on('dialog', dialog => dialog.accept('{}'));
    await page.getByRole('button', { name: '运行工作流', exact: true }).click();
    await expect(page.locator('.execution-node.state-completed')).toHaveCount(1, { timeout: 20000 });
    await page.locator('[data-id="build"]').click();
    await page.getByRole('button', { name: '文件', exact: true }).click();
    await page.getByRole('button', { name: /result\.txt/ }).click();
    await expect(page.locator('.file-preview')).toHaveText('native-plugin-management');
    const runs = (await (await request.get(`${base}/runs`)).json()).runs;
    expect(runs).toHaveLength(1);
    expect(runs[0].status).toBe('completed');
    const artifact = await (await request.get(`${base}/runs/${runs[0].run}/files/build/result.txt`)).json();
    expect(artifact.text).toBe('native-plugin-management');
    await writeFile(test.info().outputPath('plugin-evidence.json'), JSON.stringify({ status: 'passed',
      real_model_calls: 0, native_library: true, frozen_plugin: true, run: runs[0].run, artifact }, null, 2));
  } finally {
    await stop(backend);
    await writeFile(join(root, 'host.log'), host.logs);
    console.log(`Native Plugin browser fixture: ${root}`);
  }
});
