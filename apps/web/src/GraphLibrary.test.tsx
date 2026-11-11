/** @vitest-environment jsdom */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphLibrary } from './GraphLibrary';
import type { GraphSummary } from './model';

vi.mock('./GraphActions', () => ({ GraphActions: ({ graph }: { graph: string }) => <span data-actions={graph} /> }));

describe('Graph catalog task collection', () => {
  let container: HTMLDivElement, root: Root;
  const select = vi.fn(() => true);
  const graphs: GraphSummary[] = [
    { graph: 'assistant', running: null, listener: { platform: 'feishu', status: 'authenticated' } },
    { graph: 'task-manual', running: null },
    { graph: 'weather', running: null, completed_task: true },
  ];
  const render = async (items = graphs, name = 'assistant') => act(async () => root.render(
    <GraphLibrary graphs={items} runs={[]} name={name} busy={false} selectGraph={select}
      create={async () => {}} deleteGraph={async () => {}} sidebarControl={<button>收起侧栏</button>}>
      <button>导出</button>
    </GraphLibrary>));
  const completed = () => container.querySelector<HTMLDivElement>('.completed-tasks > div')!;
  const toggle = () => container.querySelector<HTMLButtonElement>('.completed-tasks-toggle')!;
  const main = () => container.querySelector('[aria-label="主工作流列表"]')!;
  const search = async (text: string) => act(async () => {
    const input = container.querySelector('input')!;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, text);
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });

  beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
    container = document.createElement('div'); root = createRoot(container); select.mockClear();
  });
  afterEach(async () => { await act(async () => root.unmount()); vi.unstubAllGlobals(); });

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
});
