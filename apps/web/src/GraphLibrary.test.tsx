/** @vitest-environment jsdom */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphLibrary } from './GraphLibrary';
import type { ComponentProps } from 'react';
import type { GraphActions } from './GraphActions';
import type { GraphSummary, OurRun } from './model';

vi.mock('./GraphActions', () => ({ GraphActions: ({ graph, runCount, running, busy, onDelete }: ComponentProps<typeof GraphActions>) =>
  <button data-actions={graph} data-run-count={runCount} data-running={running} disabled={busy}
    onClick={() => void onDelete(graph, 'confirmed-version')}>操作</button> }));

describe('Graph catalog task collection', () => {
  let container: HTMLDivElement, root: Root;
  const select = vi.fn(() => true);
  const remove = vi.fn(async () => {});
  const graphs: GraphSummary[] = [
    { graph: 'assistant', running: null, listener: { platform: 'feishu', status: 'authenticated' } },
    { graph: 'task-manual', running: null },
    { graph: 'weather', running: null, completed_task: true },
  ];
  const render = async (items = graphs, name = 'assistant', runs: OurRun[] = []) => act(async () => root.render(
    <GraphLibrary graphs={items} runs={runs} name={name} busy={false} selectGraph={select}
      create={async () => {}} deleteGraph={remove} sidebarControl={<button>收起侧栏</button>}>
      <button>导出</button>
    </GraphLibrary>));
  const completed = () => container.querySelector<HTMLDivElement>('.completed-tasks > div')!;
  const toggle = () => container.querySelector<HTMLButtonElement>('.completed-tasks-toggle')!;
  const main = () => container.querySelector('[aria-label="主工作流列表"]')!;
  const row = (graph: string) => container.querySelector<HTMLDivElement>(`[data-graph="${graph}"]`)!;
  const taskToggle = () => container.querySelector<HTMLButtonElement>('.graph-tasks-toggle')!;
  const taskContent = () => container.querySelector<HTMLDivElement>('.graph-created-tasks > div')!;
  const search = async (text: string) => act(async () => {
    const input = container.querySelector('input')!;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, text);
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });

  beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 0; });
    container = document.createElement('div'); document.body.appendChild(container); root = createRoot(container);
    select.mockReset().mockReturnValue(true); remove.mockClear();
  });
  afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.unstubAllGlobals(); });

  it('keeps normal and task-named user Graphs visible while collecting only Host-marked completions', async () => {
    await render();
    expect(main().textContent).toContain('assistant');
    expect(main().textContent).toContain('task-manual');
    expect(main().textContent).not.toContain('weather');
    expect(container.querySelector('.section-heading .count')?.textContent).toBe('2');
    expect(toggle().textContent).toBe('已完成的一次性任务1');
    expect(completed().hidden).toBe(true);
    expect(main().querySelector('.listener-listening')).not.toBeNull();
    expect(container.textContent).toContain('导出');
  });

  it('expands retained task Graphs with their original selection and row controls', async () => {
    await render();
    await act(async () => toggle().click());
    expect(completed().hidden).toBe(false);
    expect(toggle().getAttribute('aria-expanded')).toBe('true');
    expect(completed().textContent).toContain('执行记录和回执保留');
    expect(completed().querySelector('[data-actions="weather"]')).not.toBeNull();
    await act(async () => completed().querySelector<HTMLButtonElement>('.library-row')!.click());
    expect(select).toHaveBeenCalledWith('weather');
    await render(graphs, 'weather');
    expect(completed().querySelector('[aria-pressed="true"]')).not.toBeNull();
    await act(async () => toggle().click());
    expect(completed().hidden).toBe(true);
  });

  it('searches completed tasks without requiring a second search or losing the collapse control', async () => {
    await render();
    await search('WEATHER');
    expect(main().textContent).toBe('');
    expect(completed().hidden).toBe(false);
    expect(completed().textContent).toContain('weather');
    await act(async () => toggle().click());
    expect(completed().hidden).toBe(true);
    await search('missing');
    expect(container.textContent).toContain('没有匹配的工作流');
    await search('');
    expect(completed().hidden).toBe(true);
    expect(main().textContent).toContain('assistant');
  });

  it('returns a reused task to the main list from the next Host projection', async () => {
    await render();
    await render(graphs.map(item => item.graph === 'weather' ? { ...item, completed_task: undefined, running: 'new-run', active_runs: ['new-run'] } : item));
    expect(main().textContent).toContain('weather执行中');
    expect(container.querySelector('.completed-tasks')).toBeNull();
    expect(container.querySelector('.section-heading .count')?.textContent).toBe('3');
  });

  it('shows only names while retaining objective search and completion classification', async () => {
    const graph = 'task-0123456789abcdef';
    const objective = '  每日天气简报  ';
    await render([{ graph, objective, running: null }], graph);
    const row = main().querySelector<HTMLButtonElement>('.library-row')!;
    expect(row.querySelector('.library-name')?.textContent).toBe(graph);
    expect(row.querySelector('.library-identifier')).toBeNull();
    expect(row.querySelector('.library-label')?.getAttribute('title')).toBe(graph);
    expect(row.textContent).not.toContain('每日天气简报');
    expect(row.getAttribute('aria-pressed')).toBe('true');
    expect(container.querySelector('.completed-tasks')).toBeNull();
    expect(main().querySelector('[data-actions]')?.getAttribute('data-actions')).toBe(graph);
    await act(async () => row.click());
    expect(select).toHaveBeenCalledWith(graph);
    await search('天气');
    expect(main().querySelector('.library-row')).not.toBeNull();
    await search('TASK-0123');
    expect(main().querySelector('.library-row')).not.toBeNull();
    await render([{ graph, objective, running: null, completed_task: true }], graph);
    await search('天气');
    expect(main().textContent).toBe('');
    expect(completed().hidden).toBe(false);
    expect(completed().textContent).toContain(graph);
    expect(completed().textContent).not.toContain('每日天气简报');
  });

  it('falls back to stable identifiers for unavailable or blank objectives', async () => {
    await render([
      { graph: 'missing', running: null },
      { graph: 'blank', objective: ' \n ', running: null },
      { graph: 'same', objective: 'same', running: null },
    ]);
    expect([...main().querySelectorAll('.library-name')].map(item => item.textContent))
      .toEqual(['missing', 'blank', 'same']);
    expect(main().querySelectorAll('.library-identifier')).toHaveLength(0);
  });

  const family: GraphSummary[] = [
    { graph: 'assistant', objective: '日常工作助手', running: null, listener: { platform: 'feishu', status: 'authenticated' } },
    { graph: 'shared-briefing', objective: '统一生产晨报', running: null, schedule_summary: { total: 1, enabled: 1, recurring: 1 } },
    { graph: 'task-abc', objective: '核查天气来源', running: null, task_source: 'assistant' },
    { graph: 'task-def', objective: '未完成的研究', running: null, task_source: 'assistant' },
    { graph: 'finished', objective: '完成的月报', running: null, task_source: 'assistant', completed_task: true },
  ];

  it('collects created tasks once under their source and keeps shared production independent', async () => {
    await render(family);
    expect([...main().children].map(group => group.querySelector('[data-graph]')?.getAttribute('data-graph')))
      .toEqual(['assistant', 'shared-briefing']);
    expect(taskToggle().textContent).toBe('创建的任务2');
    expect(taskToggle().getAttribute('aria-expanded')).toBe('false');
    expect(taskToggle().getAttribute('aria-controls')).toBe(taskContent().id);
    expect(taskContent().hidden).toBe(true);
    expect(container.querySelectorAll('[data-graph="task-abc"]')).toHaveLength(1);
    expect(completed().querySelector('[data-graph="finished"]')).not.toBeNull();
    expect(main().querySelector('[data-graph="finished"]')).toBeNull();
    expect(container.querySelector('.section-heading .count')?.textContent).toBe('4');
    await act(async () => taskToggle().click());
    expect(taskContent().hidden).toBe(false);
    await act(async () => taskToggle().click());
    expect(taskContent().hidden).toBe(true);
  });

  it('reveals matching children by objective or ID with source context, then keeps the selected child open', async () => {
    await render(family);
    await search('天气');
    expect(taskContent().hidden).toBe(false);
    expect(row('assistant')).not.toBeNull();
    expect(row('task-abc')).not.toBeNull();
    expect(row('task-def')).toBeNull();
    await search('TASK-ABC');
    expect(row('task-abc')).not.toBeNull();
    await act(async () => row('task-abc').querySelector<HTMLButtonElement>('.library-row')!.click());
    expect(select).toHaveBeenCalledWith('task-abc');
    await render(family, 'task-abc');
    await search('');
    expect(taskContent().hidden).toBe(false);
    expect(row('task-abc').querySelector('[aria-pressed="true"]')).not.toBeNull();
    expect(row('task-def')).not.toBeNull();
    await search('nothing-matches');
    expect(main().children).toHaveLength(0);
    expect(container.textContent).toContain('没有匹配的工作流');
  });

  it('reveals deep-linked selections without changing the navigation guard', async () => {
    await render(family, 'task-abc');
    expect(taskContent().hidden).toBe(false);
    select.mockReturnValue(false);
    await act(async () => row('task-def').querySelector<HTMLButtonElement>('.library-row')!.click());
    expect(select).toHaveBeenCalledWith('task-def');
    expect(row('task-abc').querySelector('[aria-pressed="true"]')).not.toBeNull();
    expect(row('task-def').querySelector('[aria-pressed="true"]')).toBeNull();
    await render(family, 'finished');
    expect(completed().hidden).toBe(false);
    await search('月报');
    await search('');
    expect(completed().hidden).toBe(true);
  });

  it('does not hide an unfinished child under a completed or absent source', async () => {
    await render([
      { graph: 'finished-parent', running: null, completed_task: true },
      { graph: 'unfinished', running: null, task_source: 'finished-parent' },
      { graph: 'dangling', running: null, task_source: 'absent' },
      { graph: 'self', running: null, task_source: 'self' },
      { graph: 'cycle-a', running: null, task_source: 'cycle-b' },
      { graph: 'cycle-b', running: null, task_source: 'cycle-a' },
    ]);
    expect(main().querySelectorAll('[data-graph]')).toHaveLength(5);
    expect(container.querySelector('.graph-created-tasks')).toBeNull();
    expect(row('unfinished').closest('[hidden]')).toBeNull();
    expect(row('finished-parent').closest('[hidden]')).not.toBeNull();
  });

  it('shows authoritative recurrence while retaining listener and running Graph semantics', async () => {
    await render([
      ...family,
      { graph: 'working', running: null, active_runs: ['r1', 'r2'], listener: { platform: 'wecom', status: 'authenticated' } },
      { graph: 'daily-by-name-only', running: null },
      { graph: 'one-time', running: null, schedule_summary: { total: 2, enabled: 1, recurring: 0 } },
      { graph: 'disabled-recurring', running: null, schedule_summary: { total: 2, enabled: 0, recurring: 2 } },
    ]);
    expect(row('shared-briefing').querySelector('.graph-recurring')?.textContent).toBe('周期任务');
    expect(row('disabled-recurring').querySelector('.graph-recurring')?.getAttribute('title')).toBe('共 2 个计划，0 个启用，2 个周期计划');
    expect(row('daily-by-name-only').querySelector('.graph-recurring')).toBeNull();
    expect(row('one-time').querySelector('.graph-recurring')).toBeNull();
    expect(row('assistant').classList.contains('listener-listening')).toBe(true);
    expect(row('working').classList.contains('listener-working')).toBe(true);
    expect(row('working').textContent).toContain('执行中 2');
  });

  it('preserves the shared row action contract for grouped tasks and restores focus after delete', async () => {
    const runs = [{ graph: 'task-abc' }, { graph: 'task-abc' }, { graph: 'unrelated' }] as OurRun[];
    await render(family, 'task-abc', runs);
    const actions = row('task-abc').querySelector<HTMLButtonElement>('[data-actions]')!;
    expect(actions.dataset.runCount).toBe('2');
    expect(actions.dataset.running).toBe('false');
    await act(async () => actions.click());
    expect(remove).toHaveBeenCalledWith('task-abc', 'confirmed-version');
    expect(document.activeElement).toBe(container.querySelector('.new-graph'));
  });

  it('distinguishes an empty catalog from an unmatched search', async () => {
    await render([]);
    expect(container.textContent).toContain('从一个想法开始');
    expect(container.textContent).not.toContain('没有匹配的工作流');
    await render(family);
    await search('unmatched');
    expect(container.textContent).not.toContain('从一个想法开始');
    expect(container.textContent).toContain('没有匹配的工作流');
  });
});
