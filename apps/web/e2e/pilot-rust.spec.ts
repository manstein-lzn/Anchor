import { expect, test } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { createServer } from 'node:net';
import { mkdtemp, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

async function freePort() {
  const server = createServer();
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('missing port');
  await new Promise<void>(resolve => server.close(() => resolve()));
  return address.port;
}

async function stop(child?: ChildProcess) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise<void>(resolve => child.once('exit', () => resolve()));
  child.kill('SIGINT');
  await exited;
}

test('Pilot controls the same Rust Run and reads its artifact after both hosts restart', async ({ page, request }) => {
  test.setTimeout(120000);
  const repo = fileURLToPath(new URL('../../../', import.meta.url));
  const root = await mkdtemp(join(repo, '.local/pilot-rust-browser-'));
  const platform = join(root, 'platform'), catalog = join(root, 'catalog'), bundle = join(catalog, 'work');
  const workspaces = join(root, 'workspaces');
  await mkdir(bundle, { recursive: true });
  const config = join(root, 'runtime.json');
  await writeFile(config, '{}');
  const graph = { objective: 'Pilot and Rust acceptance', entry: 'first',
    ops: { first: { run: 'printf pilot-rust-marker > first.txt; while [ ! -f release ]; do sleep 0.1; done' },
      last: { run: 'cat /in/first/first.txt > result.txt' } },
    nodes: [{ id: 'first', op: 'first' }, { id: 'last', op: 'last' }], edges: [{ from: 'first', to: 'last' }] };
  await writeFile(join(bundle, 'graph.json'), JSON.stringify(graph));
  await writeFile(join(bundle, 'manifest.json'), '{"format":1,"graph":"graph.json","plugins":[]}');
  const rustPort = await freePort(), platformPort = await freePort();
  const upstream = `http://127.0.0.1:${rustPort}`, base = `http://127.0.0.1:${platformPort}`;
  let rust: ChildProcess | undefined, web: ChildProcess | undefined, logs = '';
  const launch = (binary: string, args: string[], env: NodeJS.ProcessEnv) => {
    const child = spawn(binary, args, { cwd: repo, stdio: ['ignore', 'pipe', 'pipe'], env });
    child.stdout?.on('data', data => { logs += data.toString(); });
    child.stderr?.on('data', data => { logs += data.toString(); });
    return child;
  };
  const startRust = () => launch(join(repo, 'rust/target/release/anchor-runner-host'), ['serve'], {
    PATH: '/usr/bin:/bin', ANCHOR_RUNNER_BUNDLE_ROOT: bundle,
    ANCHOR_RUNNER_CATALOG_ROOT: catalog, ANCHOR_RUNNER_GRAPH_NAME: 'work',
    ANCHOR_RUNNER_STATE_ROOT: join(root, 'state'), ANCHOR_RUNNER_WORKSPACE_ROOT: workspaces,
    ANCHOR_RUNNER_LISTEN: `127.0.0.1:${rustPort}`, ANCHOR_RUNNER_ALLOWED_COMMANDS: 'sh',
  });
  const startPlatform = () => launch(join(repo, '.venv/bin/python'), [
    join(repo, 'apps/web/e2e/fixtures/rust_pilot_server.py'), '--root', platform,
    '--config', config, '--port', String(platformPort)], {
    PATH: '/usr/bin:/bin', PYTHONPATH: join(repo, 'src'),
    ANCHOR_MODEL_URL: '', ANCHOR_MODEL_API_KEY: '', ANCHOR_MODEL_NAME: '',
    ANCHOR_API_KEYS: '', ANCHOR_API_KEY: '',
    ANCHOR_RUNTIME_BACKEND: 'rust', ANCHOR_RUNTIME_URL: upstream,
  });
  const ready = async () => expect(async () => {
    expect((await request.get(`${base}/graphs`)).ok(), logs).toBeTruthy();
  }).toPass({ timeout: 20000 });
  const send = async (message: string) => {
    const input = page.getByLabel('发送给 Anchor Pilot');
    await expect(input).toBeEnabled(); await input.fill(message); await input.press('Enter');
  };
  try {
    rust = startRust(); web = startPlatform(); await ready();
    await page.goto(base); await page.getByRole('button', { name: 'Pilot', exact: true }).click();
    await send('启动并暂停');
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('已发起并请求暂停', { timeout: 45000 });
    const session = (await (await request.get(`${base}/sessions`)).json()).sessions[0].id;
    const runId = (await (await request.get(`${base}/runs`)).json()).runs[0].run;
    const runFile = join(root, 'state/runs', `${runId}.json`);
    await expect.poll(async () => (await request.get(`${base}/runs/${runId}`).then(r => r.json())).control_requested).toBe('pause');
    let gate = '';
    await expect(async () => {
      const first = (await readdir(workspaces, { recursive: true })).find(path => path.endsWith('/first.txt'));
      expect(first).toBeTruthy(); gate = join(dirname(join(workspaces, first!)), 'release');
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
    await stop(web); await stop(rust);
    rust = startRust(); web = startPlatform(); await ready(); await page.reload();
    await expect(page.locator('.pilot-chat-heading strong')).toHaveAttribute('title', session);
    await expect(page.locator('.pilot-message.user')).toHaveCount(2);
    await send('重开后读取产物');
    await expect(page.locator('.pilot-message.assistant').last()).toContainText('重开后已核查原产物', { timeout: 30000 });
    await page.screenshot({ path: join(root, 'reopened-pilot.png') });
    await page.getByRole('link', { name: '结果文件', exact: true }).click();
    await expect(page.locator('.file-preview')).toHaveText('pilot-rust-marker');
    await expect(page.locator('.canvas-head .pill')).toHaveText('已完成');
    await page.screenshot({ path: join(root, 'artifact.png') });
    const runs = (await (await request.get(`${base}/runs`)).json()).runs;
    expect(runs).toHaveLength(1); expect(runs[0].run).toBe(runId);
    const details = (await (await request.get(`${base}/sessions/${session}`)).json()).session;
    expect(details.run_ids).toEqual([runId]);
    const calls = (await readFile(join(platform, 'fixture-model-calls.jsonl'), 'utf8')).trim().split('\n').map(line => JSON.parse(line));
    expect(calls.filter(call => call.tool === 'graph_run')).toHaveLength(1);
    expect(calls.map(call => call.tool)).toEqual(['graph_list', 'graph_read', 'graph_validate', 'graph_run', 'run_pause',
      'session_wait', 'run_resume', 'session_wait', 'artifact_read']);
    expect(await readdir(join(platform, 'workspaces')).catch(() => [])).toEqual([]);
    await writeFile(join(root, 'evidence.json'), JSON.stringify({ status: 'passed', session, run: runId,
      model: 'controlled FunctionModel; existing Pilot/Harness and both HTTP hosts are real',
      scope: 'Public Graph validation, Run admission/pause/resume, UI links, both-host restart and artifact read; no Python Graph Run copies' }, null, 2));
  } finally {
    await stop(web); await stop(rust);
    await writeFile(join(root, 'services.log'), logs);
    console.log(`Pilot Rust browser acceptance: ${root}`);
  }
});
