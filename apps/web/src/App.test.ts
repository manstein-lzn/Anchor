import { describe, expect, it } from 'vitest';
import { loadWorkbenchData } from './useWorkbenchData';

describe('loadWorkbenchData', () => {
  it('does not request the timeline when the current page has no timeline consumer', async () => {
    const calls: string[] = [];
    const api = async <T>(path: string): Promise<T> => {
      calls.push(path);
      if (path === '/graphs') return { graphs: [] } as T;
      return { runs: [] } as T;
    };

    await expect(loadWorkbenchData(api)).resolves.toEqual({ graphs: [], runs: [] });
    expect(calls).toEqual(['/graphs', '/runs']);
  });

  it('keeps Graph and Run data when the timeline projection is unavailable', async () => {
    const calls: string[] = [];
    const api = async <T>(path: string): Promise<T> => {
      calls.push(path);
      if (path === '/graphs') return { graphs: [{ graph: 'rust-rsi', running: null }] } as T;
      if (path === '/runs') return { runs: [{ run: 'run-1', graph: 'rust-rsi', status: 'completed', running: false, started: '', updated: '', executed: [], objective: 'review' }] } as T;
      throw new Error('GET /timeline → 404');
    };

    await expect(loadWorkbenchData(api, '/timeline?days=30')).resolves.toEqual({
      graphs: [{ graph: 'rust-rsi', running: null }],
      runs: [{ run: 'run-1', graph: 'rust-rsi', status: 'completed', running: false, started: '', updated: '', executed: [], objective: 'review' }],
      timeline: null,
      timelineProblem: 'GET /timeline → 404',
    });
    expect(calls).toEqual(['/graphs', '/timeline?days=30', '/runs']);
  });

  it('publishes the timeline without waiting for Graphs or requesting Runs twice', async () => {
    const calls: string[] = [];
    let finishGraphs!: (value: { graphs: [] }) => void;
    const timeline = { runs: [], scheduled: [], schedules: [] };
    const updates: unknown[] = [];
    const api = <T>(path: string): Promise<T> => {
      calls.push(path);
      return (path === '/graphs' ? new Promise<{ graphs: [] }>(resolve => { finishGraphs = resolve; })
        : Promise.resolve(timeline)) as Promise<T>;
    };
    const request = loadWorkbenchData(api, '/timeline?days=30', data => updates.push(data));
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(calls).toEqual(['/graphs', '/timeline?days=30']);
    expect(updates).toEqual([{ runs: [], timeline, timelineProblem: '' }]);
    finishGraphs({ graphs: [] });
    await expect(request).resolves.toEqual({ graphs: [], runs: [], timeline, timelineProblem: '' });
  });
});
