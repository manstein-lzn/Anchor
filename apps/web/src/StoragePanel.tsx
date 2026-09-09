import { useCallback, useEffect, useState } from 'react';
import { Database, HardDrive, RefreshCw, Save, ShieldAlert, Trash2 } from 'lucide-react';
import { ApiError, request } from './api';

type StorageGraph = {
  graph_id: string; runs: number; artifact_files: number; artifact_bytes: number;
  exclusive_bytes: number; shared_bytes: number;
  budget_bytes: number | null; over_budget: boolean;
};
type StorageReport = {
  database_bytes: number | null; artifacts_bytes: number; artifacts_files: number;
  total_bytes: number; budget: { global_bytes: number | null }; over_global_budget: boolean;
  graphs: StorageGraph[]; runs_total: number; runs_terminal: number;
};
type Budgets = { global_bytes: number | null; graphs: Record<string, number | null> };
type Candidate = { run_id: string; graph_id: string | null; created_at: string; exclusive_bytes: number };
type Preview = {
  needed: boolean; reason?: string; total_bytes: number; over_global_budget: boolean;
  graphs: StorageGraph[]; candidates: Candidate[]; protected_runs: number;
};
type Audit = { audit_id: string; created_at: string; trigger: string; evicted_runs: number; freed_bytes: number };

const GB = 1024 ** 3;
const human = (bytes: number | null | undefined) => {
  if (bytes === null || bytes === undefined) return '—';
  if (bytes >= GB) return `${(bytes / GB).toFixed(2)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024).toFixed(0)} KB`;
};
const toBytes = (value: string) => {
  const parsed = Number(value);
  return value.trim() === '' || !Number.isFinite(parsed) || parsed <= 0 ? null : Math.round(parsed * GB);
};
const toGb = (bytes: number | null | undefined) => (bytes === null || bytes === undefined ? '' : String(+(bytes / GB).toFixed(3)));

// Budgets are monitoring targets that never terminate a running node. They are
// stored server-side, so an operator can retune them at runtime without a
// restart; this panel only ever writes the two knobs.
export function StoragePanel({ token, onUnauthorized }: { token: string; onUnauthorized: () => void }) {
  const [report, setReport] = useState<StorageReport | null>(null);
  const [globalInput, setGlobalInput] = useState('');
  const [graphInputs, setGraphInputs] = useState<Record<string, string>>({});
  const [plan, setPlan] = useState<Preview | null>(null);
  const [audit, setAudit] = useState<Audit[]>([]);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  const [error, setError] = useState('');

  const api = useCallback(<T,>(path: string, method = 'GET', body?: unknown) =>
    request<T>(token, path, method, body), [token]);
  const fail = useCallback((cause: unknown) => {
    setError((cause as Error).message);
    if (cause instanceof ApiError && cause.status === 401) onUnauthorized();
  }, [onUnauthorized]);

  const load = useCallback(async () => {
    const [nextReport, nextBudgets, nextAudit] = await Promise.all([
      api<StorageReport>('/api/storage'), api<Budgets>('/api/storage/budget'),
      api<Audit[]>('/api/retention/audit?limit=5'),
    ]);
    setReport(nextReport); setAudit(nextAudit);
    setGlobalInput(toGb(nextBudgets.global_bytes));
    setGraphInputs(Object.fromEntries(nextReport.graphs.map(item => [
      item.graph_id, toGb(nextBudgets.graphs[item.graph_id] ?? item.budget_bytes)])));
  }, [api]);

  useEffect(() => { void load().catch(fail); }, [fail, load]);

  const saveGlobal = useCallback(async () => {
    setBusy(true); setError(''); setNotice('');
    try {
      await api<Budgets>('/api/storage/budget', 'PUT', { global_bytes: toBytes(globalInput) });
      await load(); setNotice('全局预算已更新');
    } catch (cause) { fail(cause); } finally { setBusy(false); }
  }, [api, fail, globalInput, load]);

  const saveGraph = useCallback(async (graphId: string) => {
    setBusy(true); setError(''); setNotice('');
    try {
      await api<Budgets>('/api/storage/budget', 'PUT',
        { graphs: { [graphId]: toBytes(graphInputs[graphId] ?? '') } });
      await load(); setNotice(`${graphId} 预算已更新`);
    } catch (cause) { fail(cause); } finally { setBusy(false); }
  }, [api, fail, graphInputs, load]);

  const previewRetention = useCallback(async () => {
    setError(''); setNotice('');
    try { setPlan(await api<Preview>('/api/retention/preview')); }
    catch (cause) { fail(cause); }
  }, [api, fail]);

  const runSweep = useCallback(async () => {
    if (!window.confirm('滚动清理会永久删除最老的已结束运行及其产物，且不可恢复。继续？')) return;
    setBusy(true); setError(''); setNotice('');
    try {
      const result = await api<{ evicted: number; freed_bytes: number }>('/api/retention/sweep', 'POST');
      await load(); setPlan(null);
      setNotice(result.evicted ? `已清理 ${result.evicted} 条运行，释放 ${human(result.freed_bytes)}` : '当前没有需要清理的运行');
    } catch (cause) { fail(cause); } finally { setBusy(false); }
  }, [api, fail, load]);

  if (!report) return <div className="storage-panel"><p className="muted">正在读取存储占用…</p></div>;
  const usage = report.budget.global_bytes ? Math.min(100, (report.total_bytes / report.budget.global_bytes) * 100) : 0;
  return <div className="storage-panel">
    <header className="storage-heading">
      <div><span className="eyebrow">STORAGE</span><h1>存储预算</h1></div>
      <button className="icon-button" title="刷新存储报告" aria-label="刷新存储报告" disabled={busy} onClick={() => void load()}><RefreshCw size={17} /></button>
    </header>
    {(error || notice) && <div className={`message ${error ? 'error' : ''}`} role={error ? 'alert' : 'status'}>{error || notice}</div>}

    <section className="storage-summary">
      <div className="storage-card"><HardDrive size={18} /><span>总占用</span><strong>{human(report.total_bytes)}</strong><small>数据库 {human(report.database_bytes)} · 产物 {human(report.artifacts_bytes)}（{report.artifacts_files} 文件）</small></div>
      <div className="storage-card"><Database size={18} /><span>运行记录</span><strong>{report.runs_total}</strong><small>其中已结束 {report.runs_terminal}</small></div>
      <div className={`storage-card ${report.over_global_budget ? 'over' : ''}`}><span>全局预算</span><strong>{report.budget.global_bytes ? human(report.budget.global_bytes) : '未设置'}</strong>
        {report.budget.global_bytes ? <><div className="storage-bar"><i style={{ width: `${usage}%` }} /></div><small>{report.over_global_budget ? '已超出预算' : `已用 ${usage.toFixed(1)}%`}</small></> : <small>设置后在此显示占用比例</small>}
      </div>
    </section>

    <section className="storage-budget-form">
      <label>全局预算（GB）<input aria-label="全局存储预算" type="number" min="0" step="0.5" value={globalInput} onChange={event => setGlobalInput(event.target.value)} placeholder="留空表示不限制" /></label>
      <button className="primary" disabled={busy} onClick={() => void saveGlobal()}><Save size={15} />保存全局预算</button>
      <small className="muted">预算只用于监控和提示，不会终止正在运行的节点。</small>
    </section>

    <section className="storage-retention">
      <div className="section-heading"><h2><ShieldAlert size={16} />滚动清理</h2><span className="muted">仅淘汰已结束的运行 · 不终止在跑节点</span></div>
      <div className="retention-actions">
        <button type="button" disabled={busy} onClick={() => void previewRetention()}>预览清理</button>
        <button type="button" className="danger-link" disabled={busy} onClick={() => void runSweep()}><Trash2 size={14} />立即清理</button>
      </div>
      {plan && <div className="retention-plan">
        {!plan.needed && <span>当前未超出预算，无需清理。</span>}
        {plan.needed && <span>已超出预算：候选 <strong>{plan.candidates.length}</strong> 条（最老优先），受保护 <strong>{plan.protected_runs}</strong> 条不会删除。</span>}
        {plan.needed && plan.candidates.length > 0 && <small>最老：{new Date(plan.candidates[0].created_at).toLocaleString()} · 最近：{new Date(plan.candidates[plan.candidates.length - 1].created_at).toLocaleString()}</small>}
      </div>}
      {!!audit.length && <div className="ledger-table">{audit.map(item => <div className="ledger-row" key={item.audit_id}>
        <span><strong>{item.evicted_runs} 条运行</strong><small>{item.trigger} · {new Date(item.created_at).toLocaleString()}</small></span>
        <span>释放 {human(item.freed_bytes)}</span>
      </div>)}</div>}
    </section>

    <section className="storage-graphs">
      <div className="section-heading"><h2>每个图的占用与预算</h2><span className="muted">{report.graphs.length} 个图</span></div>
      {!report.graphs.length && <p className="muted">还没有任何运行记录</p>}
      {!!report.graphs.length && <div className="ledger-table">
        <div className="ledger-row storage-row head"><span>图</span><span>占用</span><span>预算（GB）</span><span /></div>
        {report.graphs.map(item => <div className="ledger-row storage-row" key={item.graph_id}>
          <span><strong>{item.graph_id}</strong><small>{item.runs} 次运行 · {item.artifact_files} 个产物{item.shared_bytes ? ` · 共享 ${human(item.shared_bytes)}` : ''}</small></span>
          <span className={item.over_budget ? 'over-text' : ''}>{human(item.artifact_bytes)}{item.over_budget && <small>已超出</small>}</span>
          <span><input aria-label={`${item.graph_id} 存储预算`} type="number" min="0" step="0.5" value={graphInputs[item.graph_id] ?? ''} placeholder="不限" onChange={event => setGraphInputs(current => ({ ...current, [item.graph_id]: event.target.value }))} /></span>
          <span><button type="button" className="inline-link" disabled={busy} onClick={() => void saveGraph(item.graph_id)}>保存</button></span>
        </div>)}
      </div>}
    </section>
  </div>;
}
