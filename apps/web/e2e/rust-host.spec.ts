import { expect, test } from '@playwright/test';
import type { ChildProcess } from 'node:child_process';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { nativeHost, stop } from './fixtures/native-host';

test('Rust-hosted React workbench runs a protected Graph and inspects its committed file', async ({ page, request }) => {
  test.setTimeout(60000);
  const root = await mkdtemp(join(tmpdir(), 'anchor-rust-web-'));
  const graph = {
    objective: 'Rust host browser slice', entry: 'build',
    ops: { build: { run: "sh -c 'printf rust-native > result.txt'" } },
    nodes: [{ id: 'build', op: 'build', plugins: [] }], edges: [],
  };
  const key = 'rust-browser-test-operator-key-32-bytes';
  const host = await nativeHost(root, { graphName: 'rust-ui', definition: graph,
    environment: { ANCHOR_API_KEYS: JSON.stringify([key]) },
  });
  const api = host.base;
  let backend: ChildProcess | undefined;
  try {
    backend = host.start();
    await expect(async () => {
      const response = await request.get(`${api}/`);
      expect(response.ok(), `${response.status()} ${await response.text()}\n${host.logs}`).toBeTruthy();
    }).toPass({ timeout: 20000 });
    const unauthorized = await request.get(`${api}/graphs`);
    expect(unauthorized.status()).toBe(401);
    await page.goto(api);
    await expect(page.getByRole('dialog', { name: '连接 Anchor 服务' })).toBeVisible();
    await page.getByLabel('Anchor API key').fill(key);
    await page.getByRole('button', { name: '连接', exact: true }).click();
    await expect(page.locator('[data-id="build"]')).toBeVisible();
    page.on('dialog', dialog => dialog.accept('{}'));
    await page.getByRole('button', { name: '运行工作流', exact: true }).click();
    await expect(page.locator('.execution-node.state-completed')).toHaveCount(1, { timeout: 20000 });
    await page.locator('[data-id="build"]').click();
    await page.getByRole('button', { name: '文件' }).click();
    await page.getByRole('button', { name: /result\.txt/ }).click();
    await expect(page.locator('.file-preview')).toHaveText('rust-native');
    const headers = { Authorization: `Bearer ${key}` };
    const run = await (await request.get(`${api}/runs`, { headers })).json();
    expect(run.runs).toHaveLength(1);
    const runId = run.runs[0].run as string;
    const detail = await (await request.get(`${api}/runs/${runId}`, { headers })).json();
    expect(detail.state.nodes.build.files).toContain('result.txt');
    const artifact = await (await request.get(`${api}/runs/${runId}/files/build/result.txt`, { headers })).json();
    expect(artifact.text).toBe('rust-native');
    await page.getByRole('button', { name: '运行看板' }).click();
    await page.getByRole('button', { name: /rust-ui，已完成/ }).click();
    const preview = page.getByRole('dialog', { name: '运行详情' });
    await expect(preview).toContainText(runId);
    await expect(preview).toContainText('运行时长');
    await expect(preview).toContainText('结束时间');
    await expect(preview).not.toContainText('暂无记录');
    await expect(preview).not.toContainText('Invalid Date');
    await expect(page.getByText('服务在线', { exact: true })).toBeVisible();
  } finally {
    await writeFile(join(root, 'server.log'), host.logs);
    await stop(backend);
    console.log(`Rust web acceptance fixture: ${root}`);
  }
});
