/** @vitest-environment jsdom */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { PluginCredentials } from './PluginCredentials';
import { PluginInstall, PluginUninstall, type PluginReview } from './PluginLifecycle';

const response = (status: number, value: unknown) => ({ ok: status >= 200 && status < 300, status, json: async () => value }) as Response;
const metadata = (state = 'configured') => ({ plugin: 'demo', targets: [
  { target: 'api', kind: 'mcp', authorization_digest: 'digest', fields: [{ field: 'API_KEY', state, editable: true }] },
  { target: 'channel_wecom', kind: 'channel', authorization_digest: 'channel-digest', fields: [
    { field: 'BOT_TOKEN', state: 'stale', editable: true }, { field: 'DEPLOYMENT_KEY', state: 'deployment', editable: false }] },
] });

describe('standalone plugin management', () => {
  let container: HTMLDivElement; let root: Root; let fetchMock: ReturnType<typeof vi.fn>;
  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement('div'); document.body.appendChild(container); root = createRoot(container);
    Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value() { this.open = true; } });
    Object.defineProperty(HTMLDialogElement.prototype, 'close', { configurable: true, value() { this.open = false; } });
    fetchMock = vi.fn(); vi.stubGlobal('fetch', fetchMock);
  });
  afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
  const click = async (text: string) => { const button = [...container.querySelectorAll('button')].find(item => item.textContent === text)!;
    expect(button, text).toBeTruthy(); await act(async () => button.click()); };

  it('filters channel credentials and distinguishes stale and external configuration', async () => {
    fetchMock.mockResolvedValue(response(200, metadata()));
    await act(async () => root.render(<PluginCredentials pluginId="demo" target="channel_wecom" />));
    expect(container.textContent).toContain('需重新配置'); expect(container.textContent).toContain('由部署管理');
    expect(container.textContent).not.toContain('API_KEY');
    expect(container.textContent).toContain('设置 BOT_TOKEN');
    expect(container.textContent).not.toContain('设置 DEPLOYMENT_KEY');
    expect(container.textContent).toContain('不验证密钥是否有效');
  });

  it('clears only after confirmation and updates current state without a Pilot request', async () => {
    const changed = vi.fn(); fetchMock.mockResolvedValueOnce(response(200, metadata()));
    await act(async () => root.render(<PluginCredentials pluginId="demo" onChanged={changed} />));
    await click('清除 API_KEY');
    expect(fetchMock).toHaveBeenCalledTimes(1);
    fetchMock.mockResolvedValueOnce(response(200, metadata('missing')));
    await click('确认清除');
    expect(fetchMock.mock.calls.at(-1)?.[0]).toBe('/plugins/demo/credentials/api/API_KEY');
    expect(fetchMock.mock.calls.at(-1)?.[1]).toMatchObject({ method: 'DELETE', body: JSON.stringify({ authorization_digest: 'digest' }) });
    expect(container.textContent).toContain('已清除 API_KEY'); expect(container.textContent).toContain('设置 API_KEY');
    expect(changed).toHaveBeenCalledOnce();
    expect(fetchMock.mock.calls.every(call => !String(call[0]).includes('/sessions/'))).toBe(true);
  });

  it('saves through the shared endpoint and clears the ephemeral input', async () => {
    sessionStorage.clear(); localStorage.clear();
    fetchMock.mockResolvedValueOnce(response(200, metadata('missing')));
    await act(async () => root.render(<PluginCredentials pluginId="demo" />));
    await click('设置 API_KEY');
    const input = container.querySelector<HTMLInputElement>('input[type="password"]')!;
    input.value = 'private-ephemeral-value';
    fetchMock.mockResolvedValueOnce(response(200, metadata('configured')));
    await act(async () => container.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })));
    expect(fetchMock.mock.calls.at(-1)?.[1]).toMatchObject({ method: 'PUT', body: JSON.stringify({ authorization_digest: 'digest', value: 'private-ephemeral-value' }) });
    expect(container.textContent).toContain('已保存 API_KEY'); expect(container.innerHTML).not.toContain('private-ephemeral-value');
    expect(localStorage.length).toBe(0); expect(sessionStorage.length).toBe(0);
  });

  it('configures dynamic Feishu app fields through this Graph\'s credentials endpoint', async () => {
    const endpoint = '/graphs/feishu-assistant/channel/credentials';
    const data = { plugin: 'chat-gateway', targets: [{ target: 'channel_feishu@feishu-assistant', kind: 'channel', authorization_digest: 'feishu-digest',
      fields: ['FEISHU_APP_ID', 'FEISHU_APP_SECRET'].map(field => ({ field, state: 'missing', editable: true })) }] };
    fetchMock.mockResolvedValueOnce(response(200, data));
    await act(async () => root.render(<PluginCredentials pluginId="chat-gateway" endpoint={endpoint} />));
    expect(container.textContent).toContain('channel_feishu@feishu-assistant');
    expect(container.textContent).toContain('设置 FEISHU_APP_ID');
    expect(container.textContent).toContain('设置 FEISHU_APP_SECRET');
    expect(container.textContent).not.toContain('WECOM');
    for (const field of ['FEISHU_APP_ID', 'FEISHU_APP_SECRET']) {
      await click(`设置 ${field}`);
      const input = container.querySelector<HTMLInputElement>('input[type="password"]')!;
      input.value = `ephemeral-${field}`;
      data.targets[0].fields.find(item => item.field === field)!.state = 'configured';
      fetchMock.mockResolvedValueOnce(response(200, data));
      await act(async () => container.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })));
      expect(fetchMock.mock.calls.at(-1)?.[0]).toBe(`${endpoint}/${field}`);
      expect(fetchMock.mock.calls.at(-1)?.[1]).toMatchObject({ method: 'PUT', body: JSON.stringify({ authorization_digest: 'feishu-digest', value: `ephemeral-${field}` }) });
      expect(input.value).toBe('');
      expect(container.textContent).toContain(`轮换 ${field}`);
    }
  });


  it('protects a legacy draft from focus refresh and clears detached inputs on target switch', async () => {
    fetchMock.mockResolvedValue(response(200, metadata()));
    await act(async () => root.render(<PluginCredentials pluginId="demo" />));
    await click('轮换 API_KEY');
    const input = container.querySelector<HTMLInputElement>('input[type="password"]')!;
    input.value = 'legacy-draft';
    const calls = fetchMock.mock.calls.length;
    await act(async () => window.dispatchEvent(new Event('focus')));
    expect(fetchMock).toHaveBeenCalledTimes(calls); expect(input.value).toBe('legacy-draft');
    await act(async () => root.render(<PluginCredentials pluginId="demo" target="channel_wecom" />));
    expect(input.value).toBe('');
    expect(container.textContent).not.toContain('API_KEY');
    expect(container.textContent).toContain('设置 BOT_TOKEN');
  });

  it('aborts legacy saves on unmount and ignores their delayed results', async () => {
    const changed = vi.fn(); fetchMock.mockResolvedValueOnce(response(200, metadata()));
    await act(async () => root.render(<PluginCredentials pluginId="demo" onChanged={changed} />));
    await click('轮换 API_KEY');
    const input = container.querySelector<HTMLInputElement>('input[type="password"]')!;
    input.value = 'legacy-rotation';
    let complete: (value: Response) => void = () => undefined;
    fetchMock.mockImplementationOnce(() => new Promise<Response>(resolve => { complete = resolve; }));
    await act(async () => container.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })));
    const signal = fetchMock.mock.calls.at(-1)![1].signal as AbortSignal;
    expect(input.value).toBe('');
    await act(async () => root.render(<p>已离开</p>));
    expect(signal.aborted).toBe(true);
    await act(async () => complete(response(200, metadata())));
    expect(changed).not.toHaveBeenCalled(); expect(container.textContent).toBe('已离开');
  });

  it('clears a rejected legacy value and rereads metadata without echoing the server error', async () => {
    fetchMock.mockResolvedValueOnce(response(200, metadata()));
    await act(async () => root.render(<PluginCredentials pluginId="demo" />));
    await click('轮换 API_KEY');
    const input = container.querySelector<HTMLInputElement>('input[type="password"]')!;
    input.value = 'rejected-legacy-secret';
    fetchMock.mockResolvedValueOnce(response(409, { error: 'bad value rejected-legacy-secret' }));
    fetchMock.mockResolvedValueOnce(response(200, metadata('missing')));
    await act(async () => container.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })));
    expect(input.value).toBe(''); expect(container.textContent).toContain('凭据未保存');
    expect(container.textContent).not.toContain('rejected-legacy-secret');
    expect(container.textContent).toContain('设置 API_KEY');
  });

  it('requires both uninstall confirmations and surfaces the existing holder refusal', async () => {
    const review = { token: 'review-token', operation: 'uninstall', plugin: 'demo', name: '示例', digest: 'digest',
      expires_at: 9999999999, graphs: ['research'], coverage: { kind: 'complete' }, mutable: [{ path: 'data/', bytes: null }] };
    fetchMock.mockResolvedValueOnce(response(200, review));
    await act(async () => root.render(<PluginUninstall pluginId="demo" />));
    await click('查看卸载影响');
    expect(container.textContent).toContain('research'); expect(container.textContent).toContain('体积未知');
    const button = [...container.querySelectorAll('button')].find(item => item.textContent === '确认卸载')!;
    expect(button.disabled).toBe(true);
    const boxes = container.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
    await act(async () => boxes[0].click()); expect(button.disabled).toBe(true);
    await act(async () => boxes[1].click()); expect(button.disabled).toBe(false);
    fetchMock.mockResolvedValueOnce(response(409, { error: '还有会话挂着它；请重新检查' }));
    await click('确认卸载');
    expect(container.textContent).toContain('还有会话挂着它');
    expect(fetchMock.mock.calls.at(-1)?.[1]).toMatchObject({ method: 'POST', body: JSON.stringify({ confirm: true, confirm_data_loss: true }) });
  });

  it('observes background publication before reporting completion without confirming twice', async () => {
    vi.useFakeTimers();
    const changed = vi.fn(); const close = vi.fn();
    const review: PluginReview = { token: 'review-token', operation: 'install', plugin: 'demo', name: '示例', digest: 'digest',
      state: 'ready', expires_at: 9999999999, graphs: [], coverage: { kind: 'complete' }, servers: [], resources: [] };
    await act(async () => root.render(<PluginInstall initialReview={review} pluginId="demo" onChanged={changed} close={close} />));
    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
    fetchMock.mockResolvedValueOnce(response(200, { ...review, state: 'running' }));
    fetchMock.mockResolvedValueOnce(response(200, { ...review, state: 'succeeded', result: { installed: true, digest: 'digest' } }));
    await click('确认安装插件');
    expect(container.textContent).toContain('正在发布插件'); expect(changed).not.toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(700); });
    expect(changed).toHaveBeenCalledOnce(); expect(close).toHaveBeenCalledOnce();
    expect(fetchMock.mock.calls.map(call => call[1].method)).toEqual(['POST', 'GET']);
  });
});
