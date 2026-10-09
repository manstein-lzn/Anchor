import { spawn, type ChildProcess } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync } from 'node:fs';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { OurGraph } from '../../src/model';

export const repo = fileURLToPath(new URL('../../../../', import.meta.url));
export const gooseSha256 = '71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7';

export function nativeHostBinary() {
  const configured = process.env.ANCHOR_TEST_GOOSE_HOST_BINARY || process.env.ANCHOR_TEST_NATIVE_PILOT_BINARY
    || process.env.ANCHOR_TEST_RUNTIME_BINARY || process.env.ANCHOR_RUNNER_BINARY;
  if (configured) return configured;
  const target = process.env.CARGO_TARGET_DIR || join(repo, 'rust/target');
  const release = join(target, 'release/anchor-runner-host');
  return existsSync(release) ? release : join(target, 'debug/anchor-runner-host');
}

export async function freePort() {
  const server = createServer();
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('missing test port');
  await new Promise<void>(resolve => server.close(() => resolve()));
  return address.port;
}

export async function stop(child?: ChildProcess | null) {
  if (!child || child.exitCode !== null || child.signalCode !== null || !child.pid) return;
  await new Promise<void>(resolve => {
    const timer = setTimeout(() => child.kill('SIGKILL'), 5000);
    child.once('exit', () => { clearTimeout(timer); resolve(); });
    child.kill('SIGTERM');
  });
}

export async function nativeHost(root: string, options: {
  graphName?: string;
  definition?: OurGraph;
  port?: number;
  environment?: NodeJS.ProcessEnv;
} = {}) {
  const graphName = options.graphName || 'browser-fixture';
  const port = options.port ?? await freePort();
  const environment: NodeJS.ProcessEnv = {
    PATH: process.env.PATH || '/usr/bin:/bin', LANG: 'C.UTF-8', TZ: 'UTC',
    ANCHOR_BWRAP: process.env.ANCHOR_BWRAP,
    ANCHOR_RUNNER_BUNDLE_ROOT: join(root, 'catalog', graphName),
    ANCHOR_RUNNER_CATALOG_ROOT: join(root, 'catalog'),
    ANCHOR_RUNNER_STATE_ROOT: join(root, 'state'),
    ANCHOR_RUNNER_WORKSPACE_ROOT: join(root, 'workspaces'),
    ANCHOR_RUNNER_LIBRARY_ROOT: join(root, 'library'),
    ANCHOR_RUNNER_SCHEDULES_PATH: join(root, 'state/schedules.json'),
    ANCHOR_RUNNER_WEB_ROOT: fileURLToPath(new URL('../../dist', import.meta.url)),
    ANCHOR_RUNNER_GRAPH_NAME: graphName,
    ANCHOR_RUNNER_LISTEN: `127.0.0.1:${port}`,
    ANCHOR_RUNNER_ALLOWED_COMMANDS: 'sh,git,cat,printf,true,sleep,echo',
    ANCHOR_API_KEYS: '',
    ...options.environment,
  };
  for (const name of ['ANCHOR_RUNNER_BUNDLE_ROOT', 'ANCHOR_RUNNER_CATALOG_ROOT',
    'ANCHOR_RUNNER_STATE_ROOT', 'ANCHOR_RUNNER_WORKSPACE_ROOT', 'ANCHOR_RUNNER_LIBRARY_ROOT']) {
    await mkdir(environment[name]!, { recursive: true });
  }
  const definition = options.definition || { entry: 'seed', ops: { seed: { run: 'true' } },
    nodes: [{ id: 'seed', op: 'seed' }], edges: [] };
  await writeFile(join(environment.ANCHOR_RUNNER_BUNDLE_ROOT!, 'graph.json'), JSON.stringify(definition));
  await writeFile(join(environment.ANCHOR_RUNNER_BUNDLE_ROOT!, 'manifest.json'),
    JSON.stringify({ format: 1, graph: 'graph.json', plugins: [] }));
  let logs = '';
  return {
    base: `http://127.0.0.1:${port}`,
    environment,
    get logs() { return logs; },
    start() {
      const child = spawn(nativeHostBinary(), ['serve'], { cwd: root, env: environment, stdio: ['ignore', 'pipe', 'pipe'] });
      child.stdout?.on('data', data => { logs += data.toString(); });
      child.stderr?.on('data', data => { logs += data.toString(); });
      child.on('error', error => { logs += error.message; });
      return child;
    },
  };
}

export async function gooseEnvironment(providerUrl?: string): Promise<NodeJS.ProcessEnv> {
  const binary = process.env.ANCHOR_GOOSE_BINARY;
  if (!binary) throw new Error('ANCHOR_GOOSE_BINARY must select pinned Goose v1.53.0');
  const digest = createHash('sha256').update(await readFile(binary)).digest('hex');
  if (digest !== gooseSha256) throw new Error('Goose binary does not match the pinned SHA256');
  return {
    ANCHOR_GOOSE_BINARY: binary, ANCHOR_GOOSE_BINARY_SHA256: digest, ANCHOR_GOOSE_ALLOW_SHARED_NETWORK: '1',
    ...(providerUrl ? { ANCHOR_MODEL_API_KEY: 'fixture-only', ANCHOR_MODEL_URL: providerUrl,
      ANCHOR_MODEL_NAME: 'fixture-browser', ANCHOR_MODEL_WIRE_API: 'chat', ANCHOR_MODEL_ALIASES: '{}' } : {}),
  };
}

export async function fixturePlugin(library: string) {
  const directory = join(library, 'plugins/browser-plugin');
  await mkdir(join(directory, 'skills/evidence'), { recursive: true });
  await mkdir(join(directory, 'resources'), { recursive: true });
  await writeFile(join(directory, 'plugin.json'), JSON.stringify({ name: '浏览器证据',
    description: 'Native Plugin browser fixture', skills: 'skills/' }));
  await writeFile(join(directory, 'skills/evidence/SKILL.md'),
    '---\nname: evidence\ndescription: Read native evidence\n---\n# 浏览器证据\n\nRead /plugins/browser-plugin/resources/input.txt.\n');
  await writeFile(join(directory, 'resources/input.txt'), 'Native Plugin evidence');
  return directory;
}
