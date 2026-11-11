import { useEffect, useRef, useState } from 'react';
import { api, reason } from './api';
import type { Plugin } from './model';
import { PluginDetails, type PluginInformation } from './PluginPanel';
import { PluginConfiguration } from './PluginConfiguration';
import { PluginInstall, PluginUninstall } from './PluginLifecycle';
import { GraphBot, PluginBots } from './GraphBot';

type Authorization = { state: 'loading' | 'authorized' | 'unauthorized' | 'unknown'; error?: string };

type Props = {
  plugin: PluginInformation; status: string; problem?: string | null; close: () => void; management?: boolean; onChanged?: () => void;
  /** The Graph whose node opened these details; `saved` is false while the draft differs from the saved Graph. */
  graph?: { name: string; saved: boolean };
};

/** Current-library adapter. Historical run details never mount this component. */
export function LibraryPluginDetails(props: Props) {
  return <CurrentLibraryPluginDetails key={props.plugin.id} {...props} />;
}

function CurrentLibraryPluginDetails({ plugin, status, problem, close, management = false, onChanged, graph }: Props) {
  const [updating, setUpdating] = useState(false);
  const [definition, setDefinition] = useState<PluginInformation | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [revision, setRevision] = useState(0);
  const [statuses, setStatuses] = useState<Record<string, Authorization>>({});
  const [pending, setPending] = useState('');
  const [authError, setAuthError] = useState('');
  const [authorizationLink, setAuthorizationLink] = useState('');
  const alive = useRef(true);
  const authBusy = useRef(false);
  const statusRevision = useRef(0);
  const [authRevision, setAuthRevision] = useState(0);
  const information: PluginInformation = { ...plugin, ...definition,
    error: problem || (definition ? definition.error : plugin.error) };
  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; statusRevision.current += 1; };
  }, []);
  useEffect(() => {
    let active = true;
    const controller = new AbortController();
    setLoading(true); setError('');
    api<Plugin>(`/plugins/${encodeURIComponent(plugin.id)}`, 'GET', undefined, controller.signal).then(data => {
      if (active) setDefinition(data);
    }).catch(problem => { if (active) setError(reason(problem)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; controller.abort(); };
  }, [plugin.id, revision]);
  useEffect(() => {
    let active = true;
    const names = Object.entries(information.mcpServers ?? {}).filter(([, server]) => server.auth === 'oauth').map(([name]) => name);
    const refresh = async () => {
      const version = ++statusRevision.current;
      setStatuses(current => Object.fromEntries(names.map(name => [name, current[name] ?? { state: 'loading' }])));
      const results = await Promise.all(names.map(async name => {
        try {
          const data = await api<{ authorized: boolean; expired?: boolean }>(`/plugins/${encodeURIComponent(plugin.id)}/oauth/${encodeURIComponent(name)}`);
          return [name, { state: data.authorized && !data.expired ? 'authorized' : 'unauthorized' }] as const;
        } catch (problem) { return [name, { state: 'unknown', error: reason(problem) }] as const; }
      }));
      if (active && version === statusRevision.current) setStatuses(Object.fromEntries(results) as Record<string, Authorization>);
    };
    const onFocus = () => { if (!authBusy.current) void refresh(); };
    void refresh();
    window.addEventListener('focus', onFocus);
    return () => { active = false; statusRevision.current += 1; window.removeEventListener('focus', onFocus); };
  }, [information.mcpServers, plugin.id, authRevision]);
  const authorize = async (name: string) => {
    if (authBusy.current) return;
    authBusy.current = true; setPending(name); setAuthError(''); setAuthorizationLink('');
    const popup = window.open('about:blank', '_blank');
    if (popup) popup.opener = null;
    try {
      const data = await api<{ authorization_url: string }>(`/plugins/${encodeURIComponent(plugin.id)}/authorize/${encodeURIComponent(name)}`, 'POST');
      if (!alive.current) { popup?.close(); return; }
      if (popup && !popup.closed) popup.location.assign(data.authorization_url);
      else { setAuthorizationLink(data.authorization_url); setAuthError('授权窗口未能打开。点击“打开授权页面”继续。'); }
    } catch (problem) {
      popup?.close();
      if (alive.current) setAuthError(reason(problem));
    } finally { authBusy.current = false; if (alive.current) setPending(''); }
  };
  const revoke = async (name: string) => {
    if (authBusy.current) return;
    authBusy.current = true; setPending(name); setAuthError(''); setAuthorizationLink('');
    statusRevision.current += 1;
    try {
      await api(`/plugins/${encodeURIComponent(plugin.id)}/oauth/${encodeURIComponent(name)}`, 'DELETE');
      if (alive.current) setStatuses(current => ({ ...current, [name]: { state: 'unauthorized' } }));
    } catch (problem) { if (alive.current) setAuthError(reason(problem)); }
    finally { authBusy.current = false; if (alive.current) setPending(''); }
  };
  const oauth = Object.entries(information.mcpServers ?? {}).filter(([, server]) => server.auth === 'oauth');
  return <><PluginDetails plugin={information} status={status} loading={loading} error={error}
    retry={() => setRevision(value => value + 1)} close={close}>
    <PluginConfiguration pluginId={plugin.id} onChanged={onChanged} />
    {!!definition?.channels?.length && (graph ? <GraphBot graph={graph.name} pluginId={plugin.id} saved={graph.saved} /> : <PluginBots pluginId={plugin.id} />)}
    {management && information.canReplace === true && <section className="plugin-details-section"><div className="plugin-management-heading"><h3>插件版本</h3>
      <button type="button" onClick={() => setUpdating(true)}>更新插件</button></div></section>}
    {!!oauth.length && <section className="plugin-details-section plugin-authorization"><h3>服务授权 <span>OAuth</span></h3>
      <p>授权保存在当前账户下，供后续任务使用。</p>
      {oauth.map(([name]) => {
        const authorization = statuses[name] ?? { state: 'loading' };
        return <div className="plugin-authorization-service" key={name}><div><strong>{name}</strong>
          <span>{authorization.state === 'authorized' ? '已授权' : authorization.state === 'unauthorized' ? '未授权'
            : authorization.state === 'loading' ? '正在读取授权状态…' : '无法读取授权状态'}</span></div>
          <div className="plugin-authorization-actions"><button type="button" disabled={!!pending} onClick={() => void authorize(name)}>
            {pending === name ? '处理中…' : authorization.state === 'authorized' ? `重新授权 ${name}` : `授权 ${name}`}</button>
            {authorization.state === 'authorized' && <button type="button" disabled={!!pending} onClick={() => void revoke(name)}>撤销 {name}</button>}
            {authorization.state === 'unknown' && <button type="button" disabled={!!pending} onClick={() => setAuthRevision(value => value + 1)}>重试授权状态</button>}
          </div>{authorization.error && <p className="plugin-message is-error">{authorization.error}</p>}
        </div>;
      })}
      {authError && <p className="plugin-message is-error" role="alert">{authError}</p>}
      {authorizationLink && <a className="plugin-authorization-link" href={authorizationLink} target="_blank" rel="noopener noreferrer">打开授权页面</a>}
    </section>}
    {management && information.canUninstall === true && <PluginUninstall pluginId={plugin.id} onChanged={() => { onChanged?.(); close(); }} />}
  </PluginDetails>
    {updating && information.canReplace === true && <PluginInstall pluginId={plugin.id} close={() => setUpdating(false)} onChanged={() => { onChanged?.(); setRevision(value => value + 1); setAuthRevision(value => value + 1); }} />}
  </>;
}
