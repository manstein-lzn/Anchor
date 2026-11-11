#!/usr/bin/env node
// Source-checkout onboarding. Product writes use the Library installer and Host API.
import { execFileSync } from 'node:child_process';
import { cp, mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const starter = JSON.parse(await readFile(join(repo, 'examples/starter.json'), 'utf8'));
const exists = async path => {
  try { await stat(path); return true; }
  catch (error) { if (error.code === 'ENOENT') return false; throw error; }
};
const marker = root => join(root, '.source-starter.json');
const complete = async (root, items) => {
  await mkdir(root, { recursive: true });
  await writeFile(marker(root), JSON.stringify({ items }, null, 2) + '\n');
};

async function plugins(target) {
  const root = process.env.ANCHOR_RUNNER_LIBRARY_ROOT;
  if (await exists(marker(root))) return;
  const pending = [];
  for (const [id, binaries] of Object.entries(starter.plugins)) {
    if (!await exists(join(root, 'plugins', id))) pending.push([id, binaries]);
  }
  if (pending.length) {
    console.log('Preparing bundled starter plugins...');
    const packages = [...new Set(['anchor-library', ...pending.flatMap(([, binaries]) => binaries)])];
    execFileSync('cargo', ['build', '--manifest-path', join(repo, 'rust/Cargo.toml'), '--locked',
      ...packages.flatMap(name => ['-p', name]), '--bins'],
    { stdio: 'inherit', env: { ...process.env, CARGO_TARGET_DIR: resolve(target, '..') } });
    const staging = await mkdtemp(join(tmpdir(), 'anchor-starter-'));
    try {
      for (const [id, binaries] of pending) {
        const directory = join(staging, id);
        await cp(join(repo, 'plugins', id), directory, { recursive: true });
        if (binaries.length) await mkdir(join(directory, 'bin'), { recursive: true });
        for (const binary of binaries) await cp(join(target, binary), join(directory, 'bin', binary));
        const candidate = ['--directory', directory, '--id', id];
        const review = JSON.parse(execFileSync(join(target, 'anchor-library'),
          ['--root', root, 'install', ...candidate, '--review'], { encoding: 'utf8' }));
        if (!['system', 'extension'].includes(review.kind)) throw new Error(`Unknown plugin kind for ${id}`);
        const publication = review.kind === 'system'
          ? ['install-system', ...candidate, '--digest', review.digest]
          : ['install', ...candidate];
        execFileSync(join(target, 'anchor-library'), ['--root', root, ...publication], { stdio: 'inherit' });
      }
    } finally {
      await rm(staging, { recursive: true, force: true });
    }
  }
  await complete(root, Object.keys(starter.plugins));
}

async function graphs() {
  const root = process.env.ANCHOR_RUNNER_CATALOG_ROOT;
  if (await exists(marker(root))) return;
  const api = process.env.ANCHOR_WEB_API_URL.replace(/\/$/, '');
  const headers = { 'Content-Type': 'application/json' };
  for (const name of starter.graphs) {
    const current = await fetch(`${api}/graphs/${name}`, { headers, signal: AbortSignal.timeout(30000) });
    if (current.ok) continue;
    if (current.status !== 404) throw new Error(`Cannot inspect starter Graph ${name}: HTTP ${current.status}`);
    const definition = JSON.parse(await readFile(join(repo, 'examples/graphs', `${name}.json`), 'utf8'));
    const response = await fetch(`${api}/graphs`, { method: 'POST', headers, body: JSON.stringify({ name, definition }),
      signal: AbortSignal.timeout(30000) });
    // A concurrent author can create the same name after the read; never overwrite it.
    if (!response.ok && response.status !== 409) {
      throw new Error(`Cannot import starter Graph ${name}: HTTP ${response.status}: ${await response.text()}`);
    }
    if (response.ok) console.log(`Starter Graph available: ${name}`);
  }
  await complete(root, starter.graphs);
}

try {
  if (process.argv[2] === 'plugins') await plugins(resolve(process.argv[3]));
  else if (process.argv[2] === 'graphs') await graphs();
  else throw new Error('usage: dev-examples.mjs plugins TARGET/debug | graphs');
} catch (error) {
  console.error(`Starter content preparation failed: ${error.message}`);
  process.exitCode = 1;
}
