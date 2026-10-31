/** @vitest-environment jsdom */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphBot, PluginBots } from './GraphBot';

vi.mock('./PluginCredentials', () => ({ PluginCredentials: ({ pluginId, endpoint }: { pluginId: string; endpoint: string }) => <div data-testid="credentials">{pluginId} {endpoint}</div> }));
const response = (status: number, data: unknown) => ({ ok: status >= 200 && status < 300, status, json: async () => data }) as Response;

describe('a Graph bot', () => {
  let container: HTMLDivElement;
  let root: Root;
  let fetchMock: ReturnType<typeof vi.fn>;
  let status: Record<string, unknown>;
  const requests = () => fetchMock.mock.calls.map(([path, init]) => ({ path, method: init?.method ?? 'GET', body: init?.body ? JSON.parse(init.body) : undefined }));
  const button = (name: string) => [...container.querySelectorAll<HTMLButtonElement>('button')].find(item => item.textContent === name)!;
  const render = async (node: React.ReactNode) => { await act(async () => root.render(node)); };
  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement('div'); document.body.appendChild(container); root = createRoot(container);
    status = { graph: 'helper', configured: true, owner_matches: true, mutable: true, enabled: false, listener: 'stopped',
      binding: { plugin: 'wecom', users: ['alice'], send_users: [] } };
    fetchMock = vi.fn(async (path: string, init: RequestInit = {}) => {
      if (path === '/graphs/helper/channel') {
        if (init.method === 'PUT') status = { ...status, binding: { plugin: 'wecom', ...JSON.parse(String(init.body)) } };
        return response(200, status);
      }
      if (path === '/graphs/helper/channel/enabled') { status = { ...status, enabled: JSON.parse(String(init.body)).enabled, listener: 'authenticated' }; return response(200, status); }
      if (path === '/channels') return response(200, { channels: [{ graph: 'helper', enabled: true, listener: 'authenticated', binding: { plugin: 'wecom' } }, { graph: 'other', binding: { plugin: 'feishu' } }] });
      return response(500, { error: `unexpected ${init.method ?? 'GET'} ${path}` });
    });
    vi.stubGlobal('fetch', fetchMock);
  });
  afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

  it('asks to save the Graph before reading an unsaved draft', async () => {
    await render(<GraphBot graph="helper" pluginId="wecom" saved={false} />);
    expect(container.textContent).toContain('请先保存 Graph');
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('saves members, uses this Graph\'s credentials and enables with the saved binding', async () => {
    await render(<GraphBot graph="helper" pluginId="wecom" saved />);
    expect(container.querySelector('[data-testid="credentials"]')?.textContent).toBe('wecom /graphs/helper/channel/credentials');
    const send = container.querySelectorAll('textarea')[1];
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(send, 'alice, bob\nbob');
      send.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => button('保存成员').click());
    await act(async () => button('启用机器人').click());
    expect(container.textContent).toContain('常驻监听');
    expect(container.querySelector('fieldset')?.disabled).toBe(true);
    expect(container.querySelector('[data-testid="credentials"]')).toBeNull();
    expect(container.textContent).toContain('停用后才能修改凭据');
    const writes = requests().filter(request => request.method !== 'GET');
    expect(writes).toEqual([
      { path: '/graphs/helper/channel', method: 'PUT', body: { users: ['alice'], send_users: ['alice', 'bob'] } },
      { path: '/graphs/helper/channel/enabled', method: 'POST', body: { enabled: true, expected_binding: { plugin: 'wecom', users: ['alice'], send_users: ['alice', 'bob'] } } },
    ]);
  });

  it('cannot be enabled before members are saved', async () => {
    status = { graph: 'helper', configured: false, enabled: false, listener: 'stopped', mutable: true };
    await render(<GraphBot graph="helper" pluginId="wecom" saved />);
    expect(button('启用机器人').disabled).toBe(true);
  });

  it('repairs a failed enabled gateway without disabling the bot or changing its binding', async () => {
    status = { ...status, enabled: true, available: true, listener: 'unavailable', restart_required: true };
    const binding = status.binding;
    await render(<GraphBot graph="helper" pluginId="wecom" saved />);
    expect(container.textContent).toContain('网关需要重新启动');
    expect(container.textContent).not.toContain('账户身份已变更');
    await act(async () => button('恢复连接').click());
    expect(requests().filter(request => request.method !== 'GET')).toEqual([
      { path: '/graphs/helper/channel/enabled', method: 'POST', body: { enabled: true, expected_binding: binding } },
    ]);
    expect(container.textContent).toContain('常驻监听');
  });

  it('lists only the Graphs using this Plugin from the library', async () => {
    await render(<PluginBots pluginId="wecom" />);
    expect(container.textContent).toContain('helper');
    expect(container.textContent).not.toContain('other');
  });
});
