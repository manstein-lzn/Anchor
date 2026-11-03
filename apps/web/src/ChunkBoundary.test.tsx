// @vitest-environment jsdom
import { describe, expect, it, beforeEach } from 'vitest';
import { isChunkLoadError, reloadForStaleChunk } from './ChunkBoundary';

describe('stale chunk detection', () => {
  it('recognizes the errors a removed chunk produces in each browser', () => {
    for (const message of [
      'Failed to fetch dynamically imported module: https://host/assets/RunDetailPage-CemRcHXC.js',
      'Importing a module script failed.',
      'error loading dynamically imported module',
      'ChunkLoadError: Loading chunk 12 failed.',
    ]) {
      expect(isChunkLoadError(new TypeError(message))).toBe(true);
    }
  });

  it('does not swallow ordinary render errors', () => {
    expect(isChunkLoadError(new Error('Cannot read properties of undefined'))).toBe(false);
    expect(isChunkLoadError(undefined)).toBe(false);
  });
});

describe('reload guard', () => {
  const original = globalThis.location;
  let reloads = 0;

  beforeEach(() => {
    reloads = 0;
    sessionStorage.clear();
    Object.defineProperty(globalThis, 'location', {
      configurable: true,
      value: { ...original, reload: () => { reloads++; } },
    });
  });

  it('reloads once, then refuses a second reload inside the window', () => {
    expect(reloadForStaleChunk(1_000)).toBe(true);
    expect(reloadForStaleChunk(1_500)).toBe(false);
    expect(reloads).toBe(1);
  });

  it('reloads again after the window expires', () => {
    expect(reloadForStaleChunk(1_000)).toBe(true);
    expect(reloadForStaleChunk(60_000)).toBe(true);
    expect(reloads).toBe(2);
  });
});
