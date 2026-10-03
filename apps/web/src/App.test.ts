import { describe, expect, it } from 'vitest';
import { loadWorkbenchData } from './App';

describe('loadWorkbenchData', () => {
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
    expect(calls).toEqual(['/graphs', '/runs', '/timeline?days=30']);
  });
});
