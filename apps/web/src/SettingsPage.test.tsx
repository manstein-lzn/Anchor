/** @vitest-environment jsdom */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SettingsPage } from './SettingsPage';

const fields = { endpoint: 'https://provider.example/v1', wire_api: 'chat', model: 'example-model', context_window: null, max_tokens: null, aliases: {} };
const providers = { default: { fields: { endpoint: fields.endpoint, wire_api: 'chat', flash: fields.model, pro: '' }, api_key_present: true, configured: false, source: 'legacy' }, backup: { fields: { endpoint: '', wire_api: 'responses', flash: '', pro: '' }, api_key_present: false, configured: false, source: 'empty' } };
const model = { providers, configured: true, fields, api_key_present: true, deployment_fields: [], retained_providers: [{ id: 'provider-1', endpoint: fields.endpoint, wire_api: 'chat', current: true, api_key_present: true, deployment: false }], notice: '原配置保持不变。' };
const runtime = { runtime: { python_bootstrap_available: true, python_prepare_available: true, rust_bootstrap_available: true, python_configuration_owner: 'host', rust_configuration_owner: 'host', python_default_version: '3.12', rust_default_toolchain: 'stable', python_configured: false, python_enabled: false, python_explicitly_disabled: false, rust_configured: false, rust_toolchain: null }, operation: null };
const operation = { kind: 'python', version: '3.12', status: 'running', phase: 'installing_python', started_at: '2026-10-14T10:00:00Z', updated_at: '2026-10-14T10:00:00Z', message: null };
const deployedPython = { ...runtime.runtime, python_version: '3.12.13', python_version_error: null as string | null, python_configuration_owner: 'deployment', python_configured: true, python_enabled: true, python_bootstrap_available: false };
const bootstrapError = 'the installed Anchor package has no native uv bootstrap tool';
const response = (body: unknown, status = 200) => ({ ok: status >= 200 && status < 300, status, json: async () => body }) as Response;

describe('Host settings', () => {
  let root: Root;
  let container: HTMLDivElement;
  let fetchMock: ReturnType<typeof vi.fn>;
  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement('div'); document.body.appendChild(container); root = createRoot(container);
    sessionStorage.clear(); localStorage.clear();
    fetchMock = vi.fn((path: string) => Promise.resolve(response(path === '/settings/model' ? model : runtime)));
    vi.stubGlobal('fetch', fetchMock);
  });
  afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.useRealTimers(); vi.unstubAllGlobals(); });
  const draw = async () => { await act(async () => root.render(<SettingsPage />)); };
  const button = (text: string) => [...container.querySelectorAll<HTMLButtonElement>('button')].find(item => item.textContent === text)!;
  const serveRuntime = (status: unknown) => fetchMock.mockImplementation((path: string) => Promise.resolve(response(path === '/settings/model' ? model : status)));
  const authorizeNetwork = async () => { await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click()); };
  it('loads and refreshes only model and runtime settings', async () => {
    await draw();
    const paths = () => fetchMock.mock.calls.map(([path]) => path).sort();
    expect(paths()).toEqual(['/settings/model', '/settings/runtime']);
    expect([...container.querySelectorAll('h2')].map(heading => heading.textContent)).toEqual(['模型服务', '开发环境']);
    fetchMock.mockClear();
    await act(async () => button('刷新状态').click());
    expect(paths()).toEqual(['/settings/model', '/settings/runtime']);
  });
  it('ignores a delayed refresh from before initialization and keeps polling the accepted operation', async () => {
    vi.useFakeTimers();
    let resolveRefresh!: (value: Response) => void;
    const delayedRefresh = new Promise<Response>(resolve => { resolveRefresh = resolve; });
    let runtimeReads = 0;
    fetchMock.mockImplementation((path: string) => {
      if (path === '/settings/model') return Promise.resolve(response(model));
      if (path === '/settings/runtime/prepare') return Promise.resolve(response({ ...runtime, operation }));
      runtimeReads += 1;
      if (runtimeReads === 2) return delayedRefresh;
      return Promise.resolve(response(runtimeReads === 1 ? runtime : { ...runtime, operation }));
    });
    await draw(); await authorizeNetwork();
    await act(async () => button('刷新状态').click());
    expect(runtimeReads).toBe(2);
    await act(async () => button('初始化 Python').click());
    expect(container.querySelector('progress')).not.toBeNull();
    await act(async () => resolveRefresh(response(runtime)));
    expect(container.querySelector('progress')).not.toBeNull();
    expect(button('初始化 Python').disabled).toBe(true);
    expect(button('刷新状态').disabled).toBe(false);
    await act(async () => { await vi.advanceTimersByTimeAsync(1500); });
    expect(runtimeReads).toBe(3);
    expect(container.textContent).toContain('下载并安装 Python');
  });
  it('initializes the Host runtime only after explicit network authorization and shows durable progress', async () => {
    await draw(); expect(button('初始化 Python').disabled).toBe(true);
    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
    expect(button('初始化 Python').disabled).toBe(false);
    fetchMock.mockImplementation((path: string) => Promise.resolve(response(path === '/settings/runtime/prepare' ? { ...runtime, operation } : path === '/settings/runtime/cancel' ? { ...runtime, operation: { ...operation, status: 'cancelling', phase: 'cancelling' } } : path === '/settings/model' ? model : { ...runtime, operation })));
    await act(async () => button('初始化 Python').click());
    const start = fetchMock.mock.calls.find(([path]) => path === '/settings/runtime/prepare');
    expect(JSON.parse((start?.[1] as RequestInit).body as string)).toEqual({ kind: 'python', version: '3.12', allow_network: true });
    expect(container.textContent).toContain('下载并安装 Python');
    expect(container.querySelector('progress')).not.toBeNull(); expect(button('初始化 Python').disabled).toBe(true);
    await act(async () => button('停止初始化').click()); expect(container.textContent).toContain('正在停止初始化');
    expect(fetchMock.mock.calls.some(([path]) => String(path).startsWith('/sessions'))).toBe(false);
  });
  it('accepts a slow poll without overlapping it or discarding its progress', async () => {
    vi.useFakeTimers();
    let resolvePoll!: (value: Response) => void;
    const delayedPoll = new Promise<Response>(resolve => { resolvePoll = resolve; });
    let runtimeReads = 0;
    fetchMock.mockImplementation((path: string) => {
      if (path === '/settings/model') return Promise.resolve(response(model));
      runtimeReads += 1;
      return runtimeReads === 2 ? delayedPoll : Promise.resolve(response({ ...runtime, operation }));
    });
    await draw();
    await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
    expect(runtimeReads).toBe(2);
    await act(async () => resolvePoll(response({ ...runtime, operation: { ...operation, phase: 'probing_python' } })));
    expect(container.textContent).toContain('验证 Python 解释器');
    expect(container.querySelector('progress')).not.toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(1500); });
    expect(runtimeReads).toBe(3);
  });
  it('shows interrupted initialization after reopening and leaves retry available', async () => {
    fetchMock.mockImplementation((path: string) => Promise.resolve(response(path === '/settings/model' ? model : { ...runtime, operation: { ...operation, status: 'interrupted', phase: 'interrupted', message: 'Host 重启中断了初始化。' } })));
    await draw(); expect(container.textContent).toContain('初始化被重启中断'); expect(container.textContent).toContain('Host 重启中断了初始化');
    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click()); expect(button('初始化 Python').disabled).toBe(false);
  });
  it.each(['failed', 'interrupted', 'cancelled'])('keeps deployed Python ready after %s but disables retry without preparation tools', async status => {
    serveRuntime({ runtime: { ...deployedPython, python_prepare_available: false }, operation: { ...operation, status, phase: status, message: bootstrapError } });
    await draw();
    expect(container.querySelector('.settings-runtime-row')!.textContent).toContain('已就绪 · 部署管理');
    expect(container.textContent).toContain('已有 Python 解释器仍可使用；此部署尚未配置可用的依赖准备工具');
    expect(container.textContent).toContain('此前初始化未能完成：部署未配置 Python 依赖准备工具。');
    expect(container.textContent).not.toContain('native uv bootstrap tool');
    expect(container.textContent).not.toContain('允许联网后，点击上方初始化按钮重试');
    const version = container.querySelector<HTMLOutputElement>('output[aria-label="Python 版本"]')!;
    expect(version.value).toBe('3.12.13');
    expect(container.querySelector('input[aria-label="Python 版本"]')).toBeNull();
    expect(container.textContent).toContain('当前 Python 由服务部署指定');
    await authorizeNetwork();
    expect(button('检查并初始化').disabled).toBe(true);
    await act(async () => button('检查并初始化').click());
    expect(fetchMock.mock.calls.some(([path]) => path === '/settings/runtime/prepare')).toBe(false);
  });
  it('allows complete deployment preparation without release bootstrap and keeps the historical failure visible', async () => {
    serveRuntime({ runtime: deployedPython, operation: { ...operation, status: 'failed', phase: 'failed', message: bootstrapError } });
    await draw();
    expect(container.textContent).toContain('Python · 初始化失败');
    expect(container.textContent).toContain('此前初始化未能完成：部署未配置 Python 依赖准备工具。');
    expect(container.textContent).toContain('允许联网后，点击上方初始化按钮重试');
    expect(container.textContent).not.toContain('此部署尚未配置可用的依赖准备工具');
    expect(button('检查并初始化').disabled).toBe(true);
    await authorizeNetwork(); expect(button('检查并初始化').disabled).toBe(false);
    await act(async () => button('检查并初始化').click());
    const start = fetchMock.mock.calls.find(([path]) => path === '/settings/runtime/prepare');
    expect(JSON.parse((start?.[1] as RequestInit).body as string)).toEqual({ kind: 'python', version: null, allow_network: true });
  });
  it('honors unavailable preparation even when release bootstrap exists', async () => {
    serveRuntime({ ...runtime, runtime: { ...runtime.runtime, python_prepare_available: false } });
    await draw(); await authorizeNetwork();
    expect(button('初始化 Python').disabled).toBe(true);
    expect(container.textContent).toContain('此部署尚未配置可用的 Python 初始化和依赖准备工具');
  });
  it('refreshes the detected version and clears stale observations when a probe fails', async () => {
    serveRuntime({ runtime: deployedPython, operation: null });
    await draw();
    const version = () => container.querySelector<HTMLOutputElement>('output[aria-label="Python 版本"]')!.value;
    expect(version()).toBe('3.12.13');
    serveRuntime({ runtime: { ...deployedPython, python_version: '3.13.9' }, operation: null });
    await act(async () => button('刷新状态').click());
    expect(version()).toBe('3.13.9');
    serveRuntime({ runtime: { ...deployedPython, python_version: null, python_version_error: 'probe timed out' }, operation: null });
    await act(async () => button('刷新状态').click());
    expect(version()).toBe('检测失败');
    expect(container.textContent).toContain('点击“刷新状态”可重新检测');
    expect(container.textContent).not.toContain('3.13.9');
  });
  it('preserves the last detected version while an initialization response omits observations', async () => {
    serveRuntime({ runtime: deployedPython, operation: null });
    await draw(); await authorizeNetwork();
    const { python_version: _version, python_version_error: _error, ...configuration } = deployedPython;
    serveRuntime({ runtime: configuration, operation });
    await act(async () => button('检查并初始化').click());
    expect(container.querySelector<HTMLOutputElement>('output[aria-label="Python 版本"]')!.value).toBe('3.12.13');
  });
  it('does not present the installation default as an observed version from an older Host', async () => {
    const { python_version: _version, python_version_error: _error, ...configuration } = deployedPython;
    serveRuntime({ runtime: configuration, operation: null });
    await draw();
    expect(container.querySelector<HTMLOutputElement>('output[aria-label="Python 版本"]')!.value).toBe('未检测到版本');
  });
  it.each([false, true])('uses verified bootstrap availability for an older Host with deployment Python: %s', async bootstrapAvailable => {
    const legacyFacts: Partial<typeof deployedPython> = { ...deployedPython, python_bootstrap_available: bootstrapAvailable };
    delete legacyFacts.python_prepare_available;
    serveRuntime({ runtime: legacyFacts, operation: { ...operation, status: 'cancelled', phase: 'cancelled' } });
    await draw(); await authorizeNetwork();
    expect(button('检查并初始化').disabled).toBe(!bootstrapAvailable);
    expect(container.textContent?.includes('允许联网后，点击上方初始化按钮重试')).toBe(bootstrapAvailable);
    expect(container.textContent?.includes('已有 Python 解释器仍可使用；此部署尚未配置可用的依赖准备工具')).toBe(!bootstrapAvailable);
  });
  it('keeps explicit deployment denial authoritative for the Python button and retry guidance', async () => {
    serveRuntime({ runtime: { ...deployedPython, python_prepare_available: true, python_explicitly_disabled: true }, operation: { ...operation, status: 'failed', phase: 'failed' } });
    await draw(); await authorizeNetwork();
    expect(button('检查并初始化').disabled).toBe(true);
    expect(container.textContent).toContain('部署已禁用 Python 初始化，请联系部署者调整配置');
    expect(container.textContent).not.toContain('允许联网后，点击上方初始化按钮重试');
    expect(container.textContent).not.toContain('已有 Python 解释器仍可使用');
  });
  it.each(['missing_bootstrap', 'deployment'])('does not offer Rust retry when preparation is unavailable: %s', async restriction => {
    serveRuntime({ runtime: { ...runtime.runtime, rust_bootstrap_available: restriction !== 'missing_bootstrap', rust_configuration_owner: restriction === 'deployment' ? 'deployment' : 'host' }, operation: { ...operation, kind: 'rust', status: 'failed', phase: 'failed' } });
    await draw(); await authorizeNetwork();
    expect(button('初始化 Rust').disabled).toBe(true);
    expect(container.textContent).toContain('当前部署不支持在此初始化 Rust，请联系部署者配置后重试');
    expect(container.textContent).not.toContain('允许联网后，点击上方初始化按钮重试');
  });
});
