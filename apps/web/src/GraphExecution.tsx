import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { MarkerType } from '@xyflow/react';
import { CheckCircle2, Download, Play, RefreshCw, X, FileText, Square } from 'lucide-react';
import { request, serverNow } from './api';
import { project, type Layout, type Version, type NodeSpec } from './graph';
import { executionAttempt, reviewOutcome, reviewDisplay, type ReviewOutcome } from './execution';
import { decisionFor, elapsed, label, latestNodes, nodeState, terminal, type ExecutionRun, type ExecutionNode, type ExecutionLease, type ExecutionDecision, type ExecutionOperation, type ExecutionDiagnostic, type ExecutionProgress } from './execution';
import { ExecutionCanvas, type ExecutionFlowNode } from './ExecutionCanvas';

type Snapshot = { run: ExecutionRun; version: Version; objective: string; nodes: ExecutionNode[]; leases: ExecutionLease[]; decisions: ExecutionDecision[]; operations: ExecutionOperation[]; contexts: { node_run_id: string; snapshot: unknown }[]; diagnostics: ExecutionDiagnostic[]; progress: ExecutionProgress[]; outcomes: Record<string, ReviewOutcome | undefined> };

function academicRole(spec: NodeSpec | undefined): string | undefined {
  const metadata = spec?.metadata as { behavior_ref?: string } | undefined;
  return metadata?.behavior_ref?.replace('academic.', '')
    ?? (spec?.agent_ref?.startsWith('agents.academic.') ? spec.agent_ref.split('.').at(-1) : undefined);
}

export function GraphExecution({ token, graphId, layout }: { token: string; graphId: string; layout: Layout }) {
  const [runs, setRuns] = useState<ExecutionRun[]>([]);
  const [selected, setSelected] = useState('');
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [versions, setVersions] = useState<Version[]>([]);
  const [nodeId, setNodeId] = useState('');
  const [attempt, setAttempt] = useState<number | null>(null);
  const [content, setContent] = useState('');
  const [contentRef, setContentRef] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [starting, setStarting] = useState(false);
  const [objective, setObjective] = useState('');
  const [inputs, setInputs] = useState('{}');
  const [now, setNow] = useState(serverNow());
  const [lastSync, setLastSync] = useState(0);
  const [startVersion, setStartVersion] = useState('');
  const submission = useRef<{ key: string; body: string; trigger: string } | null>(null);
  const api = useCallback(<T,>(path: string, method = 'GET', body?: unknown) => request<T>(token, path, method, body), [token]);
  useEffect(() => {
    let active = true;
    async function load() {
      try {
        const list = await api<ExecutionRun[]>(`/api/runs?graph_id=${encodeURIComponent(graphId)}&limit=100`);
        const published = await api<Version[]>(`/api/graphs/${encodeURIComponent(graphId)}/versions?limit=100`);
        if (active) { setRuns(list); setVersions(published); setSelected(value => value || list[0]?.id || ''); setStartVersion(value => value || published[0]?.graph_version_id || ''); }
      } catch (cause) { if (active) setError((cause as Error).message); }
    }
    void load();
    const timer = window.setInterval(() => void load(), 10000);
    return () => { active = false; window.clearInterval(timer); };
  }, [api, graphId]);
  useEffect(() => {
    let active = true;
    let timer = 0;
    const outcomeCache = new Map<string, ReviewOutcome | undefined>();
    setSnapshot(null); setNodeId(''); setContent(''); setAttempt(null); setLastSync(0);
    async function poll() {
      if (!selected) return;
      try {
        const run = await api<ExecutionRun>(`/api/runs/${selected}`);
        const [version, task, nodes, leases, decisions, operations, contexts, diagnostics, progress] = await Promise.all([
          api<Version>(`/api/graph-versions/${run.graph_version_id}`), api<{ objective: string }>(`/api/tasks/${run.task_id}`),
          api<ExecutionNode[]>(`/api/runs/${selected}/nodes`), api<ExecutionLease[]>(`/api/leases/active?run_id=${selected}`),
          api<ExecutionDecision[]>(`/api/runs/${selected}/decisions`), api<ExecutionOperation[]>(`/api/runs/${selected}/operations`),
          api<Snapshot['contexts']>(`/api/runs/${selected}/contexts`),
          api<ExecutionDiagnostic[]>(`/api/runs/${selected}/diagnostics`),
          api<ExecutionProgress[]>(`/api/runs/${selected}/progress`),
        ]);
        const reviewNodes = [...latestNodes(nodes).values()].filter(node => node.status === 'completed' && node.output_ref
          && ['reviewer', 'review_gate'].includes(academicRole(version.definition.nodes.find(spec => spec.id === node.node_id)) || ''));
        await Promise.all(reviewNodes.map(async node => {
          if (outcomeCache.has(node.output_ref!)) return;
          try {
            const artifact = await api<{ content: string }>(`/api/artifacts/${node.output_ref!.replace('artifact://sha256/', '')}`);
            outcomeCache.set(node.output_ref!, reviewOutcome(artifact.content));
          } catch { /* Keep polling execution state even if an outcome is temporarily unavailable. */ }
        }));
        if (active) {
          // Every request above observes the API server clock. Refresh the
          // display clock in the same render as the snapshot so a newly
          // acquired lease cannot flash the uncorrected browser elapsed time.
          setNow(serverNow());
          setSnapshot({ run, version, objective: task.objective, nodes, leases, decisions, operations, contexts, diagnostics, progress, outcomes: Object.fromEntries(outcomeCache) });
          setLastSync(Date.now()); setError('');
        }
      } catch (cause) { if (active) setError((cause as Error).message); }
      finally { if (active) timer = window.setTimeout(() => void poll(), 3000); }
    }
    void poll();
    return () => { active = false; window.clearTimeout(timer); };
  }, [api, selected]);
  useEffect(() => { const timer = window.setInterval(() => setNow(serverNow()), 1000); return () => window.clearInterval(timer); }, []);

  const version = snapshot?.version ?? versions[0];
  // While composing/admitting a new Run, never render the previous Run's
  // execution snapshot. This prevents stale elapsed times and node states
  // from appearing during the short admission window.
  const liveSnapshot = starting ? null : snapshot;
  const latest = useMemo(() => latestNodes(liveSnapshot?.nodes ?? []), [liveSnapshot?.nodes]);
  const activeNodes = [...latest.values()].filter(node => ['running', 'ready', 'retrying', 'waiting_approval', 'waiting_event', 'failed'].includes(node.status));
  const stalled = liveSnapshot?.leases.some(item => item.state !== 'healthy') || [...latest.values()].some(node => nodeState(node, liveSnapshot?.leases ?? []) === 'stalled');
  const runState = liveSnapshot ? (liveSnapshot.run.status === 'running' && stalled ? 'stalled' : liveSnapshot.run.status) : '';
  const graph = useMemo(() => version ? project({ definition: version.definition, layout }, null, true) : { nodes: [], edges: [] }, [version, layout]);
  const gateId = version?.definition.nodes.find(spec => academicRole(spec) === 'review_gate')?.id;
  const completedChecks = liveSnapshot?.nodes.filter(node => node.node_id === gateId && node.status === 'completed').length ?? 0;
  const plannerId = version?.definition.nodes.find(spec => academicRole(spec) === 'planner')?.id;
  const planner = plannerId ? latest.get(plannerId) : undefined;
  const displayFor = (state: ExecutionNode | undefined, spec: NodeSpec | undefined) => {
    const role = academicRole(spec);
    const historical = state && planner && new Date(planner.created_at) > new Date(state.updated_at);
    if (state?.status === 'completed' && ['reviewer', 'review_gate'].includes(role || '')) {
      const outcome = state.output_ref === contentRef ? reviewOutcome(content) : liveSnapshot?.outcomes[state.output_ref || ''];
      const display = reviewDisplay(outcome, role === 'review_gate');
      return { ...display, statusLabel: `${historical ? '上次：' : ''}${display.statusLabel}` };
    }
    if (state?.status === 'completed' && role === 'researcher' && historical) {
      return { state: 'completed', statusLabel: '上次：已完成', detail: '' };
    }
    if (state?.status === 'skipped' && liveSnapshot) {
      const ongoing = !terminal(liveSnapshot.run.status);
      return { state: ongoing ? 'pending' : 'skipped', statusLabel: ongoing || historical ? '上次分支未执行' : '未执行',
        detail: role === 'markdown_report' ? '未导出最终报告' : ongoing ? '当前运行尚未结束' : '运行已结束' };
    }
    return { state: nodeState(state, liveSnapshot?.leases ?? []), statusLabel: undefined, detail: '' };
  };
  const nodes: ExecutionFlowNode[] = graph.nodes.map(node => {
    const state = latest.get(node.id);
    const prior = liveSnapshot?.nodes.find(item => item.node_id === node.id && item.attempt === (state?.attempt ?? 0) - 1);
    const retrying = Boolean(state && state.status === 'ready' && prior?.status === 'failed'
      && ['agent_timeout', 'agent_execution_failed'].includes(prior.error_code || ''));
    const lease = liveSnapshot?.leases.find(item => item.lease.node_run_id === state?.id);
    const ops = liveSnapshot?.operations.filter(item => item.node_run_id === state?.id) ?? [];
    const searches = ops.filter(item => item.tool_ref === 'scholarly.search' && item.status === 'succeeded').length;
    const reads = ops.filter(item => item.tool_ref === 'scholarly.read' && item.status === 'succeeded').length;
    const searchFailures = ops.filter(item => item.tool_ref === 'scholarly.search' && item.status === 'failed').length;
    const readFailures = ops.filter(item => item.tool_ref === 'scholarly.read' && item.status === 'failed').length;
    const activity = searches || reads || searchFailures || readFailures
      ? `检索 ${searches} 成功/${searchFailures} 失败 · 阅读 ${reads} 成功/${readFailures} 失败` : '';
    const display = displayFor(state, node.data.spec);
    const detail = retrying ? '等待失败后重试' : display.detail || (state?.status === 'failed' ? label(state.error_code || 'failed') : '') || activity || (lease ? `已执行 ${elapsed(lease.lease.acquired_at, now)}` : state ? `更新 ${new Date(state.updated_at).toLocaleTimeString()}` : '等待依赖');
    return { ...node, type: 'execution', width: 240, height: 146, selected: node.id === nodeId,
      position: layout.positions?.[node.id] ?? { x: 70 + graph.nodes.indexOf(node) % 3 * 300, y: 60 + Math.floor(graph.nodes.indexOf(node) / 3) * 210 },
      data: { name: node.data.spec.name, kind: node.data.spec.type, state: retrying ? 'retrying' : display.state, statusLabel: display.statusLabel, detail,
        attempt: state && !['pending', 'skipped'].includes(state.status) ? state.attempt : undefined } };
  });
  const edges = useMemo(() => graph.edges.map((edge, index) => {
    const decision = decisionFor(liveSnapshot?.decisions ?? [], index, latest.get(edge.source)?.attempt);
    // Keep the full graph topology visible even when a Run fails or an edge
    // was not selected. Routing evidence is conveyed by color, not by hiding
    // the underlying connection.
    const color = decision?.selected ? '#167568' : decision ? '#8d9e99' : '#829b96';
    return { ...edge, label: decision ? (decision.selected ? '已通过' : '未选择') : edge.label,
      style: { stroke: color, strokeWidth: decision?.selected ? 2.5 : 1.7, opacity: 1 },
      markerEnd: { type: MarkerType.ArrowClosed, color } };
  }), [graph.edges, liveSnapshot?.decisions, latest]);
  const history = (liveSnapshot?.nodes.filter(node => node.node_id === nodeId) ?? []).sort((a, b) => b.attempt - a.attempt);
  const node = history.find(item => item.attempt === attempt) ?? history[0];
  const nodeDisplay = displayFor(node, version?.definition.nodes.find(spec => spec.id === nodeId));
  const context = liveSnapshot?.contexts.find(item => item.node_run_id === node?.id);
  const lease = liveSnapshot?.leases.find(item => item.lease.node_run_id === node?.id);
  const nodeOps = liveSnapshot?.operations.filter(item => item.node_run_id === node?.id) ?? [];
  useEffect(() => {
    let active = true;
    setContent(''); setContentRef('');
    if (node?.output_ref) void api<{ content: string }>(`/api/artifacts/${node.output_ref.replace('artifact://sha256/', '')}`)
      .then(value => { if (active) { setContent(value.content); setContentRef(node.output_ref!); } }).catch(cause => { if (active) setError(cause.message); });
    return () => { active = false; };
  }, [api, node?.id, node?.output_ref]);

  async function start() {
    setBusy(true); setError('');
    try {
      const parsed = JSON.parse(inputs);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('输入必须是 JSON 对象');
      const body = JSON.stringify({ objective, inputs: graphId === 'academic-research' ? { ...parsed, topic: objective } : parsed });
      if (!submission.current || submission.current.body !== body || !submission.current.trigger) {
        const triggers = await api<{ id: string; type: string; enabled: boolean; filter_expression?: string }[]>(`/api/graph-versions/${startVersion}/triggers`);
        let trigger = triggers.find(item => item.type === 'manual' && item.enabled && !item.filter_expression)?.id;
        if (!trigger) { trigger = crypto.randomUUID(); await api(`/api/triggers/${trigger}`, 'PUT', { graph_version_id: startVersion }); }
        submission.current = { key: crypto.randomUUID(), body, trigger };
      }
      const receipt = await request<{ run_id: string }>(token, `/api/triggers/${submission.current.trigger}/runs`, 'POST', JSON.parse(body), { 'Idempotency-Key': submission.current.key });
      const run = await api<ExecutionRun>(`/api/runs/${receipt.run_id}`);
      setRuns(values => [run, ...values.filter(item => item.id !== run.id)]); setSelected(run.id); setStarting(false); submission.current = null;
    } catch (cause) { setError((cause as Error).message); } finally { setBusy(false); }
  }
  async function download(ref: string, name: string, draft = false) {
    try {
      const result = await api<{ content: string }>(`/api/artifacts/${ref.replace('artifact://sha256/', '')}`);
      let text = result.content;
      if (draft) {
        try { const work = JSON.parse(text); if (typeof work.manuscript === 'string') text = work.manuscript; } catch { /* Keep malformed output verbatim as evidence. */ }
        text = '# 未通过评审的草稿\n\n此文件不是最终报告。引用和结论可能尚未核实。\n\n' + text;
      }
      const url = URL.createObjectURL(new Blob([text], { type: 'text/markdown;charset=utf-8' }));
      const link = document.createElement('a'); link.href = url; link.download = name; link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (cause) { setError((cause as Error).message); }
  }
  const report = latest.get('report');
  const draft = latest.get('research');
  return <section className="graph-execution" aria-label="图上执行">
    <div className="execution-toolbar"><select aria-label="选择运行" value={selected} onChange={event => setSelected(event.target.value)}><option value="">暂无运行</option>{runs.map(item => <option key={item.id} value={item.id}>{new Date(item.created_at).toLocaleString()} · {label(item.id === selected ? runState || item.status : item.status)} · {item.id.slice(0, 8)}</option>)}</select>
      <button className="primary" disabled={!versions.length || busy} onClick={() => {
        const currentObjective = snapshot?.objective ?? '';
        // A new run has not been admitted yet. Clear the previous snapshot so
        // its elapsed time cannot be mistaken for the run being configured.
        setStarting(true); setSelected(''); setSnapshot(null); setNodeId(''); setAttempt(null); setObjective(currentObjective);
        setInputs(graphId === 'academic-research' ? JSON.stringify({ topic: currentObjective, language: 'Chinese', minimum_sources: 12, minimum_reads: 5 }, null, 2) : '{}');
      }}><Play size={15} />发起运行</button>
      {liveSnapshot?.run.status === 'completed' && report?.output_ref && <button onClick={() => void download(report.output_ref!, `${selected}.md`)}><Download size={15} />报告</button>}
      {draft?.output_ref && liveSnapshot?.run.status !== 'completed' && <button onClick={() => void download(draft.output_ref!, `${selected}-unapproved.md`, true)}><Download size={15} />未审定草稿</button>}
      {liveSnapshot && !terminal(liveSnapshot.run.status) && <button title="停止运行" aria-label="停止运行" disabled={busy} onClick={async () => {
        if (!window.confirm('停止此运行并保留已有材料？已发出的外部请求可能仍会完成。')) return;
        setBusy(true); try { const run = await api<ExecutionRun>(`/api/runs/${selected}/stop`, 'POST', { reason: 'User stopped from execution graph' }); setSnapshot(value => value ? { ...value, run } : value); } catch (cause) { setError((cause as Error).message); } finally { setBusy(false); }
      }}><Square size={15} /></button>}
    </div>
    {error && <div className="message error" role="alert">{error}</div>}
    {starting && <form className="execution-start" onSubmit={event => { event.preventDefault(); void start(); }}>
      <label>发布版本<select value={startVersion} onChange={event => { setStartVersion(event.target.value); submission.current = null; }}>{versions.map(item => <option key={item.graph_version_id} value={item.graph_version_id}>v{item.version} · {item.definition.name}</option>)}</select></label>
      <label>研究主题 / 任务目标<input required value={objective} onChange={event => setObjective(event.target.value)} /></label>
      <label>运行输入 JSON<textarea aria-label="运行输入 JSON" value={inputs} onChange={event => setInputs(event.target.value)} /></label>
      <div><button className="primary" disabled={busy || !objective.trim()}><Play size={15} />{busy ? '提交中' : '确认运行'}</button><button type="button" disabled={busy} onClick={() => setStarting(false)}><X size={15} />取消</button></div>
    </form>}
    {liveSnapshot && <div className={`execution-summary state-${runState}`} aria-live="polite"><strong>{label(runState)}</strong><span>v{version?.version}</span>{gateId && <span>已完成 {completedChecks} 轮证据检查</span>}<span>总耗时 {elapsed(liveSnapshot.run.created_at, terminal(liveSnapshot.run.status) ? liveSnapshot.run.updated_at : now)}</span><span>{activeNodes.map(item => version?.definition.nodes.find(def => def.id === item.node_id)?.name ?? item.node_id).join(' · ')}</span><small>{error ? '同步失败，显示上次状态' : lastSync ? `同步于 ${new Date(lastSync).toLocaleTimeString()}` : '同步中'}</small></div>}
    {liveSnapshot && <div className="execution-objective" title={liveSnapshot.objective}>{liveSnapshot.objective}</div>}
    {liveSnapshot && (liveSnapshot.diagnostics.length > 0 || liveSnapshot.progress.length > 0) && <div className="execution-signals">
      {liveSnapshot.diagnostics.map(item => <div className="signal-row" key={item.diagnostic_id}><strong>诊断</strong><span>{item.reason}</span></div>)}
      {liveSnapshot.progress.length > 0 && (() => { const latestEvidence = liveSnapshot.progress[liveSnapshot.progress.length - 1]; return <div className="signal-row"><strong>进展证据</strong><span>r{latestEvidence.state_revision} · {latestEvidence.phase} · 已验证 {latestEvidence.verified_progress_refs.length} · 循环 {latestEvidence.cycle_iteration}</span></div>; })()}
    </div>}
    {!!version?.definition.metadata && <div className="execution-budget">{(() => { const meta = version.definition.metadata as Record<string, string>; return meta.run_timeout_seconds ? `显式运营预算 ${Number(meta.run_timeout_seconds) / 60} 分钟 · 最多 ${meta.max_rounds || '?'} 轮` : '无显式运营预算；健康运行不因预设轮数或时长终止'; })()}</div>}
    {!version ? <div className="run-placeholder"><FileText size={30} /><h2>尚无发布版本</h2></div> : <div className={`execution-body ${nodeId ? 'has-detail' : ''}`}>
      <ExecutionCanvas
        instanceKey={`${version.graph_version_id}:${liveSnapshot?.run.id ?? 'no-run'}`}
        nodes={nodes}
        edges={edges}
        onSelectNode={value => { setNodeId(value); setAttempt(null); }}
      />
      {nodeId && liveSnapshot && <aside className="execution-inspector" aria-label="执行详情"><header><h2>{version.definition.nodes.find(item => item.id === nodeId)?.name}</h2><button className="icon-button" aria-label="关闭执行详情" title="关闭执行详情" onClick={() => setNodeId('')}><X size={16} /></button></header>
        <label>执行记录<select value={node?.attempt ?? ''} onChange={event => setAttempt(Number(event.target.value))}>{history.map(item => <option key={item.id} value={item.attempt}>{executionAttempt(item, history)} · {label(item.status)}</option>)}</select></label>
        <p className={`execution-detail-state state-${nodeDisplay.state}`}>{nodeDisplay.statusLabel || label(nodeDisplay.state)}</p>
        {nodeDisplay.detail && <p>{nodeDisplay.detail}</p>}
        {node?.error_code && <p className="inline-error">{node.error_code}</p>}
        {lease && <p>最近心跳 {new Date(lease.lease.heartbeat_at).toLocaleTimeString()}{lease.state !== 'healthy' && ' · 执行可能已经中断'}</p>}
        {lease?.recoverable && <button disabled={busy} onClick={async () => {
          if (!window.confirm('仅在确认原 worker 已停止后恢复。确认恢复此节点？')) return;
          setBusy(true); try { await api(`/api/leases/${lease.lease.claim_id}/recover`, 'POST', { reason: 'Operator confirmed stopped worker from execution graph' }); } catch (cause) { setError((cause as Error).message); } finally { setBusy(false); }
        }}><RefreshCw size={15} />恢复节点</button>}
        {node?.status === 'waiting_approval' && <button disabled={busy} onClick={async () => {
          const reason = window.prompt('处理意见 / 批准理由'); if (!reason) return;
          setBusy(true); try { await api(`/api/waits/${node.id}/approve`, 'POST', { reason, actor: 'web-operator' }); } catch (cause) { setError((cause as Error).message); } finally { setBusy(false); }
        }}><CheckCircle2 size={15} />提交处理意见</button>}
        <details><summary>输入上下文</summary><pre>{context ? JSON.stringify(context.snapshot, null, 2) : '尚无已持久化上下文'}</pre></details>
        <details open><summary>节点输出</summary><pre>{content || '尚无输出'}</pre></details>
        {content && <button onClick={() => void download(node!.output_ref!, `${nodeId}-${node!.attempt}.txt`)}><Download size={15} />下载节点输出</button>}
        <details open><summary>检索与读取记录 · {nodeOps.length}</summary>{nodeOps.map(item => <div className="execution-operation" key={item.operation_id}><strong>{item.tool_ref}</strong><span>{label(item.status)} · {new Date(item.updated_at).toLocaleTimeString()}</span>{item.error_code && <small>{label(item.error_code)}</small>}{item.result_ref && <button className="inline-link" onClick={() => void download(item.result_ref!, `${item.operation_id}.json`)}>下载证据</button>}</div>)}</details>
      </aside>}
    </div>}
  </section>;
}
