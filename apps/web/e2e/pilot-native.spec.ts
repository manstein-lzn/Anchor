import { expect, test } from '@playwright/test';
import type { ChildProcess } from 'node:child_process';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gooseEnvironment, nativeHost, stop } from './fixtures/native-host';
import { loopbackProvider, toolResultField } from './fixtures/loopback-provider';

test('native Goose Pilot serves React chat, tool history and restart through the Rust Host', async ({ page, request }) => {
  test.skip(!process.env.ANCHOR_GOOSE_BINARY, 'pinned Goose binary required');
  test.setTimeout(90000);
  const root = await mkdtemp(join(tmpdir(), 'anchor-native-pilot-browser-'));
  const provider = await loopbackProvider((body, position) => {
    if (position === 1) return { tool: { name: 'graph_read', arguments: { graph: 'native-browser' } } };
    if (position === 2) {
      expect(toolResultField(body, 'objective')).toBe('native-browser-marker');
      return { text: 'native-browser-marker #anchor/graph/native-browser' };
    }
    if (position === 3) {
      expect(JSON.stringify(body.messages)).toContain('native-browser-marker');
      return { text: 'Remembered native-browser-marker' };
    }
    throw new Error('unexpected Provider request');
  });
  const key = 'native-browser-operator-key-32-bytes';
  const headers = { Authorization: `Bearer ${key}` };
  let backend: ChildProcess | undefined;
  const host = await nativeHost(root, { graphName: 'native-browser',
    definition: { objective: 'native-browser-marker', entry: 'idle', ops: { idle: { run: 'true' } },
      nodes: [{ id: 'idle', op: 'idle' }], edges: [] },
    environment: { ...await gooseEnvironment(provider.url), ANCHOR_API_KEYS: JSON.stringify([key]) },
  });
  const base = host.base;
  const ready = async () => expect.poll(async () => {
    try { return (await request.get(`${base}/health`, { headers })).status(); } catch { return 0; }
  }).toBe(200);
  try {
    backend = host.start(); await ready();
    expect(provider.calls).toHaveLength(0);
    await page.addInitScript(key => sessionStorage.setItem('anchor-api-key', key), key);
    await page.goto(base);
    await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    const input = page.getByLabel('发送给 Anchor Pilot');
    await input.fill('Read the native-browser graph without running it.'); await input.press('Enter');
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('native-browser-marker');
    await expect(input).toBeEnabled();
    const sessions = (await (await request.get(`${base}/sessions`, { headers })).json()).sessions;
    expect(sessions).toHaveLength(1);
    const session = sessions[0].id;
    const turns = (await (await request.get(`${base}/sessions/${session}/turns`, { headers })).json()).turns;
    const events = await request.get(`${base}/sessions/${session}/turns/${turns[0].id}/events`, { headers });
    expect(await events.text()).toContain('tool-output-available');
    await stop(backend); backend = host.start(); await ready();
    await page.reload();
    await expect(page.locator('.pilot-chat-heading strong')).toHaveAttribute('title', session);
    await expect(page.locator('.pilot-message.user')).toHaveCount(1);
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('native-browser-marker');
    expect(provider.calls).toHaveLength(2);
    await input.fill('Recall the previous result without using tools.'); await input.press('Enter');
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('Remembered native-browser-marker');
    await expect(input).toBeEnabled();
    expect(provider.calls).toHaveLength(3); expect(provider.failures).toEqual([]);
    await page.screenshot({ path: join(root, 'pilot-native.png') });
    await writeFile(join(root, 'evidence.json'), JSON.stringify({ status: 'passed', provider: 'deterministic loopback',
      real_model_calls: 0, actual_host: true, actual_goose: true, session, restart: true, provider_requests: provider.calls.length }));
    console.log(`Native Pilot browser evidence: ${root}`);
  } finally {
    await stop(backend); await provider.close();
    await writeFile(join(root, 'host.log'), host.logs);
  }
});
