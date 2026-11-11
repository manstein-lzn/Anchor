/** @vitest-environment jsdom */

/** The session plugin panel writes to the Host and reads it again on a timer.
 *
 * Two of these behaviours are the ones a person would notice if they were wrong: a required Plugin
 * must not move, and a refused switch must show the Host's own sentence while the row stays where
 * the Host left it. The third is the point of the panel — Pilot installs a Plugin mid-session and it
 * has to appear without a page reload.
 */

import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { maySwitch, POLL_MS, SessionPlugins, type SessionPlugin } from './SessionPlugins';

const plugin = (fields: Partial<SessionPlugin> & { id: string }): SessionPlugin => ({
  name: fields.id, description: '', skills: [], mcpServerNames: [], required: false, on: false,
  available: true, error: null, ...fields,
});
const response = (status: number, body: unknown) => ({
  ok: status >= 200 && status < 300, status, json: async () => body,
}) as unknown as Response;

describe('the session plugin panel', () => {
  let container: HTMLDivElement;
  let root: Root;
  let fetchMock: ReturnType<typeof vi.fn>;
  beforeEach(() => {
    // `act` needs to be told this is a test renderer; without it React flushes state updates late.
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    // jsdom arrives without the timer helpers `api()` composes; the test only needs a signal.
    if (typeof AbortSignal.timeout !== 'function') {
      Object.defineProperty(AbortSignal, 'timeout', { value: () => new AbortController().signal });
    }
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
    fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    vi.useFakeTimers();
  });
  afterEach(async () => {
    await act(async () => { root.unmount(); });
    container.remove();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });
  const draw = async () => { await act(async () => { root.render(<SessionPlugins session="s-1" />); }); };
  const switches = () => [...container.querySelectorAll<HTMLButtonElement>('.session-plugin-switch')];
  const posts = () => fetchMock.mock.calls.filter(call => (call[1] as RequestInit | undefined)?.method === 'POST');

  it('polls within three to five seconds', () => {
    expect(POLL_MS).toBeGreaterThanOrEqual(3000);
    expect(POLL_MS).toBeLessThanOrEqual(5000);
  });

  it('prefetches once while collapsed, shares that read across opening and closing, and retains its result', async () => {
    let finish: (value: Response) => void = () => undefined;
    fetchMock.mockImplementationOnce(() => new Promise<Response>(resolve => { finish = resolve; }));
    await act(async () => { root.render(<SessionPlugins session="s-1" defaultOpen={false} />); });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(switches()).toHaveLength(0);
    await act(async () => { await vi.advanceTimersByTimeAsync(POLL_MS * 3); });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const togglePanel = () => container.querySelector<HTMLButtonElement>('.pilot-plugins-heading button')!.click();
    await act(async () => { togglePanel(); });
    expect(container.querySelector('[role="status"]')?.textContent).toBe('正在加载插件…');
    expect(container.querySelector('.pilot-plugins-body')?.getAttribute('aria-busy')).toBe('true');
    expect(container.querySelectorAll('.session-plugin-placeholder')).toHaveLength(3);
    expect(container.textContent).not.toContain('还没有安装插件');
    await act(async () => { await vi.advanceTimersByTimeAsync(POLL_MS * 2); });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    await act(async () => { togglePanel(); });
    await act(async () => { finish(response(200, { plugins: [plugin({ id: 'ready', name: '已准备的插件', on: true })] })); });
    expect(container.querySelector('.plugin-trigger-count')?.textContent).toBe('1');
    await act(async () => { await vi.advanceTimersByTimeAsync(POLL_MS * 2); });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    fetchMock.mockImplementationOnce(() => new Promise<Response>(() => undefined));
    await act(async () => { togglePanel(); });
    expect(container.textContent).toContain('已准备的插件');
    expect(container.querySelector('.session-plugins-loading')).toBeNull();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('cancels a departed session read and stays loading until the new session answers', async () => {
    let old: (value: Response) => void = () => undefined;
    let next: (value: Response) => void = () => undefined;
    fetchMock.mockImplementationOnce(() => new Promise<Response>(resolve => { old = resolve; }));
    await draw();
    const signal = (fetchMock.mock.calls[0][1] as RequestInit).signal!;
    fetchMock.mockImplementationOnce(() => new Promise<Response>(resolve => { next = resolve; }));
    await act(async () => { root.render(<SessionPlugins session="s-2" />); });
    expect(signal.aborted).toBe(true);
    await act(async () => { old(response(200, { plugins: [plugin({ id: 'old' })] })); });
    expect(switches()).toHaveLength(0);
    expect(container.querySelector('[role="status"]')?.textContent).toBe('正在加载插件…');
    expect(container.textContent).not.toContain('还没有安装插件');
    await act(async () => { next(response(200, { plugins: [] })); });
    expect(container.textContent).toContain('还没有安装插件');
    expect(container.querySelector('.session-plugins-loading')).toBeNull();
  });

  it('shows loading again during an explicit retry and only shows empty after a confirmed empty response', async () => {
    let finish: (value: Response) => void = () => undefined;
    fetchMock.mockResolvedValueOnce(response(503, { error: '插件库暂时不可读' }));
    await draw();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain('插件库暂时不可读');
    fetchMock.mockImplementationOnce(() => new Promise<Response>(resolve => { finish = resolve; }));
    const retry = [...container.querySelectorAll('button')].find(button => button.textContent === '重试')!;
    await act(async () => { retry.click(); });
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(container.querySelector('[role="status"]')?.textContent).toBe('正在加载插件…');
    expect(container.textContent).not.toContain('还没有安装插件');
    await act(async () => { finish(response(200, { plugins: [] })); });
    expect(container.textContent).toContain('还没有安装插件');
  });

  it('uses one network policy control and applies denial to every row without an ordinary mount write', async () => {
    const networking = plugin({ id: 'network', name: '联网', on: true, explicitOn: false,
      networkProvider: true, networkAllowed: true, networkMode: 'auto', enabledBy: ['research'] });
    const research = plugin({ id: 'research', name: '学术调研', on: true, requiresNetwork: true, networkAllowed: true });
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [networking, research] }));
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [
      { ...networking, networkAllowed: false, networkMode: 'denied', available: false, error: '当前任务已禁止联网' },
      { ...research, networkAllowed: false, available: false, error: '该插件需要联网：当前任务已禁止联网' },
    ] }));
    await draw();
    expect(container.textContent).toContain('由学术调研依赖启用');
    expect(switches()).toHaveLength(1);
    expect(container.querySelector('[role="switch"][aria-label$=" 联网"]')).toBeNull();
    expect(container.textContent).toContain('2 已挂载');
    const policy = container.querySelector<HTMLSelectElement>('.plugin-network-policy select')!;
    expect(policy.value).toBe('auto');
    await act(async () => { policy.value = 'denied'; policy.dispatchEvent(new Event('change', { bubbles: true })); });
    expect(posts()[0][0]).toBe('/sessions/s-1/network');
    expect(posts()[0][1]).toMatchObject({ body: JSON.stringify({ mode: 'denied' }) });
    expect(posts()).toHaveLength(1);
    expect(container.textContent).toContain('已禁止联网');
    expect(container.textContent).toContain('该插件需要联网：当前任务已禁止联网');
    expect(policy.value).toBe('denied');
    expect(policy.disabled).toBe(false);
    expect(container.textContent).toContain('插件依赖不能重新启用');
    expect(container.textContent).toContain('已保存，下次发消息时使用新设置');
  });

  it('renders a required Plugin as a switch that cannot move, and lets no switch past the guard', async () => {
    fetchMock.mockResolvedValue(response(200, { plugins: [plugin({
      id: 'core', name: '核心能力', description: '图里预置的能力', skills: ['搜索'],
      mcpServerNames: ['search', 'fetch'], required: true, on: true,
    })] }));
    await draw();
    const [toggle] = switches();
    expect(container.textContent).toContain('核心能力');
    expect(container.querySelector('.plugin-card-name')?.textContent).toBe('核心能力');
    expect(container.textContent).toContain('必需');
    expect(container.textContent).toContain('工作流需要此插件，保持挂载。');
    expect(container.querySelector('button[aria-label="查看 核心能力 的详情"]')).not.toBeNull();
    expect(container.querySelector('.plugin-card-description')?.textContent).toBe('图里预置的能力');
    // The disabled attribute is what makes it unclickable in a browser; jsdom would not deliver a
    // click to it anyway, so the guard that decides is asserted directly instead of through a click.
    expect(toggle.disabled).toBe(true);
    expect(toggle.getAttribute('aria-checked')).toBe('true');
    expect(maySwitch(plugin({ id: 'core', required: true }), false)).toBe(false);
    expect(maySwitch(plugin({ id: 'core' }), false)).toBe(true);
    expect(maySwitch(plugin({ id: 'core' }), true)).toBe(false);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(posts()).toEqual([]);
  });

  it('does not infer network permission from effective access when the policy field is missing', async () => {
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [plugin({ id: 'network', name: '联网',
      networkProvider: true, networkAllowed: true, on: true, explicitOn: true })] }));
    await draw();
    const policy = container.querySelector<HTMLSelectElement>('.plugin-network-policy select')!;
    expect(policy.selectedOptions[0].textContent).toBe('联网策略未知');
    expect(policy.disabled).toBe(true);
    expect(container.textContent).not.toContain('已禁止联网');
    await act(async () => { policy.click(); });
    expect(posts()).toHaveLength(0);
    expect(switches()).toHaveLength(0);
  });

  it('retains the confirmed network mode after a failed save and never schedules a continuation', async () => {
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [plugin({ id: 'network', name: '联网',
      networkProvider: true, networkMode: 'denied', networkAllowed: false })] }));
    fetchMock.mockResolvedValueOnce(response(503, { error: '联网策略暂时无法保存' }));
    const onContinuation = vi.fn();
    await act(async () => root.render(<SessionPlugins session="s-1" onContinuation={onContinuation} />));
    const policy = container.querySelector<HTMLSelectElement>('.plugin-network-policy select')!;
    await act(async () => { policy.value = 'auto'; policy.dispatchEvent(new Event('change', { bubbles: true })); });
    expect(policy.value).toBe('denied');
    expect(container.textContent).toContain('联网策略暂时无法保存');
    expect(onContinuation).not.toHaveBeenCalled();
  });

  it('sends one switch at a time while the first is still waiting on the Host', async () => {
    let release: (value: Response) => void = () => undefined;
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [plugin({ id: 'alpha', name: '阿尔法' }), plugin({ id: 'beta', name: '贝塔' })] }));
    fetchMock.mockImplementationOnce(() => new Promise<Response>(resolve => { release = resolve; }));
    await draw();
    const [alpha, beta] = switches();
    await act(async () => { alpha.click(); });
    expect(posts()).toHaveLength(1);
    // The second row is still enabled (only the row being written is), so this click really reaches
    // the handler — and the in-flight guard is the only thing that can stop the second POST.
    expect(beta.disabled).toBe(false);
    await act(async () => { beta.click(); });
    expect(posts()).toHaveLength(1);
    await act(async () => { release(response(200, { plugin: plugin({ id: 'alpha', name: '阿尔法', on: true }) })); });
    expect(switches()[0].getAttribute('aria-checked')).toBe('true');
    expect(switches()[1].getAttribute('aria-checked')).toBe('false');
  });

  it('keeps the row where the Host left it and shows the refused switch\'s own words', async () => {
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [plugin({ id: 'demo', name: '示例插件' })] }));
    fetchMock.mockResolvedValueOnce(response(400, { error: '该插件在图里被声明为必需（required: true）' }));
    await draw();
    const [toggle] = switches();
    expect(toggle.getAttribute('aria-checked')).toBe('false');
    await act(async () => { toggle.click(); });
    expect(posts()).toHaveLength(1);
    expect(posts()[0][0]).toBe('/sessions/s-1/plugins/demo');
    expect(posts()[0][1]).toMatchObject({ method: 'POST', body: JSON.stringify({ on: true }) });
    expect(container.textContent).toContain('该插件在图里被声明为必需（required: true）');
    expect(switches()[0].getAttribute('aria-checked')).toBe('false');
    expect(container.querySelector('.plugin-card-status')?.textContent).toBe('未挂载');
    expect(container.textContent).not.toContain('已切换');
  });

  it('saves an idle switch from the Host without requesting continuation', async () => {
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [plugin({ id: 'demo', name: '示例插件' })] }));
    fetchMock.mockResolvedValueOnce(response(200, { plugin: plugin({ id: 'demo', name: '示例插件', on: true }) }));
    await draw();
    const [toggle] = switches();
    await act(async () => { toggle.click(); });
    expect(switches()[0].getAttribute('aria-checked')).toBe('true');
    expect(container.querySelector('.plugin-card-status')?.textContent).toBe('已挂载');
    expect(container.textContent).toContain('已保存，下次发消息时使用新设置');
    expect(container.querySelector('.pilot-plugins-intro')).toBeNull();
    expect(container.textContent).not.toContain('当前回合保持原有能力');
    expect(container.textContent).not.toContain('已生效');
  });

  it('follows the Host scheduled source and displays admission and cancellation truthfully', async () => {
    const onContinuation = vi.fn();
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [plugin({ id: 'demo' })] }));
    fetchMock.mockResolvedValueOnce(response(200, { plugin: plugin({ id: 'demo', on: true }),
      effect: { status: 'after_current_turn', source_turn: 'source' },
      continuation: { status: 'scheduled', request_id: 'continuation:source' } }));
    await act(async () => { root.render(<SessionPlugins session="s-1" onContinuation={onContinuation} />); });
    await act(async () => { switches()[0].click(); });
    expect(onContinuation).toHaveBeenCalledExactlyOnceWith('s-1', 'source');
    expect(container.textContent).toContain('Pilot 会先完成当前回复，再使用新设置继续处理你的请求。');
    await act(async () => { root.render(<SessionPlugins session="s-1" onContinuation={onContinuation}
      continuation={{ session: 's-1', source: 'source', state: { status: 'admitted', turn: 'next' } }} />); });
    expect(container.textContent).toContain('Pilot 已开始使用新设置继续处理你的请求。');
    await act(async () => { root.render(<SessionPlugins session="s-1" onContinuation={onContinuation}
      continuation={{ session: 's-1', source: 'source', state: { status: 'cancelled' } }} />); });
    expect(container.textContent).toContain('已保存。本次未继续，下次发消息时使用新设置。');
    for (const state of [
      { status: 'pending' as const, notice: '已保存。本次未继续，下次发消息时使用新设置。' },
      { status: 'superseded' as const, notice: '已保存，Pilot 会在新的请求中使用新设置。' },
    ]) {
      await act(async () => { root.render(<SessionPlugins session="s-1" onContinuation={onContinuation}
        continuation={{ session: 's-1', source: 'source', state: { status: state.status } }} />); });
      expect(container.textContent).toContain(state.notice);
    }
    expect(onContinuation).toHaveBeenCalledTimes(1);
  });

  it('does not schedule an unchanged switch or a late response from a departed Session', async () => {
    const onContinuation = vi.fn();
    let release: (value: Response) => void = () => undefined;
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [plugin({ id: 'demo' })] }));
    fetchMock.mockImplementationOnce(() => new Promise<Response>(resolve => { release = resolve; }));
    await act(async () => { root.render(<SessionPlugins key="s-1" session="s-1" onContinuation={onContinuation} />); });
    await act(async () => { switches()[0].click(); });
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [plugin({ id: 'demo' })] }));
    await act(async () => { root.render(<SessionPlugins key="s-2" session="s-2" onContinuation={onContinuation} />); });
    await act(async () => { release(response(200, { plugin: plugin({ id: 'demo', on: true }),
      effect: { status: 'after_current_turn', source_turn: 'old-source' }, continuation: { status: 'scheduled' } })); });
    expect(onContinuation).not.toHaveBeenCalled();
    expect(switches()[0].getAttribute('aria-checked')).toBe('false');
    fetchMock.mockResolvedValueOnce(response(200, { plugin: plugin({ id: 'demo', on: true }),
      effect: { status: 'unchanged' }, continuation: { status: 'none' } }));
    await act(async () => { switches()[0].click(); });
    expect(onContinuation).not.toHaveBeenCalled();
    expect(container.textContent).toContain('设置未改变');
  });

  it('shows a Plugin Pilot just installed after one poll, and an unavailable one with its reason', async () => {
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [plugin({ id: 'alpha', name: '阿尔法' })] }));
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [
      plugin({ id: 'alpha', name: '阿尔法' }),
      plugin({ id: 'beta', name: '贝塔', available: false, error: '技能目录视图不可用' }),
    ] }));
    await draw();
    expect(container.textContent).not.toContain('贝塔');
    await act(async () => { await vi.advanceTimersByTimeAsync(POLL_MS); });
    expect(container.textContent).toContain('贝塔');
    expect(container.textContent).toContain('不可用：技能目录视图不可用');
    expect(switches()).toHaveLength(2);
  });

  it('keeps unavailable mounted plugins removable and prevents attaching broken definitions', async () => {
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [
      plugin({ id: 'mounted', on: true, available: false, error: '定义无法读取' }),
      plugin({ id: 'broken', available: false, error: '入口缺失' }),
    ] }));
    await draw();
    expect(switches()[0].disabled).toBe(false);
    expect(switches()[1].disabled).toBe(true);
    expect(container.querySelectorAll('.plugin-card-details')).toHaveLength(2);
    expect(maySwitch(plugin({ id: 'bad', on: true, available: false }), false)).toBe(true);
    expect(maySwitch(plugin({ id: 'bad', available: false }), false)).toBe(false);
  });

  it('clears old facts on a session prop change and ignores its late POST even without a remount', async () => {
    let finishOld: (value: Response) => void = () => undefined;
    let finishNew: (value: Response) => void = () => undefined;
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [plugin({ id: 'old' })] }));
    fetchMock.mockImplementationOnce(() => new Promise<Response>(resolve => { finishOld = resolve; }));
    await draw();
    await act(async () => { switches()[0].click(); });
    fetchMock.mockImplementationOnce(() => new Promise<Response>(resolve => { finishNew = resolve; }));
    await act(async () => { root.render(<SessionPlugins session="s-2" />); });
    expect(switches()).toHaveLength(0);
    expect(container.textContent).not.toContain('old');
    await act(async () => { finishOld(response(200, { plugin: plugin({ id: 'old', on: true }) })); });
    expect(switches()).toHaveLength(0);
    await act(async () => { finishNew(response(200, { plugins: [plugin({ id: 'new' })] })); });
    expect(container.querySelector('.plugin-card-name')?.textContent).toBe('new');
    expect(switches()[0].getAttribute('aria-checked')).toBe('false');
  });

  it('preserves known rows after a failed poll and refuses an older GET overwriting a successful change', async () => {
    let finishPoll: (value: Response) => void = () => undefined;
    fetchMock.mockResolvedValueOnce(response(200, { plugins: [plugin({ id: 'demo' })] }));
    fetchMock.mockImplementationOnce(() => new Promise<Response>(resolve => { finishPoll = resolve; }));
    await draw();
    await act(async () => { await vi.advanceTimersByTimeAsync(POLL_MS); });
    fetchMock.mockResolvedValueOnce(response(200, { plugin: plugin({ id: 'demo', on: true }) }));
    await act(async () => { switches()[0].click(); });
    await act(async () => { finishPoll(response(200, { plugins: [plugin({ id: 'demo' })] })); });
    expect(switches()[0].getAttribute('aria-checked')).toBe('true');
    fetchMock.mockResolvedValueOnce(response(503, { error: '插件库暂时不可读' }));
    await act(async () => { await vi.advanceTimersByTimeAsync(POLL_MS); });
    expect(switches()).toHaveLength(1);
    expect(switches()[0].getAttribute('aria-checked')).toBe('true');
    expect(container.textContent).toContain('插件库暂时不可读');
  });

  it('stops polling once the panel is closed', async () => {
    fetchMock.mockResolvedValue(response(200, { plugins: [plugin({ id: 'alpha' })] }));
    await draw();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const close = [...container.querySelectorAll('button')].find(button => button.textContent === '收起');
    await act(async () => { close?.click(); });
    await act(async () => { await vi.advanceTimersByTimeAsync(POLL_MS * 3); });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
