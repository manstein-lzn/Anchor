import { useCallback, useEffect, useRef, useState } from 'react';
import { Activity, AlertTriangle, Bot, CheckCircle2, Circle, Clock3, FileClock,
  GitBranch, LoaderCircle, RefreshCw, ShieldCheck, TerminalSquare, Wrench } from 'lucide-react';
import { ApiError, request } from './api';

type Run = { id: string; task_id: string; graph_version_id: string; status: string; current_phase: string;
  revision: number; last_event_sequence: number; workflow_version: string; created_at: string; updated_at: string };
type Task = { id: string; objective: string; constraints: string[]; success_criteria: string[]; status: string };
type NodeRun = { id: string; node_id: string; status: string; attempt: number; revision: number;
  input_hash?: string | null; context_generation: number; output_ref?: string | null; error_code?: string | null };
type Event = { sequence: number; event_type: string; payload: Record<string, unknown>; created_at: string };
type ContextSnapshot = { id: string; node_run_id: string; generation: number; input_hash: string;
  snapshot: Record<string, unknown>; created_at: string };
type EdgeDecision = { edge_index: number; source_node_id: string; target_node_id: string; selected: boolean;
  reason: string; condition?: string | null; evaluator: string; evaluator_version: string;
  evaluation_context_hash?: string | null; evidence_ref?: string | null; decided_at: string };
type Verification = { verification_id: string; node_id: string; verifier_ref: string; verifier_version: string;
  adapter: string; adapter_version: string; verdict: string; reason: string; evidence_ref: string;
  verified_artifact_hashes: string[]; verified_context_hash: string; model_ref?: string | null;
  model_provider?: string | null; model_name?: string | null; model_response_id?: string | null; decided_at: string };
type Operation = { operation_id: string; tool_ref: string; status: string; request_hash: string;
  result_ref?: string | null; error_code?: string | null; reconciliation_ref?: string | null; updated_at: string };
type Version = { graph_id: string; version: number; definition: { name: string; nodes: { id: string; name: string; type: string }[] } };
type Memory = { memory_id: string; content: string; content_hash: string; created_at: string; deleted_at?: string | null };
type Lease = { lease: { node_id: string; worker_id: string; claim_id: string; run_id: string }; node_type: string; state: string; recoverable: boolean; reason: string };

const statusText: Record<string, string> = {
  created: '已接纳', queued: '已排队', ready: '可领取', running: '执行中', pending: '等待依赖',
  waiting_approval: '等待审批', waiting_event: '等待事件', paused: '已暂停', retrying: '重试准备',
  completed: '已完成', skipped: '已跳过', succeeded: '成功', failed: '失败', cancelled: '已取消',
  registered: '已登记', outcome_unknown: '结果未知', blocked: '已阻塞', passed: '通过',
  rejected: '拒绝', error: '错误',
};
const tone = (status: string) => ['failed', 'outcome_unknown', 'rejected', 'error'].includes(status) ? 'danger' :
  ['completed', 'succeeded', 'passed'].includes(status) ? 'success' : ['running'].includes(status) ? 'active' : 'neutral';
const stamp = (value: string) => new Date(value).toLocaleString();
const short = (value: string) => value.slice(0, 8);

export function RunConsole({ token, onUnauthorized }: { token: string; onUnauthorized: () => void }) {
  const [runs, setRuns] = useState<Run[]>([]);
  const [selected, setSelected] = useState('');
  const [run, setRun] = useState<Run | null>(null);
  const [task, setTask] = useState<Task | null>(null);
  const [version, setVersion] = useState<Version | null>(null);
  const [nodes, setNodes] = useState<NodeRun[]>([]);
  const [events, setEvents] = useState<Event[]>([]);
  const [contexts, setContexts] = useState<ContextSnapshot[]>([]);
  const [decisions, setDecisions] = useState<EdgeDecision[]>([]);
  const [verifications, setVerifications] = useState<Verification[]>([]);
  const [operations, setOperations] = useState<Operation[]>([]);
  const [memories, setMemories] = useState<Memory[]>([]);
  const [leases, setLeases] = useState<Lease[]>([]);
  type WaitItem = { node_run: { id: string; node_id: string; status: string }; run_id: string; node_type: string };
  const [waits, setWaits] = useState<WaitItem[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const cursor = useRef(0);
  const api = useCallback(<T,>(path: string, method = 'GET', body?: unknown) => request<T>(token, path, method, body), [token]);
  const fail = useCallback((cause: unknown) => {
    const problem = cause as Error;
    setError(problem.message);
    if (cause instanceof ApiError && cause.status === 401) onUnauthorized();
  }, [onUnauthorized]);
  const openArtifact = useCallback(async (digest: string) => {
    try {
      const result = await api<{ content: string }>(`/api/artifacts/${digest}`);
      const blob = new Blob([result.content], { type: 'text/plain;charset=utf-8' });
      window.open(URL.createObjectURL(blob), '_blank', 'noopener,noreferrer');
    } catch (cause) { fail(cause); }
  }, [api, fail]);

  const loadRuns = useCallback(async () => {
    const list = await api<Run[]>('/api/runs?limit=100&offset=0');
    setRuns(list);
    setSelected(current => current && list.some(item => item.id === current) ? current : list[0]?.id ?? '');
  }, [api]);

  const loadDetail = useCallback(async (runId: string, incremental = false) => {
    if (!runId) { setRun(null); return; }
    const current = await api<Run>(`/api/runs/${runId}`);
    const [taskValue, versionValue, nodeValues, operationValues, eventValues, contextValues, decisionValues, verificationValues, memoryValues, leaseValues, waitValues] = await Promise.all([
      api<Task>(`/api/tasks/${current.task_id}`),
      api<Version>(`/api/graph-versions/${current.graph_version_id}`),
      api<NodeRun[]>(`/api/runs/${runId}/nodes`),
      api<Operation[]>(`/api/runs/${runId}/operations`),
      api<Event[]>(`/api/runs/${runId}/events?after=${incremental ? cursor.current : 0}&limit=200`),
      api<ContextSnapshot[]>(`/api/runs/${runId}/contexts`),
      api<EdgeDecision[]>(`/api/runs/${runId}/decisions`),
      api<Verification[]>(`/api/runs/${runId}/verifications`),
      api<Memory[]>(`/api/memory?run_id=${runId}`),
      api<Lease[]>(`/api/leases/active?run_id=${runId}`),
      api<WaitItem[]>(`/api/waits?run_id=${runId}`),
    ]);
    setRun(current); setTask(taskValue); setVersion(versionValue); setNodes(nodeValues); setOperations(operationValues); setContexts(contextValues); setDecisions(decisionValues); setVerifications(verificationValues); setMemories(memoryValues); setLeases(leaseValues); setWaits(waitValues);
    if (incremental) setEvents(values => [...values, ...eventValues.filter(event => !values.some(old => old.sequence === event.sequence))]);
    else setEvents(eventValues);
    cursor.current = Math.max(cursor.current, ...eventValues.map(event => event.sequence));
  }, [api]);
  const recoverLease = useCallback(async (claimId: string) => {
    const reason = window.prompt('请输入恢复原因（确认 worker 已中断）：', 'operator confirmed worker interruption');
    if (!reason) return;
    try { await api(`/api/leases/${claimId}/recover`, 'POST', { reason }); await loadDetail(selected); }
    catch (cause) { fail(cause); }
  }, [api, fail, loadDetail, selected]);
  const failLease = useCallback(async (claimId: string) => {
    const errorCode = window.prompt('请输入失败代码：', 'operator_confirmed_failure');
    if (!errorCode || !window.confirm('确认将该 Agent 节点及其 Run 标记为失败？')) return;
    try { await api(`/api/leases/${claimId}/fail`, 'POST', { error_code: errorCode, phase: 'operator' }); await loadDetail(selected); }
    catch (cause) { fail(cause); }
  }, [api, fail, loadDetail, selected]);
  const approveWait = useCallback(async (nodeRunId: string) => {
    const reason = window.prompt('请输入批准理由：', 'operator approved');
    if (!reason) return;
    try { await api(`/api/waits/${nodeRunId}/approve`, 'POST', { reason, actor: 'web-operator' }); await loadDetail(selected); }
    catch (cause) { fail(cause); }
  }, [api, fail, loadDetail, selected]);
  const rejectWait = useCallback(async (nodeRunId: string) => {
    const reason = window.prompt('请输入拒绝理由：', 'operator rejected');
    if (!reason || !window.confirm('确认拒绝该审批并失败整个 Run？')) return;
    try { await api(`/api/waits/${nodeRunId}/reject`, 'POST', { reason, actor: 'web-operator' }); await loadDetail(selected); }
    catch (cause) { fail(cause); }
  }, [api, fail, loadDetail, selected]);
  const resumeWait = useCallback(async (nodeRunId: string) => {
    const eventType = window.prompt('请输入事件类型：', '');
    if (!eventType) return;
    try { await api(`/api/waits/${nodeRunId}/resume`, 'POST', { event_type: eventType, payload: {}, actor: 'web-operator' }); await loadDetail(selected); }
    catch (cause) { fail(cause); }
  }, [api, fail, loadDetail, selected]);

  const refresh = useCallback(async () => {
    setBusy(true); setError('');
    try { await loadRuns(); if (selected) await loadDetail(selected, true); }
    catch (cause) { fail(cause); } finally { setBusy(false); }
  }, [fail, loadDetail, loadRuns, selected]);

  useEffect(() => { void loadRuns().catch(fail); }, [fail, loadRuns]);
  useEffect(() => {
    cursor.current = 0; setEvents([]); setError('');
    if (selected) void loadDetail(selected).catch(fail);
  }, [fail, loadDetail, selected]);
  useEffect(() => {
    if (!selected) return;
    const interval = window.setInterval(() => void loadDetail(selected, true).catch(fail), 5000);
    return () => window.clearInterval(interval);
  }, [fail, loadDetail, selected]);

  const names = new Map(version?.definition.nodes.map(node => [node.id, node]) ?? []);
  const graphOrder = new Map(version?.definition.nodes.map((node, index) => [node.id, index]) ?? []);
  const orderedNodes = [...nodes].sort((left, right) =>
    (graphOrder.get(left.node_id) ?? Number.MAX_SAFE_INTEGER) - (graphOrder.get(right.node_id) ?? Number.MAX_SAFE_INTEGER));
  return <div className="run-console">
    <aside className="run-list">
      <div className="section-heading"><h2>运行</h2><button className="icon-button" title="刷新运行" aria-label="刷新运行" disabled={busy} onClick={() => void refresh()}>{busy ? <LoaderCircle className="spin" size={17} /> : <RefreshCw size={17} />}</button></div>
      <div className="run-list-items">{runs.map(item => <button key={item.id} className={`run-list-item ${selected === item.id ? 'active' : ''}`} onClick={() => setSelected(item.id)}>
        <span className={`run-status-mark ${tone(item.status)}`}><Circle size={9} /></span><span><strong>{statusText[item.status] ?? item.status}</strong><small>{short(item.id)} · {stamp(item.created_at)}</small></span><code>r{item.revision}</code>
      </button>)}{!runs.length && <div className="run-empty"><FileClock size={28} /><span>暂无运行记录</span></div>}</div>
      <div className="library-footer"><Activity size={13} />真实状态 · 5 秒轮询</div>
    </aside>
    <main className="run-detail">
      {error && <div className="message error" role="alert"><AlertTriangle size={16} /><span>{error}</span></div>}
      {!run || !task ? <div className="run-placeholder"><TerminalSquare size={36} /><h2>{runs.length ? '载入运行状态' : '尚未提交运行'}</h2></div> : <>
        <header className="run-header"><div><span className="eyebrow">RUN · {short(run.id)}</span><h1>{task.objective}</h1><div className="run-meta"><span className={`status-pill ${tone(run.status)}`}>{statusText[run.status] ?? run.status}</span><span>{version?.definition.name ?? 'Graph'} · v{version?.version ?? '?'}</span><span>{run.current_phase}</span></div></div><div className="run-updated"><Clock3 size={14} />{stamp(run.updated_at)}</div></header>
        <div className="run-grid">
          <section className="run-section node-state"><header><h2><GitBranch size={16} />节点状态</h2><span>{nodes.length}</span></header><div className="node-state-list">{orderedNodes.map(node => {
            const definition = names.get(node.node_id); const artifact = node.output_ref?.match(/^artifact:\/\/sha256\/([0-9a-f]{64})$/)?.[1]; return <div className="node-state-row" key={node.id}><span className={`state-icon ${tone(node.status)}`}>{node.status === 'running' ? <LoaderCircle className="spin" size={15} /> : node.status === 'completed' ? <CheckCircle2 size={15} /> : <Bot size={15} />}</span><span><strong>{definition?.name ?? node.node_id}</strong><small>{definition?.type ?? 'node'} · attempt {node.attempt} · context {node.context_generation}{node.input_hash && <> · {node.input_hash.slice(0, 12)}</>}{artifact && <> · <button type="button" className="inline-link" onClick={() => void openArtifact(artifact)}>查看产物</button></>}</small></span><span className={`status-pill ${tone(node.status)}`}>{statusText[node.status] ?? node.status}</span></div>;
          })}</div></section>
          <section className="run-section event-stream"><header><h2><Activity size={16} />事件</h2><span>已载入 {events.length} · 最新 #{run.last_event_sequence}</span></header><div className="event-list">{events.map(event => <div className="event-row" key={event.sequence}><span className="event-sequence">{event.sequence}</span><span><strong>{event.event_type}</strong><small>{stamp(event.created_at)}</small><code>{JSON.stringify(event.payload)}</code></span></div>)}</div></section>
        </div>
        <section className="run-section operation-ledger"><header><h2><GitBranch size={16} />路由决议</h2><span>{decisions.length}</span></header>{!decisions.length ? <div className="ledger-empty">尚无已决议的 Graph 边</div> : <div className="ledger-table">{decisions.map(item => <div className="ledger-row" key={item.edge_index}><span><strong>{item.source_node_id} → {item.target_node_id}</strong><small>edge {item.edge_index} · {item.evaluator}@{item.evaluator_version}</small></span><span className={`status-pill ${item.selected ? 'success' : ''}`}>{item.selected ? '已选择' : '未选择'}</span><span className="ledger-result">{item.condition || item.reason}{item.evaluation_context_hash && <small>{item.evaluation_context_hash.slice(0, 16)}</small>}</span><time>{stamp(item.decided_at)}</time></div>)}</div>}</section>
        <section className="run-section operation-ledger"><header><h2><ShieldCheck size={16} />验证证据</h2><span>{verifications.length}</span></header>{!verifications.length ? <div className="ledger-empty">尚无验证决议</div> : <div className="ledger-table">{verifications.map(item => { const evidence = item.evidence_ref.match(/^artifact:\/\/sha256\/([0-9a-f]{64})$/)?.[1]; return <div className="ledger-row" key={item.verification_id}><span><strong>{names.get(item.node_id)?.name ?? item.node_id}</strong><small>{item.verifier_ref}@{item.verifier_version} · {item.adapter}@{item.adapter_version}</small></span><span className={`status-pill ${tone(item.verdict)}`}>{statusText[item.verdict] ?? item.verdict}</span><span className="ledger-result">{item.reason}<small>context {item.verified_context_hash.slice(0, 16)} · artifacts {item.verified_artifact_hashes.length}</small>{evidence && <button type="button" className="inline-link" onClick={() => void openArtifact(evidence)}>查看证据</button>}</span><time>{stamp(item.decided_at)}</time></div>; })}</div>}</section>
        <section className="run-section operation-ledger"><header><h2><Wrench size={16} />工具操作账本</h2><span>{operations.length}</span></header>{!operations.length ? <div className="ledger-empty"><ShieldCheck size={17} />尚无工具副作用操作</div> : <div className="ledger-table">{operations.map(item => <div className="ledger-row" key={item.operation_id}><span><strong>{item.tool_ref}</strong><small>{short(item.operation_id)} · {item.request_hash.slice(0, 12)}</small></span><span className={`status-pill ${tone(item.status)}`}>{statusText[item.status] ?? item.status}</span><span className="ledger-result">{item.result_ref || item.error_code || '—'}{item.reconciliation_ref && <small>{item.reconciliation_ref}</small>}</span><time>{stamp(item.updated_at)}</time></div>)}</div>}</section>
        <section className="run-section operation-ledger"><header><h2><ShieldCheck size={16} />长期记忆</h2><span>{memories.length}</span></header>{!memories.length ? <div className="ledger-empty">当前 Run 尚无长期记忆</div> : <div className="ledger-table">{memories.map(item => <div className="ledger-row" key={item.memory_id}><span><strong>{item.content}</strong><small>{short(item.memory_id)} · {item.content_hash.slice(0, 12)}</small></span><time>{stamp(item.created_at)}</time></div>)}</div>}</section>
        <section className="run-section operation-ledger"><header><h2><FileClock size={16} />执行上下文</h2><span>{contexts.length}</span></header>{!contexts.length ? <div className="ledger-empty">尚无已持久化的执行上下文</div> : <div className="ledger-table">{contexts.map(item => <div className="ledger-row" key={item.id}><span><strong>generation {item.generation}</strong><small>{short(item.node_run_id)} · {item.input_hash.slice(0, 16)}</small></span><code className="ledger-result">{JSON.stringify(item.snapshot)}</code><time>{stamp(item.created_at)}</time></div>)}</div>}</section>
          <section className="run-section operation-ledger"><header><h2><Clock3 size={16} />等待审批 / 事件</h2><span>{waits.length}</span></header>{!waits.length ? <div className="ledger-empty">当前 Run 没有等待中的审批或事件</div> : <div className="ledger-table">{waits.map(item => <div className="ledger-row" key={item.node_run.id}><span><strong>{names.get(item.node_run.node_id)?.name ?? item.node_run.node_id}</strong><small>{item.node_type}</small></span><span className="status-pill">{statusText[item.node_run.status] ?? item.node_run.status}</span><span className="lease-actions">{item.node_run.status === 'waiting_approval' && <><button type="button" className="inline-link" onClick={() => void approveWait(item.node_run.id)}>批准</button><button type="button" className="inline-link danger-link" onClick={() => void rejectWait(item.node_run.id)}>拒绝</button></>}{item.node_run.status === 'waiting_event' && <button type="button" className="inline-link" onClick={() => void resumeWait(item.node_run.id)}>恢复事件</button>}</span></div>)}</div>}</section>
          <section className="run-section operation-ledger"><header><h2><Activity size={16} />Worker Lease</h2><span>{leases.length}</span></header>{!leases.length ? <div className="ledger-empty">当前 Run 没有活动 lease</div> : <div className="ledger-table">{leases.map(item => <div className="ledger-row" key={item.lease.claim_id}><span><strong>{item.lease.node_id}</strong><small>{item.node_type} · {item.lease.worker_id} · {short(item.lease.claim_id)}</small></span><span className={`status-pill ${item.state === 'healthy' ? 'success' : 'danger'}`}>{item.state}</span><span>{item.reason}</span>{item.recoverable && <span className="lease-actions"><button type="button" className="inline-link" onClick={() => void recoverLease(item.lease.claim_id)}>人工恢复</button>{item.node_type === 'agent' && <button type="button" className="inline-link danger-link" onClick={() => void failLease(item.lease.claim_id)}>标记失败</button>}</span>}</div>)}</div>}</section>
      </>}
    </main>
  </div>;
}
