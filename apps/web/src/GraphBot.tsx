import { useEffect, useRef, useState } from 'react';
import { Check, Radio, RefreshCw } from 'lucide-react';
import { api, reason } from './api';
import { listenerLabel, type GraphSummary } from './model';
import { PluginCredentials } from './PluginCredentials';
import './Channels.css';

type Binding = { plugin: string; users: string[]; send_users: string[] };
type BotStatus = {
  graph: string; configured: boolean; mutable?: boolean; enabled?: boolean; owner_matches?: boolean; available?: boolean;
  binding?: Binding; listener?: NonNullable<GraphSummary['listener']>['status']; account?: string | null; restart_required?: boolean;
};
type Channels = { channels: BotStatus[] };
const recipients = (text: string) => [...new Set(text.split(/[\n,]/).map(value => value.trim()).filter(Boolean))];
const path = (graph: string) => `/graphs/${encodeURIComponent(graph)}/channel`;

/** One Graph's bot: members, credentials and the gateway connection are separate facts of this Graph.
 *  The binding reads the saved Graph, so an unsaved draft is configured after saving. */
export function GraphBot({ graph, pluginId, saved }: { graph: string; pluginId: string; saved: boolean }) {
  const [status, setStatus] = useState<BotStatus | null>(null);
  const [read, setRead] = useState<'loading' | 'ready' | 'failed'>('loading');
  const [error, setError] = useState('');
  const [readError, setReadError] = useState('');
  const [notice, setNotice] = useState('');
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  const alive = useRef(true);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  useEffect(() => {
    if (!saved) return;
    const controller = new AbortController();
    let disposed = false;
    const load = async () => {
      try {
        const value = await api<BotStatus>(path(graph), 'GET', undefined, controller.signal);
        if (!disposed) { setStatus(value); setRead('ready'); setReadError(''); }
      } catch (cause) { if (!disposed) { setRead('failed'); setReadError(reason(cause)); } }
    };
    void load();
    const timer = window.setInterval(() => { void load(); }, 8000);
    return () => { disposed = true; controller.abort(); window.clearInterval(timer); };
  }, [graph, saved, revision]);
  const refresh = () => setRevision(value => value + 1);
  const mutate = async (operation: () => Promise<BotStatus>, message: string) => {
    if (busy) return false;
    setBusy(true); setError(''); setNotice('');
    try { const result = await operation(); if (alive.current) { setStatus(result); setNotice(message); refresh(); } return true; }
    catch (cause) { if (alive.current) { setError(reason(cause)); refresh(); } return false; }
    finally { if (alive.current) setBusy(false); }
  };
  const setEnabled = (enabled: boolean, message: string) => {
    if (!status?.binding) return;
    void mutate(() => api<BotStatus>(`${path(graph)}/enabled`, 'POST', { enabled, expected_binding: status.binding }), message);
  };
  const heading = <div className="plugin-management-heading"><h3>此 Graph 的机器人</h3>
    {saved && <button type="button" onClick={refresh} disabled={busy}><RefreshCw size={14} />刷新状态</button>}</div>;
  if (!saved) return <section className="plugin-details-section graph-bot" aria-label="此 Graph 的机器人">{heading}
    <p className="channels-message">机器人按已保存的 Graph 配置。请先保存 Graph，再配置成员、凭据和启用。</p></section>;
  const listener = read === 'ready' ? status?.listener ?? 'unavailable' : 'unavailable';
  const other = status?.binding && status.binding.plugin !== pluginId ? status.binding.plugin : '';
  return <section className="plugin-details-section graph-bot" aria-label="此 Graph 的机器人">{heading}
    <p>企业微信消息到达时唤醒此工作流，每段对话接续自己的运行实例。对话在运行看板的「消息会话」中管理。</p>
    {(error || readError) && <div className="channels-message bad" role="alert"><strong>读取或操作未完成</strong><p>{[error, readError].filter(Boolean).join('；')}</p><span>检查服务和资源状态后重试。请求超时后请先刷新，核查实际结果。</span></div>}
    {notice && <p className="channels-message good" role="status"><Check size={15} />{notice}</p>}
    <div className={`channels-signal ${listener === 'authenticated' ? 'connected' : ''}`}><Radio size={19} />
      <div><strong>{read === 'failed' ? '连接状态无法确认' : status ? listenerLabel(listener) : '正在读取机器人状态…'}</strong><small>{status?.owner_matches === false ? '当前身份无法读取机器人状态' : status ? `${read === 'failed' ? '上次读取：' : ''}${status.enabled ? '机器人已启用' : '机器人未启用'} · ${status.configured ? '成员已保存' : '尚未保存成员'}` : read === 'failed' ? '机器人状态不可用' : '正在读取机器人信息'}</small></div>
    </div>
    {status?.owner_matches === false ? <p className="channels-message">此 Graph 的机器人属于另一位管理者，当前身份无法查看或修改。</p>
      : status && <>
        {other && <p className="channels-message warn">此 Graph 的机器人当前使用插件 <code>{other}</code>。</p>}
        {status.restart_required && <p className="channels-message warn">网关需要重新启动。请检查插件和凭据，资源可用后点击「恢复连接」。</p>}
        {read === 'ready' && status.enabled && listener === 'unavailable' && !status.restart_required && <p className="channels-message warn">机器人已启用，但无法确认网关连接。请检查插件、凭据和 Host 日志。</p>}
        <Members status={status} busy={busy} save={members => mutate(
          () => api<BotStatus>(path(graph), 'PUT', members), '成员已保存；启用后按新名单接收消息。',
        )} />
        <div className="channels-credentials"><h3>机器人账户与凭据</h3>
          <p>Bot ID 和 Secret 只属于此 Graph 的机器人。机器人启用时不能修改，停用后再轮换或清除。</p>
          {status.account && <p className="channels-account">当前账户指纹 <code title={status.account}>{status.account.slice(0, 22)}…</code></p>}
          {status.enabled ? <p className="channels-message">机器人已启用。停用后才能修改凭据。</p>
            : <PluginCredentials pluginId={pluginId} endpoint={`${path(graph)}/credentials`} onChanged={refresh} />}
        </div>
        <div className="channels-enable"><button type="button" className={status.enabled ? '' : 'primary'} disabled={busy || !status.binding} onClick={() => setEnabled(!status.enabled,
          status.enabled ? '机器人已停用。现有实例的运行状态请在运行看板的消息会话中管理。' : '启用请求已完成；连接状态以网关实际状态为准。',
        )}>{busy ? '正在处理…' : status.enabled ? '停用机器人' : '启用机器人'}</button>
          {status.enabled && status.restart_required && status.available !== false && <button type="button" disabled={busy || read !== 'ready' || !status.binding} onClick={() => setEnabled(true,
            '恢复请求已完成；连接状态以网关实际状态为准。',
          )}>恢复连接</button>}
          <small>{status.binding ? '启停不会停止或退役已有实例。' : '先保存成员名单，再启用。'}</small></div>
      </>}
  </section>;
}

function Members({ status, busy, save }: { status: BotStatus; busy: boolean; save: (members: { users: string[]; send_users: string[] }) => Promise<boolean> }) {
  const savedUsers = (status.binding?.users ?? []).join('\n');
  const savedSend = (status.binding?.send_users ?? []).join('\n');
  const [users, setUsers] = useState(savedUsers);
  const [sendUsers, setSendUsers] = useState(savedSend);
  const [dirty, setDirty] = useState(false);
  useEffect(() => { if (!dirty) { setUsers(savedUsers); setSendUsers(savedSend); } }, [savedUsers, savedSend, dirty]);
  const disabled = Boolean(status.enabled) || busy;
  return <form className="channels-binding" onSubmit={event => {
    event.preventDefault(); if (disabled) return;
    void save({ users: recipients(users), send_users: recipients(sendUsers) }).then(done => { if (done) setDirty(false); });
  }}>
    <div className="channels-section-title"><h3>成员</h3>{status.enabled && <small>停用机器人后可修改</small>}</div>
    <fieldset disabled={disabled}>
      <label>允许接收的成员<textarea rows={3} value={users} onChange={event => { setUsers(event.target.value); setDirty(true); }} placeholder="每行一个成员 ID" /></label>
      <label>允许主动发送的成员<textarea rows={3} value={sendUsers} onChange={event => { setSendUsers(event.target.value); setDirty(true); }} placeholder="每行一个成员 ID；需显式填写，包含要回复的成员" /></label>
    </fieldset><p className="channels-field-help">成员 ID 以换行或逗号分隔。空接收名单拒绝所有成员；<code>*</code> 允许所有成员。接收名单不为空时，主动发送名单也必须填写。</p>
    <div className="channels-form-actions"><button type="submit" disabled={disabled}><Check size={14} />保存成员</button>
      {dirty && <button type="button" disabled={busy} onClick={() => setDirty(false)}>恢复已保存值</button>}</div>
  </form>;
}

/** Library view of a channel Plugin: which Graphs use it as their bot. Bots are edited from their Graph. */
export function PluginBots({ pluginId }: { pluginId: string }) {
  const [bots, setBots] = useState<BotStatus[] | null>(null);
  const [error, setError] = useState('');
  useEffect(() => {
    const controller = new AbortController();
    api<Channels>('/channels', 'GET', undefined, controller.signal)
      .then(data => setBots(data.channels.filter(bot => bot.binding?.plugin === pluginId)))
      .catch(cause => { if (!controller.signal.aborted) setError(reason(cause)); });
    return () => controller.abort();
  }, [pluginId]);
  return <section className="plugin-details-section graph-bot" aria-label="使用此插件的机器人"><h3>使用此插件的机器人</h3>
    {error ? <p className="plugin-message is-error" role="alert">{error}</p>
      : !bots ? <p className="plugin-message" role="status">正在读取…</p>
      : !bots.length ? <p className="plugin-message">还没有 Graph 用它做机器人。在 Graph 的 Agent 节点挂载此插件，从节点的插件详情配置。</p>
      : <ul>{bots.map(bot => <li key={bot.graph}><code>{bot.graph}</code> · {bot.enabled ? listenerLabel(bot.listener ?? 'unavailable') : '未启用'}</li>)}</ul>}
    <p>机器人凭据和成员在对应 Graph 的节点插件详情中编辑。</p>
  </section>;
}
