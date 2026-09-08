import { useCallback, useEffect, useState } from 'react';
import { Clock, Plus, RefreshCw } from 'lucide-react';
import { ApiError, request } from './api';
import type { Version } from './graph';

type TriggerType = 'manual' | 'cron' | 'interval' | 'internal_event' | 'webhook';
type Trigger = {
  id: string; graph_version_id: string; type: TriggerType; enabled: boolean;
  cron?: string | null; interval_seconds?: number | null; event_type?: string | null;
  timezone: string; filter_expression?: string | null; idempotency_field?: string | null;
  webhook_secret_ref?: string | null;
};

const typeLabels: Record<TriggerType, string> = {
  manual: '手动', cron: '定时 (cron)', interval: '定时 (间隔)',
  internal_event: '内部事件', webhook: 'Webhook',
};

function schedule(trigger: Trigger): string {
  if (trigger.type === 'cron') return trigger.cron || '(未设置 cron)';
  if (trigger.type === 'interval') return trigger.interval_seconds ? `每 ${trigger.interval_seconds}s` : '(未设置间隔)';
  if (trigger.type === 'internal_event' || trigger.type === 'webhook') return trigger.event_type || '(未设置事件类型)';
  return '由用户或 API 触发';
}

// Trigger authoring is intentionally separate from graph editing: a trigger
// pins one immutable version and never mutates it. Webhook secrets travel as
// references only; the browser never sees a secret value.
export function TriggerManager({ token, versions, onUnauthorized }: {
  token: string; versions: Version[]; onUnauthorized: () => void;
}) {
  const [versionId, setVersionId] = useState('');
  const [triggers, setTriggers] = useState<Trigger[]>([]);
  const [type, setType] = useState<TriggerType>('manual');
  const [cron, setCron] = useState('0 9 * * *');
  const [intervalSeconds, setIntervalSeconds] = useState(3600);
  const [eventType, setEventType] = useState('graph.event');
  const [secretRef, setSecretRef] = useState('');
  const [idempotencyField, setIdempotencyField] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  const api = useCallback(<T,>(path: string, method = 'GET', body?: unknown) =>
    request<T>(token, path, method, body), [token]);
  const fail = useCallback((cause: unknown) => {
    setError((cause as Error).message);
    if (cause instanceof ApiError && cause.status === 401) onUnauthorized();
  }, [onUnauthorized]);

  useEffect(() => {
    if (!versionId && versions.length) setVersionId(versions[0].graph_version_id);
  }, [versionId, versions]);

  const load = useCallback(async (id: string) => {
    if (!id) { setTriggers([]); return; }
    try { setTriggers(await api<Trigger[]>(`/api/graph-versions/${id}/triggers`)); }
    catch (cause) { fail(cause); }
  }, [api, fail]);

  useEffect(() => { void load(versionId); }, [load, versionId]);

  const register = useCallback(async () => {
    if (!versionId) return;
    const body: Record<string, unknown> = {
      graph_version_id: versionId, type, enabled: true, timezone: 'UTC',
      cron: type === 'cron' ? cron : null,
      interval_seconds: type === 'interval' ? intervalSeconds : null,
      event_type: type === 'internal_event' || type === 'webhook' ? eventType : null,
      idempotency_field: idempotencyField || null,
      webhook_secret_ref: type === 'webhook' ? secretRef || null : null,
    };
    if (type === 'webhook' && !body.webhook_secret_ref) { setError('Webhook 触发器需要 secret 引用'); return; }
    setBusy(true); setError('');
    try {
      await api(`/api/triggers/${crypto.randomUUID()}`, 'PUT', body);
      await load(versionId);
    } catch (cause) { fail(cause); } finally { setBusy(false); }
  }, [api, cron, eventType, fail, idempotencyField, intervalSeconds, load, secretRef, type, versionId]);

  const toggle = useCallback(async (trigger: Trigger) => {
    setBusy(true); setError('');
    try {
      await api(`/api/triggers/${trigger.id}`, 'PATCH', { enabled: !trigger.enabled });
      await load(versionId);
    } catch (cause) { fail(cause); } finally { setBusy(false); }
  }, [api, fail, load, versionId]);

  return <section className="trigger-manager" aria-label="触发器管理">
    <div className="section-heading"><h2><Clock size={16} />触发器</h2>
      <select aria-label="选择发布版本" value={versionId} onChange={event => setVersionId(event.target.value)}>
        {!versions.length && <option value="">暂无发布版本</option>}
        {versions.map(item => <option key={item.graph_version_id} value={item.graph_version_id}>v{item.version} · {item.definition.name}</option>)}
      </select>
      <button className="icon-button" title="刷新触发器" aria-label="刷新触发器" disabled={busy || !versionId} onClick={() => void load(versionId)}><RefreshCw size={15} /></button>
    </div>
    {error && <div className="message error" role="alert">{error}</div>}
    {triggers.length === 0
      ? <p className="muted">该版本尚无触发器</p>
      : <div className="ledger-table">{triggers.map(item => <div className="ledger-row" key={item.id}>
          <span><strong>{typeLabels[item.type]}</strong><small>{schedule(item)} · {item.timezone}{item.webhook_secret_ref && ` · secret: ${item.webhook_secret_ref}`}</small></span>
          <span className={`status-pill ${item.enabled ? 'success' : ''}`}>{item.enabled ? '已启用' : '已停用'}</span>
          <span className="lease-actions"><button type="button" className="inline-link" disabled={busy} onClick={() => void toggle(item)}>{item.enabled ? '停用' : '启用'}</button></span>
        </div>)}</div>}
    <div className="trigger-form">
      <label>类型<select aria-label="触发器类型" value={type} onChange={event => setType(event.target.value as TriggerType)}>
        {(Object.keys(typeLabels) as TriggerType[]).map(item => <option key={item} value={item}>{typeLabels[item]}</option>)}
      </select></label>
      {type === 'cron' && <label>Cron<input aria-label="Cron 表达式" value={cron} onChange={event => setCron(event.target.value)} /></label>}
      {type === 'interval' && <label>间隔（秒）<input aria-label="间隔秒数" type="number" min={1} value={intervalSeconds} onChange={event => setIntervalSeconds(Number(event.target.value))} /></label>}
      {(type === 'internal_event' || type === 'webhook') && <label>事件类型<input aria-label="事件类型" value={eventType} onChange={event => setEventType(event.target.value)} /></label>}
      {type === 'webhook' && <label>Secret 引用<input aria-label="Webhook secret 引用" value={secretRef} onChange={event => setSecretRef(event.target.value)} placeholder="OPENAI_API_KEY 之类的引用名" /></label>}
      {type !== 'manual' && <label>幂等字段（可选）<input aria-label="幂等字段" value={idempotencyField} onChange={event => setIdempotencyField(event.target.value)} /></label>}
      <button className="primary" disabled={busy || !versionId} onClick={() => void register()}><Plus size={15} />注册触发器</button>
    </div>
  </section>;
}
