/** Which Plugins this Pilot session can already use, and mounting them.
 *
 * The Host owns the facts: a session lists only installed Plugins, and a switch is written by POST,
 * never by guessing locally. The current Turn keeps its frozen capabilities; the Host reports
 * whether a change waits for the next message or schedules continuation after normal completion.
 *
 * It reads once when a session is selected, then polls only while open because Pilot can install a
 * Plugin mid-session. Opening the panel shares any pending read instead of starting it over.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { PanelRightClose, Plug } from 'lucide-react';
import { api, reason } from './api';
import { Modal } from './ui';
import type { Turn } from './PilotTurn';
import { PluginSettings, pluginSettingStatus, type PluginInformation } from './PluginPanel';
import { LibraryPluginDetails } from './LibraryPluginDetails';
import { NetworkPolicy } from './NetworkPolicy';
import type { NetworkMode } from './model';
import './SessionPlugins.css';

/** One installed Plugin as the Host reports it for a session. */
export type SessionPlugin = PluginInformation & {
  required?: boolean; on?: boolean;
  explicitOn?: boolean; requiresNetwork?: boolean; networkProvider?: boolean;
  networkAllowed?: boolean; networkMode?: NetworkMode; enabledBy?: string[];
};

export type PluginMountEffect = {
  status: 'next_turn' | 'after_current_turn' | 'unchanged'; source_turn?: string;
};
export type PluginContinuation = {
  session: string; source: string; state: NonNullable<Turn['continuation']>;
};
type MountChange = { effect: PluginMountEffect; continuation?: Turn['continuation'] };

function mountNotice(change: MountChange, continuation?: PluginContinuation | null): string {
  if (change.effect.status === 'unchanged') return '设置未改变';
  if (change.effect.status !== 'after_current_turn') return '已保存，下次发消息时使用新设置';
  const state = continuation && continuation.source === change.effect.source_turn ? continuation.state : change.continuation;
  switch (state?.status) {
    case 'admitted': return 'Pilot 已开始使用新设置继续处理你的请求。';
    case 'cancelled':
    case 'pending': return '已保存。本次未继续，下次发消息时使用新设置。';
    case 'superseded': return '已保存，Pilot 会在新的请求中使用新设置。';
    case 'scheduled': return '已保存。Pilot 会先完成当前回复，再使用新设置继续处理你的请求。';
    default: return '已保存，下次发消息时使用新设置';
  }
}

/** Long enough that a session is not one request per blink, short enough that a fresh install shows up. */
export const POLL_MS = 4000;
const COMPACT_QUERY = '(max-width: 1100px)';

/** Replace a row with what the Host returned. A row the Host no longer reports keeps its old place
 * rather than vanishing under the pointer that just clicked it. */
export function withPlugin(list: SessionPlugin[], plugin: SessionPlugin): SessionPlugin[] {
  return list.map(item => item.id === plugin.id ? plugin : item);
}

/** A required Plugin has no switch to make, and one switch is already waiting on the Host. */
export function maySwitch(plugin: SessionPlugin, inFlight: boolean): boolean {
  return !plugin.networkProvider && !plugin.required && !inFlight && (plugin.available !== false || (plugin.explicitOn ?? plugin.on) === true);
}

/** Turn the server's `error` into the line under the row, verbatim. */
export function mountProblem(error: unknown): string {
  return reason(error);
}

export function SessionPlugins({ session, defaultOpen, onContinuation, continuation }: {
  session: string; onContinuation?: (session: string, source: string) => void;
  defaultOpen?: boolean;
  continuation?: PluginContinuation | null;
}) {
  // The transcript keeps its full width on narrow screens; the same controls open in the shared
  // native dialog, which isolates focus and handles Escape.
  const [compact, setCompact] = useState(() => typeof window !== 'undefined'
    && typeof window.matchMedia === 'function' && window.matchMedia(COMPACT_QUERY).matches);
  const [open, setOpen] = useState(() => defaultOpen ?? !compact);
  const [plugins, setPlugins] = useState<SessionPlugin[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [listError, setListError] = useState('');
  const [rowError, setRowError] = useState<Record<string, string>>({});
  const [changes, setChanges] = useState<Record<string, MountChange>>({});
  const [pending, setPending] = useState('');
  const [opened, setOpened] = useState<SessionPlugin | null>(null);
  const [listRefresh, setListRefresh] = useState(0);
  const trigger = useRef<HTMLButtonElement>(null);
  const wasOpen = useRef(open);
  // A second click before the Host answers must not become a second switch of the same row.
  const inFlight = useRef(false);
  const mounted = useRef(true);
  const sessionRef = useRef(session);
  const sessionVersion = useRef(0);
  if (sessionRef.current !== session) { sessionRef.current = session; sessionVersion.current += 1; }
  const listRevision = useRef(0);
  const listRequest = useRef<AbortController | null>(null);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  useEffect(() => {
    // Removing a dialog does not reliably restore focus like dialog.close() does. Return focus
    // after React has removed the shared Modal and the compact trigger is no longer inert.
    if (compact && wasOpen.current && !open) trigger.current?.focus();
    wasOpen.current = open;
  }, [compact, open]);
  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return;
    const media = window.matchMedia(COMPACT_QUERY);
    const resize = () => {
      setCompact(media.matches);
      // Pilot starts with a quiet, collapsed entry. Resizing must preserve a panel the user opened.
      if (defaultOpen === undefined) setOpen(!media.matches);
    };
    media.addEventListener('change', resize);
    return () => media.removeEventListener('change', resize);
  }, [defaultOpen]);
  useEffect(() => {
    listRevision.current += 1;
    inFlight.current = false;
    setPlugins([]); setLoaded(false); setListError(''); setRowError({}); setChanges({}); setPending('');
    setOpened(null);
  }, [session]);
  useEffect(() => {
    if (!open) setOpened(null);
  }, [open]);
  const loadPlugins = useCallback(async () => {
    if (!session || listRequest.current) return;
    const controller = new AbortController();
    listRequest.current = controller;
    const version = sessionVersion.current;
    const revision = listRevision.current;
    const current = () => mounted.current && !controller.signal.aborted
      && version === sessionVersion.current && revision === listRevision.current;
    try {
      const data = await api<{ plugins: SessionPlugin[] }>(`/sessions/${encodeURIComponent(session)}/plugins`,
        'GET', undefined, controller.signal);
      if (!Array.isArray(data.plugins)) throw new Error('插件列表响应无效，请重试。');
      if (!current()) return;
      setPlugins(data.plugins); setListError(''); setLoaded(true);
    } catch (error) {
      // Keep the rows already read: a failed poll is not evidence that nothing is installed.
      if (current()) { setListError(mountProblem(error)); setLoaded(true); }
    } finally {
      if (listRequest.current === controller) listRequest.current = null;
    }
  }, [session]);
  useEffect(() => {
    if (!session) return;
    // One early read prepares the collapsed entry; only an open panel keeps polling.
    setListError('');
    if (listRefresh) setLoaded(false);
    void loadPlugins();
    return () => { listRequest.current?.abort(); listRequest.current = null; };
  }, [session, listRefresh, loadPlugins]);
  useEffect(() => {
    if (!open || !session) return;
    void loadPlugins();
    const timer = window.setInterval(() => { void loadPlugins(); }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [open, session, listRefresh, loadPlugins]);
  const toggle = async (plugin: SessionPlugin) => {
    if (!maySwitch(plugin, inFlight.current)) return;
    inFlight.current = true;
    const version = sessionVersion.current;
    setPending(plugin.id);
    setRowError(current => { const next = { ...current }; delete next[plugin.id]; return next; });
    try {
      const result = await api<{ plugin: SessionPlugin; plugins?: SessionPlugin[]; effect?: PluginMountEffect;
        continuation?: Turn['continuation'] }>(
        `/sessions/${encodeURIComponent(session)}/plugins/${encodeURIComponent(plugin.id)}`, 'POST', { on: !(plugin.explicitOn ?? plugin.on) });
      if (!mounted.current || version !== sessionVersion.current) return;
      listRevision.current += 1;
      setPlugins(current => result.plugins ?? withPlugin(current, result.plugin));
      const effect = result.effect ?? { status: 'next_turn' };
      setChanges(current => ({ ...current, [plugin.id]: { effect, continuation: result.continuation } }));
      if (effect.status === 'after_current_turn' && effect.source_turn && result.continuation?.status === 'scheduled') {
        onContinuation?.(session, effect.source_turn);
      }
    } catch (error) {
      // No optimistic flip: the row keeps the state the Host last confirmed, and the reason is the
      // Host's own sentence rather than a paraphrase written here.
      if (mounted.current && version === sessionVersion.current) setRowError(current => ({ ...current, [plugin.id]: mountProblem(error) }));
    } finally {
      if (mounted.current && version === sessionVersion.current) { inFlight.current = false; setPending(''); }
    }
  };
  const changeNetwork = async (plugin: SessionPlugin, mode: NetworkMode) => {
    if (inFlight.current || plugin.networkMode === undefined || mode === plugin.networkMode) return;
    inFlight.current = true;
    const version = sessionVersion.current;
    setPending(plugin.id);
    setRowError(current => { const next = { ...current }; delete next[plugin.id]; return next; });
    try {
      const result = await api<{ plugins: SessionPlugin[] }>(
        `/sessions/${encodeURIComponent(session)}/network`, 'POST', { mode });
      if (!Array.isArray(result.plugins)) throw new Error('插件列表响应无效，请刷新后重试。');
      if (!mounted.current || version !== sessionVersion.current) return;
      listRevision.current += 1;
      setPlugins(result.plugins);
      // Network policy is saved without scheduling continuation.
      setChanges(current => ({ ...current, [plugin.id]: { effect: { status: 'next_turn' } } }));
    } catch (error) {
      if (mounted.current && version === sessionVersion.current) setRowError(current => ({ ...current, [plugin.id]: mountProblem(error) }));
    } finally {
      if (mounted.current && version === sessionVersion.current) { inFlight.current = false; setPending(''); }
    }
  };
  const count = plugins.filter(plugin => plugin.on).length;
  const knowsCount = !!session && (plugins.length > 0 || (loaded && !listError));
  const loading = !!session && !loaded && !plugins.length && !listError;
  const pluginStatus = (plugin: SessionPlugin) => {
    if (plugin.networkProvider) {
      const enabled = plugin.networkMode === undefined ? '联网策略未知' : plugin.networkMode === 'denied' ? '已禁止联网'
        : plugin.networkMode === 'enabled' ? '主动启用' : plugin.on ? '自动 · 依赖启用' : '自动 · 未启用';
      return plugin.available === false ? `${enabled} · 不可用` : enabled;
    }
    return pluginSettingStatus(plugin, { selected: plugin.on === true }, { selectedLabel: '已挂载', unselectedLabel: '未挂载' });
  };
  const body = <>
    <div className="pilot-plugins-summary"><span>{knowsCount ? <>共 <strong>{plugins.length}</strong> 个插件</>
      : !session ? '为当前对话配置插件' : listError ? '暂时无法读取插件列表' : '正在读取已安装的插件…'}</span>
      {knowsCount && <span className="plugin-mounted-count">{count} 已挂载</span>}
    </div>
    <div className="pilot-plugins-body" aria-busy={loading}>
      <PluginSettings key={session} items={plugins} loading={loading} error={listError}
        retry={() => setListRefresh(value => value + 1)}
        selectedLabel="已挂载" unselectedLabel="未挂载" enableLabel="挂载" disableLabel="取消挂载"
        setting={plugin => ({ selected: (plugin.explicitOn ?? plugin.on) === true, active: plugin.on === true,
          required: plugin.required, pending: pending === plugin.id, status: pluginStatus(plugin),
          problem: rowError[plugin.id], notice: changes[plugin.id] ? mountNotice(changes[plugin.id], continuation?.session === session ? continuation : null) : undefined })}
        onToggle={plugin => void toggle(plugin)} onDetails={setOpened}
        control={plugin => plugin.networkProvider ? null : undefined}
        empty={<div className="plugin-empty"><strong>{session ? '还没有安装插件' : '为对话选择能力'}</strong>
          <p>{session ? '告诉 Pilot 你需要什么能力。安装完成后，插件会自动出现在这里。' : '先开始或选择一个对话，再为 Pilot 挂载需要的插件。'}</p></div>}>
        {plugin => {
          const dependents = (plugin.enabledBy ?? []).map(id => plugins.find(item => item.id === id)?.name || id);
          return plugin.networkProvider && <NetworkPolicy mode={plugin.networkMode} onChange={mode => void changeNetwork(plugin, mode)}
            pending={pending === plugin.id} scope="session" effective={plugin.networkAllowed} enabledBy={dependents} />;
        }}
      </PluginSettings>
    </div>
    <div className="pilot-plugins-footer"><p>需要其他能力？直接告诉 Pilot。</p></div>
    {opened && <LibraryPluginDetails key={`${session}/${opened.id}`} plugin={plugins.find(plugin => plugin.id === opened.id) ?? opened}
      problem={(plugins.find(plugin => plugin.id === opened.id) ?? opened).error}
      status={pluginStatus(plugins.find(plugin => plugin.id === opened.id) ?? opened)} close={() => setOpened(null)} />}
  </>;
  return <aside className={`pilot-plugins${open ? ' is-open' : ' is-closed'}`} aria-label="会话插件挂载">
    {compact ? <button ref={trigger} className="session-plugins-trigger" type="button" aria-haspopup="dialog" aria-expanded={open}
      aria-label="展开插件面板" onClick={() => setOpen(true)}><Plug size={15} />插件{knowsCount && <span>{count}</span>}</button>
      : <div className="pilot-plugins-heading">
        <div className="pilot-plugins-title"><span className="plugin-heading-icon"><Plug size={18} /></span><div>
          <div className="pilot-plugins-context">本会话能力</div><h3>插件</h3>
        </div></div>
        <button type="button" aria-expanded={open} aria-label={open ? '收起插件面板' : '展开插件面板'} title={open ? '收起插件面板' : '展开插件面板'}
          onClick={() => setOpen(!open)}>{open ? <PanelRightClose size={16} /> : <Plug size={16} />}<span>{open ? '收起' : '插件'}</span>{!open && knowsCount && <span className="plugin-trigger-count">{count}</span>}</button>
      </div>}
    {open && (compact ? <Modal title="会话插件" className="session-plugins-drawer" close={() => setOpen(false)}>{body}</Modal> : body)}
  </aside>;
}
