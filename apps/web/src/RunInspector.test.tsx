/** @vitest-environment jsdom */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { RunInspector } from './RunInspector';
import type { OurRunDetail, RunProjection } from './model';

vi.mock('./Plugins', () => ({ Plugins: () => null }));
vi.mock('./Transcript', () => ({ Transcript: () => null }));

const fixture = (projection?: RunProjection): OurRunDetail => ({
  run: 'run', graph: 'graph', nodes: ['worker'], traces: {},
  active: true, projection,
  state: { objective: '', started: '', updated: '',
    cursor: { node: 'worker', pass: 1, dir: '' }, nodes: {}, passes: {}, decided: {}, executed: [], skipped: [], error: '' },
});

describe('Run Inspector status projection', () => {
  let element: HTMLDivElement; let root: Root;
  const render = async (detail: OurRunDetail) => act(async () => root.render(<RunInspector run="run" node="worker" detail={detail} />));
  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    element = document.createElement('div'); document.body.appendChild(element); root = createRoot(element);
  });
  afterEach(async () => { await act(async () => root.unmount()); element.remove(); vi.unstubAllGlobals(); });

  it.each([
    [{ phase: 'executing' }, '等待接续'],
    [{ phase: 'waiting', waiting_for: 'effect', reason: '工具结果未记录' }, '等待核查效果：工具结果未记录'],
    [{ phase: 'waiting', waiting_for: 'human', reason: 'paused' }, '等待你处理：paused'],
  ] as const)('uses %s for a retained cursor instead of claiming it is executing', async (view, label) => {
    const detail = fixture({ active: false, reason: null, updated: null, actions: { pause: false, stop: false, resume: false, abandon: false }, ...view });
    detail.calls = [{ node: 'worker', invocation: 1, graph: 'target', run: 'child', mode: 'wait' }];
    await render(detail);
    expect(element.querySelector('.node-summary .pill')?.textContent).toBe(label);
    expect(element.querySelector('.call-records')?.textContent).toContain(`本节点：${label}`);
    expect(element.textContent).not.toContain('执行中');
    expect(element.textContent).not.toContain('节点开始执行后');
  });

  it('keeps an unknown current state explicit when projection is absent', async () => {
    await render(fixture());
    expect(element.querySelector('.node-summary .pill')?.textContent).toBe('运行状态不可用');
    expect(element.textContent).not.toContain('执行中');
    expect(element.textContent).not.toContain('等待执行记录');
  });

  it('preserves a recorded submission independently of missing current execution observation', async () => {
    const detail = fixture(); detail.state.cursor = null;
    detail.state.nodes.worker = { node_id: 'worker', pass_number: 1, submission: 'frozen result', files: [], submitted: true, exit_status: 'completed', route: null };
    await render(detail);
    expect(element.querySelector('.node-summary .pill')?.textContent).toBe('已提交');
    expect(element.textContent).toContain('frozen result');
  });

  it.each([
    ['expired', '交付文件已过期'], ['missing', '交付文件不可用'], ['corrupt', '交付文件不可用'],
  ] as const)('shows %s delivery metadata without requesting file bytes', async (availability, message) => {
    const detail = fixture();
    const artifact = { id: 'fs2-delivered', node_id: 'worker', invocation: 3 };
    detail.state.nodes.worker = { node_id: 'worker', pass_number: 3, submission: 'Report ready', files: ['report.md'],
      submitted: true, exit_status: 'completed', route: null, artifact, artifact_availability: availability };
    const fetchMock = vi.fn().mockResolvedValue(Response.json({ artifact, artifact_availability: availability,
      files: [{ path: 'report.md', size: 21 }], truncated: false }));
    vi.stubGlobal('fetch', fetchMock);
    await act(async () => root.render(<RunInspector run="run" node="worker" detail={detail} targetPath="report.md" />));
    expect(element.querySelector('.inspector-note')?.textContent).toContain('第 3 轮交付的固定版本文件');
    expect(element.textContent).toContain(message);
    expect(element.querySelector('.file-name')?.textContent).toContain('report.md');
    expect((element.querySelector('.file-name') as HTMLButtonElement).disabled).toBe(true);
    expect((element.querySelector('[aria-label="下载 report.md"]') as HTMLButtonElement).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toBe('/api/artifacts/fs2-delivered/files');
  });
});
