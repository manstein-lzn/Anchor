/** @vitest-environment jsdom */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from './api';
import { useWorkbenchData, type WorkbenchView } from './useWorkbenchData';

vi.mock('./api', () => ({ api: vi.fn() }));
describe('workbench page subscriptions', () => {
  let root: Root;
  let container: HTMLDivElement;
  let state: ReturnType<typeof useWorkbenchData>;
  const Harness = ({ view }: { view: WorkbenchView }) => { state = useWorkbenchData(view); return null; };
  const render = async (view: WorkbenchView) => act(async () => root.render(<Harness view={view} />));
  beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true); vi.useFakeTimers();
    container = document.createElement('div'); root = createRoot(container);
    vi.mocked(api).mockReset().mockImplementation(async path => path === '/graphs' ? { graphs: [{ graph: 'g' }] }
      : path === '/runs' ? { runs: [] } : { runs: [], scheduled: [], schedules: [] });
  });
  afterEach(async () => { await act(async () => root.unmount()); vi.useRealTimers(); vi.unstubAllGlobals(); });

  it('suspends workbench polling on Pilot and restores each page subscription immediately', async () => {
    await render('graph');
    expect(api).toHaveBeenCalledTimes(2);
    expect(vi.mocked(api).mock.calls.map(([path]) => path)).toEqual(['/graphs', '/runs']);
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(api).toHaveBeenCalledTimes(4);
    await render('pilot');
    await act(async () => vi.advanceTimersByTimeAsync(9000));
    expect(api).toHaveBeenCalledTimes(4);
    await render('runs');
    expect(vi.mocked(api).mock.calls.slice(-2).map(([path]) => path)).toEqual(['/graphs', expect.stringContaining('/timeline?')]);
    await render('graph');
    expect(vi.mocked(api).mock.calls.slice(-2).map(([path]) => path)).toEqual(['/graphs', '/runs']);
  });

  it('checks first-visit API authorization on Pilot without starting Run or timeline polling', async () => {
    await render('pilot');
    expect(vi.mocked(api).mock.calls.map(([path]) => path)).toEqual(['/graphs']);
    await act(async () => vi.advanceTimersByTimeAsync(12000));
    expect(api).toHaveBeenCalledTimes(1);
  });

  it('keeps unchanged projection identities but immediately publishes changed Host facts', async () => {
    await render('graph');
    const graphs = state.graphs, runs = state.runs;
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(state.graphs).toBe(graphs);
    expect(state.runs).toBe(runs);
    vi.mocked(api).mockImplementation(async path => path === '/graphs'
      ? { graphs: [{ graph: 'g', listener: { status: 'unavailable' } }] }
      : { runs: [{ run: 'new', running: true }] });
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(state.graphs).not.toBe(graphs);
    expect(state.graphs[0].listener?.status).toBe('unavailable');
    expect(state.runs).not.toBe(runs);
    expect(state.runs[0].run).toBe('new');
  });

  it('ignores a projection received after its page was left', async () => {
    await render('graph');
    let release!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(path => path === '/graphs' ? new Promise(resolve => { release = resolve; })
      : Promise.resolve({ runs: [] }));
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    const signal = vi.mocked(api).mock.calls.at(-2)![3]!;
    await render('pilot'); expect(signal.aborted).toBe(true);
    await act(async () => release({ graphs: [{ graph: 'late' }] }));
    expect(state.graphs.map(item => item.graph)).toEqual(['g']);
  });

  it('does not cancel or duplicate a slow projection on polling ticks', async () => {
    let release!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(path => path === '/graphs' ? new Promise(resolve => { release = resolve; })
      : Promise.resolve({ runs: [] }));
    await render('graph');
    const signal = vi.mocked(api).mock.calls[0][3]!;
    await act(async () => vi.advanceTimersByTimeAsync(12000));
    expect(api).toHaveBeenCalledTimes(2);
    expect(signal.aborted).toBe(false);
    await act(async () => release({ graphs: [{ graph: 'loaded' }] }));
    expect(state.graphs.map(item => item.graph)).toEqual(['loaded']);
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(api).toHaveBeenCalledTimes(4);
  });

  it('retries on the next tick after an in-flight request fails', async () => {
    vi.mocked(api).mockRejectedValueOnce(new Error('unavailable'));
    await render('graph');
    expect(state.problem).toContain('unavailable');
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(state.graphs.map(item => item.graph)).toEqual(['g']);
    expect(state.problem).toBe('');
  });

  it('aborts sibling reads after a projection fails before allowing the next poll', async () => {
    vi.mocked(api).mockImplementation(path => path === '/graphs'
      ? Promise.reject(new Error('unavailable')) : new Promise(() => undefined));
    await render('graph');
    const signal = vi.mocked(api).mock.calls[1][3]!;
    expect(state.problem).toContain('unavailable');
    expect(signal.aborted).toBe(true);
    vi.mocked(api).mockImplementation(async path => path === '/graphs'
      ? { graphs: [{ graph: 'recovered' }] } : { runs: [] });
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(api).toHaveBeenCalledTimes(4);
    expect(state.graphs.map(item => item.graph)).toEqual(['recovered']);
    expect(state.problem).toBe('');
  });

  it('keeps a slow timeline read in flight without restarting its projection', async () => {
    let release!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(async path => path === '/graphs' ? { graphs: [{ graph: 'g' }] }
      : path === '/runs' ? { runs: [] } : new Promise(resolve => { release = resolve; }));
    await render('runs');
    const signal = vi.mocked(api).mock.calls[1][3]!;
    expect(state.graphs.map(item => item.graph)).toEqual(['g']);
    expect(state.timeline).toBeNull();
    await act(async () => vi.advanceTimersByTimeAsync(9000));
    expect(api).toHaveBeenCalledTimes(2);
    expect(signal.aborted).toBe(false);
    const timeline = { runs: [], scheduled: [], schedules: [] };
    await act(async () => release(timeline));
    expect(state.timeline).toEqual(timeline);
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(api).toHaveBeenCalledTimes(4);
  });

  it('keeps the new page request in flight when an aborted response settles late', async () => {
    const releases: ((value: unknown) => void)[] = [];
    vi.mocked(api).mockImplementation(path => path === '/graphs'
      ? new Promise(resolve => { releases.push(resolve); }) : Promise.resolve({ runs: [] }));
    await render('graph');
    const oldSignal = vi.mocked(api).mock.calls[0][3]!;
    await render('runs');
    const nextSignal = vi.mocked(api).mock.calls[2][3]!;
    expect(oldSignal.aborted).toBe(true);
    await act(async () => releases[0]({ graphs: [{ graph: 'stale' }] }));
    await act(async () => vi.advanceTimersByTimeAsync(9000));
    expect(api).toHaveBeenCalledTimes(4);
    expect(nextSignal.aborted).toBe(false);
    expect(state.graphs).toEqual([]);
    await act(async () => releases[1]({ graphs: [{ graph: 'current' }] }));
    expect(state.graphs.map(item => item.graph)).toEqual(['current']);
  });
});
