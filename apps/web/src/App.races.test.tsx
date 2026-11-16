/** @vitest-environment jsdom */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { App } from './App';
import type { GraphSummary, OurGraph, OurRun, OurRunDetail, RunProjection, TimelineData } from './model';

vi.mock('./Pilot', () => ({ Pilot: () => null }));
vi.mock('./GraphEditorPage', () => ({ GraphEditorPage: () => null }));
vi.mock('./WorkflowCanvas', () => ({ WorkflowCanvas: ({ onPick }: { onPick: (value: { kind: 'node'; id: string }) => void }) =>
  <button onClick={() => onPick({ kind: 'node', id: 'worker' })}>选择 worker</button> }));
vi.mock('./RunInspector', () => ({ RunInspector: ({ run, node }: { run: string; node: string }) =>
  <aside aria-label="运行检查上下文">{run}/{node}</aside> }));
vi.mock('./Timeline', () => ({ Timeline: ({ data, onSelect }: { data: TimelineData | null; onSelect: (run: OurRun) => void }) =>
  <main>{data?.runs.map(run => <button key={run.run} onClick={() => onSelect(run)}>打开 {run.run}</button>)}</main> }));

const definition: OurGraph = { entry: 'worker', nodes: [{ id: 'worker', agent: 'role' }], edges: [] };
const graphs: GraphSummary[] = [{ graph: 'workflow', running: null }];
const settled: RunProjection = { phase: 'settled', outcome: 'done', active: false, reason: null, updated: null,
  actions: { pause: false, stop: false, resume: false, abandon: false } };
const runs: OurRun[] = ['run-a', 'run-b'].map(run => ({ run, graph: 'workflow', projection: settled, running: false,
  started: '2026-10-19T00:00:00Z', updated: '2026-10-19T00:00:00Z', executed: [], objective: run }));
const detailFor = (run: string): OurRunDetail => ({
  run, graph: 'workflow', definition, nodes: ['worker'], traces: {},
  projection: settled,
  state: { objective: `${run} objective`, started: '', updated: '', cursor: null,
    nodes: {}, passes: {}, decided: {}, executed: [], skipped: [], error: '' },
});
const response = (body: unknown, status = 200) => ({ ok: status >= 200 && status < 300, status, json: async () => body }) as Response;

describe('Run deletion navigation races', () => {
  let root: Root;
  let container: HTMLDivElement;
  let fetchMock: ReturnType<typeof vi.fn>;
  let resolveDelete: (value: Response) => void;
  const button = (label: string) => [...container.querySelectorAll<HTMLButtonElement>('button')].find(item => item.textContent === label)!;
  const openRun = async (run: string) => {
    await act(async () => button(`打开 ${run}`).click());
    await act(async () => vi.dynamicImportSettled());
  };

  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    vi.useFakeTimers();
    sessionStorage.clear(); localStorage.clear(); localStorage.setItem('anchor:view', 'runs');
    container = document.createElement('div'); document.body.appendChild(container); root = createRoot(container);
    const deletion = new Promise<Response>(resolve => { resolveDelete = resolve; });
    fetchMock = vi.fn((path: string, options?: RequestInit) => {
      if (path === '/runs/run-a' && options?.method === 'DELETE') return deletion;
      if (path === '/graphs') return Promise.resolve(response({ graphs }));
      if (path === '/runs') return Promise.resolve(response({ runs }));
      if (path.startsWith('/timeline?')) return Promise.resolve(response({ runs, scheduled: [], schedules: [] }));
      if (path === '/graphs/workflow') return Promise.resolve(response({ definition }));
      if (path.startsWith('/runs/')) return Promise.resolve(response(detailFor(path.slice('/runs/'.length))));
      throw new Error(`Unexpected request: ${options?.method} ${path}`);
    });
    vi.stubGlobal('fetch', fetchMock); vi.spyOn(window, 'confirm').mockReturnValue(true);
  });
  afterEach(async () => {
    await act(async () => root.unmount()); container.remove();
    vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
  });

  it('keeps a newly selected Run and node when an earlier deletion finishes', async () => {
    await act(async () => root.render(<App />));
    await openRun('run-a');
    await act(async () => button('删除运行记录').click());
    expect(fetchMock.mock.calls.filter(([path, options]) => path === '/runs/run-a' && options?.method === 'DELETE')).toHaveLength(1);
    await act(async () => button('运行看板').click());
    await openRun('run-b');
    await act(async () => button('选择 worker').click());
    expect(container.querySelector('[aria-label="运行检查上下文"]')?.textContent).toBe('run-b/worker');
    const listReads = fetchMock.mock.calls.filter(([path]) => path === '/runs').length;
    await act(async () => resolveDelete(response(undefined, 204)));
    expect(container.textContent).toContain('run-b objective');
    expect(container.querySelector('[aria-label="运行检查上下文"]')?.textContent).toBe('run-b/worker');
    expect(container.textContent).not.toContain('运行记录及其文件已删除。');
    expect(fetchMock.mock.calls.filter(([path]) => path === '/runs').length).toBeGreaterThan(listReads);
  });

  it('clears the deleted selection and reports success when navigation has not changed', async () => {
    await act(async () => root.render(<App />));
    await openRun('run-a');
    await act(async () => button('删除运行记录').click());
    await act(async () => resolveDelete(response(undefined, 204)));
    expect(container.querySelector('[aria-label="运行检查上下文"]')?.textContent).toBe('/');
    expect(container.textContent).toContain('运行记录及其文件已删除。');
  });
});
