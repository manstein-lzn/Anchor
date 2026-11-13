/** @vitest-environment jsdom */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { OurRun, OurRunDetail } from './model';
import { useRunDetail, type RunDetailLoad } from './useRunDetail';

const api = vi.hoisted(() => vi.fn());
vi.mock('./api', async importOriginal => {
  const original = await importOriginal<typeof import('./api')>();
  return { ...original, api };
});

const deferred = <T,>() => {
  let resolve!: (value: T) => void; let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((ok, bad) => { resolve = ok; reject = bad; });
  return { promise, resolve, reject };
};
const detail = (run: string): OurRunDetail => ({
  run, graph: 'graph', nodes: [], traces: {},
  state: { objective: run, started: '', updated: '', cursor: null, nodes: {}, passes: {}, decided: {}, executed: [], skipped: [], error: '' },
});
const summary = (run: string, updated = '1', running = false): OurRun => ({
  run, graph: 'graph', running, started: '', updated, executed: [], objective: run,
});

describe('useRunDetail', () => {
  let root: Root; let element: HTMLDivElement; let latest: RunDetailLoad;
  const Probe = ({ run, runs }: { run: string; runs: OurRun[] }) => { latest = useRunDetail(run, true, runs); return null; };
  const render = async (run: string, runs: OurRun[]) => act(async () => root.render(<Probe run={run} runs={runs} />));

  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    api.mockReset(); element = document.createElement('div'); document.body.appendChild(element); root = createRoot(element);
  });
  afterEach(async () => { await act(async () => root.unmount()); element.remove(); });

  it('keeps the same request alive across unchanged list refreshes and queues a changed refresh', async () => {
    const first = deferred<OurRunDetail>(); const second = deferred<OurRunDetail>();
    api.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    await render('run-a', [summary('run-a')]);
    const firstSignal = api.mock.calls[0][3] as AbortSignal;
    await render('run-a', [summary('run-a')]);
    expect(api).toHaveBeenCalledTimes(1); expect(firstSignal.aborted).toBe(false);
    await render('run-a', [summary('run-a', '2')]);
    expect(api).toHaveBeenCalledTimes(1); expect(firstSignal.aborted).toBe(false);
    await act(async () => first.resolve(detail('run-a')));
    expect(api).toHaveBeenCalledTimes(2);
    await act(async () => second.resolve(detail('run-a')));
    expect(latest.detail?.run).toBe('run-a'); expect(latest.loading).toBe(false);
  });

  it('polls active Run detail without overlapping and stops polling after it settles', async () => {
    vi.useFakeTimers();
    const first = deferred<OurRunDetail>(); const second = deferred<OurRunDetail>();
    api.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise).mockResolvedValue(detail('run-a'));
    await render('run-a', [summary('run-a', '1', true)]);
    await act(async () => vi.advanceTimersByTime(6000));
    expect(api).toHaveBeenCalledTimes(1);
    await act(async () => first.resolve(detail('run-a')));
    expect(api).toHaveBeenCalledTimes(2);
    await act(async () => second.resolve(detail('run-a')));
    await render('run-a', [summary('run-a', '1', false)]);
    await vi.waitFor(() => expect(api).toHaveBeenCalledTimes(3));
    await act(async () => vi.advanceTimersByTime(6000));
    expect(api).toHaveBeenCalledTimes(3);
    vi.useRealTimers();
  });

  it('does not poll an inactive Run whose summary is unchanged', async () => {
    vi.useFakeTimers();
    api.mockResolvedValue(detail('run-a'));
    await render('run-a', [summary('run-a')]);
    await act(async () => Promise.resolve());
    await act(async () => vi.advanceTimersByTime(9000));
    expect(api).toHaveBeenCalledTimes(1);
    vi.useRealTimers();
  });

  it('shows a timeout, retries, and preserves loaded detail when a later refresh fails', async () => {
    api.mockRejectedValueOnce(new Error('signal timed out')).mockResolvedValueOnce(detail('run-a'))
      .mockRejectedValueOnce(new Error('refresh timed out'));
    await render('run-a', [summary('run-a')]);
    await vi.waitFor(() => expect(latest.error).toBe('signal timed out'));
    expect(latest.detail).toBeNull();
    await act(async () => latest.retry());
    await vi.waitFor(() => expect(latest.detail?.run).toBe('run-a'));
    await render('run-a', [summary('run-a', '2')]);
    await vi.waitFor(() => expect(latest.error).toBe('refresh timed out'));
    expect(latest.detail?.run).toBe('run-a');
  });

  it('never displays a late response from the previously selected Run', async () => {
    const old = deferred<OurRunDetail>(); const fresh = deferred<OurRunDetail>();
    api.mockReturnValueOnce(old.promise).mockReturnValueOnce(fresh.promise);
    await render('run-a', [summary('run-a')]);
    await render('run-b', [summary('run-b')]);
    expect((api.mock.calls[0][3] as AbortSignal).aborted).toBe(true);
    await act(async () => old.resolve(detail('run-a')));
    expect(latest.detail).toBeNull();
    await act(async () => fresh.resolve(detail('run-b')));
    expect(latest.detail?.run).toBe('run-b');
  });
});
