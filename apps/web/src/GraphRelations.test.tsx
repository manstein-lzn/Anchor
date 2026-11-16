/** @vitest-environment jsdom */
import { act, type ReactNode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphRelations } from './GraphRelations';
import type { GraphRelationsData } from './model';

const { api } = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock('./api', () => ({ api, reason: (error: Error) => error.message }));
vi.mock('./ui', () => ({ Modal: ({ children }: { children: ReactNode }) => <div>{children}</div> }));

describe('Graph relationship meanings', () => {
  let container: HTMLDivElement, root: Root;
  const open = vi.fn();
  const data: GraphRelationsData = {
    graphs: [
      { graph: 'assistant', objective: '飞书助手', schedules: 0 },
      { graph: 'task-hash', objective: '每日提醒', task_source: 'assistant', schedules: 1 },
      { graph: 'briefing', objective: 'AI 晨报', schedules: 1 },
      { graph: 'vip', objective: 'VIP 助手', schedules: 0 },
      { graph: 'recovery', objective: '补投任务', schedules: 0 },
      { graph: 'delivery', objective: '报告投递', schedules: 0 },
      { graph: 'unrelated', schedules: 0 },
    ],
    calls: [{ graph: 'recovery', node: 'send', op: 'send', target: 'delivery', mode: 'wait' }],
    subscriptions: [
      { graph: 'assistant', target: 'briefing', schedule: 'schedule-1', active: true, authorized: true, not_before: '09:00' },
      { graph: 'vip', target: 'briefing', schedule: 'schedule-1', active: true, authorized: false, not_before: '09:05' },
    ],
  };
  const render = async (graph = 'assistant') => act(async () => root.render(
    <GraphRelations graph={graph} close={() => {}} onOpen={open} />));
  const changeSearch = async (value: string) => act(async () => {
    const input = container.querySelector<HTMLInputElement>('input:not([type="checkbox"])')!;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
  beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
    container = document.createElement('div'); root = createRoot(container); open.mockClear();
    api.mockReset().mockResolvedValue(data);
  });
  afterEach(async () => { await act(async () => root.unmount()); vi.unstubAllGlobals(); });

  it('shows creation and subscriptions separately without turning a producer into an owned task', async () => {
    await render();
    const created = container.querySelector('[aria-label="创建归属"]')!;
    const subscribed = container.querySelector('[aria-label="结果订阅"]')!;
    expect(created.textContent).toContain('task-hash');
    expect(created.textContent).not.toContain('briefing');
    expect(subscribed.textContent).toContain('assistant');
    expect(subscribed.textContent).toContain('briefing');
    expect([...container.querySelectorAll('select option')].map(item => item.textContent))
      .toEqual(data.graphs.map(item => item.graph));
    for (const item of data.graphs) {
      if (item.objective) expect(container.textContent).not.toContain(item.objective);
    }
    expect(subscribed.textContent).toContain('订阅中 · 09:00 后');
    expect(container.querySelector('[aria-label="工作流调用"]')).toBeNull();
    expect(container.querySelector('.relation-graphs')!.textContent).not.toContain('unrelated');
    expect(container.querySelector('.relation-graphs')!.textContent).not.toContain('vip');
    expect(api.mock.calls).toEqual([['/graph-relations']]);
    const target = [...created.querySelectorAll<HTMLButtonElement>('button')].find(b => b.title === 'task-hash')!;
    await act(async () => target.click());
    expect(open).toHaveBeenCalledWith('task-hash');
  });

  it('shows shared consumers and authorization state and searches readable labels or exact IDs', async () => {
    await render('briefing');
    const subscriptions = () => container.querySelector('[aria-label="结果订阅"]');
    expect(subscriptions()!.querySelectorAll('li')).toHaveLength(2);
    expect(subscriptions()!.textContent).toContain('授权失效 · 09:05 后');
    await changeSearch('VIP 助手');
    expect(subscriptions()!.querySelectorAll('li')).toHaveLength(1);
    await changeSearch('schedule-1');
    expect(subscriptions()!.querySelectorAll('li')).toHaveLength(2);
    await changeSearch('not-found');
    expect(subscriptions()).toBeNull();
    expect(container.textContent).toContain('当前范围没有匹配关系');
  });

  it('keeps static call navigation intact alongside all relationship types', async () => {
    await render();
    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
    expect(container.querySelectorAll('.relationship-section')).toHaveLength(3);
    const locate = container.querySelector<HTMLButtonElement>('[aria-label="定位 recovery 的调用节点 send"]')!;
    expect(locate.textContent).toContain('等待完成');
    await act(async () => locate.click());
    expect(open).toHaveBeenCalledWith('recovery', 'send');
    await render('recovery');
    expect(container.querySelector('select')!.value).toBe('recovery');
  });

  it('keeps legacy responses and missing call targets honest, with retry after read errors', async () => {
    api.mockRejectedValueOnce(new Error('暂时不可用')).mockResolvedValue({
      graphs: [{ graph: 'source', schedules: 0 }],
      calls: [{ graph: 'source', target: 'missing', node: 'invoke', op: 'invoke', mode: 'detach' }],
    });
    await render('source');
    expect(container.querySelector('[role="alert"]')!.textContent).toContain('暂时不可用');
    await act(async () => container.querySelector<HTMLButtonElement>('[role="alert"] button')!.click());
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(container.querySelector<HTMLButtonElement>('button[title="missing"]')!.disabled).toBe(true);
    expect(container.textContent).toContain('启动后继续');
    expect(container.querySelector('[aria-label="结果订阅"]')).toBeNull();
  });
});
