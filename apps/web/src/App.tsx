/** Workbench composition and graph editing. Run inspection and shared dialogs own their local UI
 * state; the model adapter and canvas layout stay independent of the page. */

import {
  Anchor, Activity, CheckCheck, ChevronDown, Copy, Download, GitBranch, MessageSquare, Plus, Redo2, Save, Trash2, Undo2, Upload,
  Play, Search, SlidersHorizontal, FolderOpen, PanelLeftClose, PanelRightClose,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type MouseEvent } from 'react';
import { WorkflowCanvas } from './WorkflowCanvas';
import { GraphActions } from './GraphActions';
import { label } from './execution';
import {
  type OurAgent, type OurGraph, type OurRun, type OurRunDetail,
  type TimelineData,
} from './model';
import { validateCallTargets } from './calls';
import { CallEditor } from './CallEditor';
import { GraphRelations } from './GraphRelations';
import { RunInspector } from './RunInspector';
import { Plugins } from './Plugins';
import { Workspace } from './Workspace';
import { Pilot } from './Pilot';
import { anchorRef, anchorTarget, type AnchorRef } from './links';
import { api, registerApiKeyPrompt, setBearerKey } from './api';
import { EmptyState, JsonDialog, Modal, ToolButton } from './ui';
import { graphColor, Timeline } from './Timeline';

const POLL_MS = 3000;
type Pick = { kind: 'node' | 'edge' | 'agent'; id: string } | null;

const recentRuns = (runs: OurRun[], graph: string) => runs
  .filter(item => item.graph === graph)
  .sort((a, b) => Date.parse(b.updated || b.started) - Date.parse(a.updated || a.started));

const runTime = (value: string) => {
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? '时间未知' : date.toLocaleString('zh-CN', {
    month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit', hour12: false,
  });
};


export function App() {
  const [search, setSearch] = useState('');
  const [libraryOpen, setLibraryOpen] = useState(true);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [view, setView] = useState<'graph' | 'runs' | 'runDetail' | 'pilot'>(() => {
    try {
      const saved = localStorage.getItem('anchor:view');
      return saved === 'pilot' || saved === 'runs' ? saved : 'graph';
    } catch { return 'graph'; }
  });
  useEffect(() => {
    try { localStorage.setItem('anchor:view', view === 'runDetail' ? 'runs' : view); }
    catch { /* Navigation still works without browser storage. */ }
  }, [view]);
  const [graphs, setGraphs] = useState<{ graph: string; running: string | null; active_runs?: string[] }[]>([]);
  const [relations, setRelations] = useState(false);
  const pendingPick = useRef<string>('');
  const [graphReturn, setGraphReturn] = useState<{ graph: string; node?: string } | null>(null);
  const [runs, setRuns] = useState<OurRun[]>([]);
  const [timeline, setTimeline] = useState<TimelineData | null>(null);
  const [timelinePage, setTimelinePage] = useState(0);
  const [name, setName] = useState('');
  const [doc, setDoc] = useState<OurGraph | null>(null);
  const [savedDoc, setSavedDoc] = useState('');
  const [past, setPast] = useState<OurGraph[]>([]);
  const [future, setFuture] = useState<OurGraph[]>([]);
  const [pick, setPick] = useState<Pick>(null);
  const [palette, setPalette] = useState(false);
  const [json, setJson] = useState<'graph' | 'node' | 'edge' | 'agent' | null>(null);
  const [connect, setConnect] = useState({ source: '', target: '' });
  const [run, setRun] = useState('');
  const [detail, setDetail] = useState<OurRunDetail | null>(null);
  const [node, setNode] = useState('');
  const [targetPath, setTargetPath] = useState('');
  const [notice, setNotice] = useState<{ kind: 'ok' | 'bad'; text: string } | null>(null);
  const [problem, setProblem] = useState('');
  const [busy, setBusy] = useState(false);
  const [apiKeyDialog, setApiKeyDialog] = useState(false);
  const [apiKeyDraft, setApiKeyDraft] = useState('');
  const apiKeyResolver = useRef<((key: string) => void) | null>(null);
  // Which Pilot session to go back to after opening a Graph, Run or artifact from the chat.
  const [chat, setChat] = useState('');
  const nameRef = useRef(name); nameRef.current = name;
  const upload = useRef<HTMLInputElement>(null);
  const newGraphButton = useRef<HTMLButtonElement>(null);
  const runHistory = useRef<HTMLDivElement>(null);

  useEffect(() => registerApiKeyPrompt(() => new Promise(resolve => {
    apiKeyResolver.current = resolve;
    setApiKeyDraft('');
    setApiKeyDialog(true);
  })), []);

  const finishApiKeyPrompt = (key: string) => {
    if (key) setBearerKey(key);
    apiKeyResolver.current?.(key);
    apiKeyResolver.current = null;
    setApiKeyDialog(false);
  };

  useEffect(() => {
    const dismiss = () => runHistory.current?.hidePopover();
    window.addEventListener('resize', dismiss);
    return () => window.removeEventListener('resize', dismiss);
  }, []);

  const dirty = doc !== null && JSON.stringify(doc, null, 2) + '\n' !== savedDoc;
  const editable = doc !== null && !busy;
  const graphRuns = useMemo(() => recentRuns(runs, name), [runs, name]);
  const activeRun = graphRuns.find(item => item.running) ?? graphRuns[0];

  useEffect(() => {
    if (!dirty) return;
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ''; };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty]);

  const selectGraph = (next: string): boolean => {
    if (next === name) return true;
    if (dirty && !window.confirm('当前工作流有未保存的修改，确定切换并放弃修改吗？')) return false;
    setName(next);
    setRun(recentRuns(runs, next)[0]?.run ?? '');
    setNode('');
    setTargetPath('');
    return true;
  };

  const refresh = useCallback(async () => {
    try {
      const [graphList, runList] = await Promise.all([
        api<{ graphs: { graph: string; running: string | null; active_runs?: string[] }[] }>('/graphs'),
        api<{ runs: OurRun[] }>('/runs'),
      ]);
      const cutoff = new Date(); cutoff.setHours(0, 0, 0, 0); cutoff.setDate(cutoff.getDate() + 1 - timelinePage * 30);
      const before = `${cutoff.getFullYear()}-${String(cutoff.getMonth() + 1).padStart(2, '0')}-${String(cutoff.getDate()).padStart(2, '0')}`;
      const board = await api<TimelineData>(`/timeline?days=30&before=${before}`);
      setGraphs(graphList.graphs);
      setRuns(runList.runs);
      setTimeline(board);
      setProblem('');
      if (!nameRef.current) {
        const initial = runList.runs.find(item => item.running)?.graph ?? graphList.graphs[0]?.graph;
        if (initial) {
          setName(initial);
          setRun(runList.runs.find(item => item.graph === initial)?.run ?? '');
        }
      }
    } catch (error) {
      const message = (error as Error).message;
      setProblem(error instanceof TypeError || /fetch|network|ECONNREFUSED|proxy error/i.test(message)
        ? '后端 API 暂不可达，请检查 8077 服务及前端代理。'
        : `后端请求失败：${message}`);
    }
  }, [timelinePage]);

  useEffect(() => { void refresh(); }, [refresh]);
  useEffect(() => {
    const timer = window.setInterval(() => { void refresh(); }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [refresh]);

  useEffect(() => {
    if (!name) { setDoc(null); return; }
    let active = true;
    void api<{ definition: OurGraph }>(`/graphs/${encodeURIComponent(name)}`)
      .then(value => {
        if (!active) return;
        setDoc(value.definition);
        setSavedDoc(JSON.stringify(value.definition, null, 2) + '\n');
        setPast([]); setFuture([]); setPick(pendingPick.current ? { kind: 'node', id: pendingPick.current } : null); pendingPick.current = ''; setNotice(null);
      })
      .catch(() => { if (active) setDoc(null); });
    return () => { active = false; };
  }, [name]);

  useEffect(() => {
    if (!run) { setDetail(null); return; }
    let active = true;
    void api<OurRunDetail>(`/runs/${encodeURIComponent(run)}`).then(value => { if (active) setDetail(value); })
      .catch(() => { if (active) setDetail(null); });
    return () => { active = false; };
  }, [run, runs]);

  /** Every edit goes through here, which is what makes undo one place instead of thirty. */
  const patch = (next: OurGraph) => {
    if (!doc) return;
    setPast(history => [...history, doc]);
    setFuture([]);
    setDoc(next);
    setNotice(null);
  };

  const perform = async (what: string, action: () => Promise<void>) => {
    setBusy(true); setNotice(null);
    try { await action(); }
    catch (error) { setNotice({ kind: 'bad', text: `${what}失败：${(error as Error).message}` }); }
    finally { setBusy(false); }
  };

  const save = () => perform('保存', async () => {
    if (!doc) return;
    validateCallTargets(doc, graphs.map(item => item.graph));
    await api(`/graphs/${encodeURIComponent(name)}`, 'PUT', { definition: doc });
    setSavedDoc(JSON.stringify(doc, null, 2) + '\n');
    setNotice({ kind: 'ok', text: '已保存。' });
  });

  // The server validates on PUT; this action also saves and must say so.
  const check = () => perform('校验', async () => {
    if (!doc) return;
    validateCallTargets(doc, graphs.map(item => item.graph));
    await api(`/graphs/${encodeURIComponent(name)}`, 'PUT', { definition: doc });
    setSavedDoc(JSON.stringify(doc, null, 2) + '\n');
    setNotice({ kind: 'ok', text: '校验通过，工作流已保存。' });
  });

  const create = () => perform('新建', async () => {
    const wanted = window.prompt('新图的名字（会成为工作区目录名）：');
    if (!wanted) return;
    await api('/graphs', 'POST', { name: wanted });
    await refresh();
    setName(wanted); setView('graph');
  });

  const openReference = (ref: AnchorRef) => {
    const graph = ref.kind === 'graph' ? '' : ref.run;
    const target = anchorTarget(ref, runs.find(item => item.run === graph)?.graph ?? nameRef.current);
    if (target.graph && !selectGraph(target.graph)) return;
    setRun(target.run); setNode(target.node); setTargetPath('path' in target ? target.path ?? '' : '');
    setView(target.view === 'runs' && target.run ? 'runDetail' : target.view);
    if (target.view === 'runs' && !runs.some(item => item.run === target.run)) void refresh();
  };

  // A Pilot reply links its objects with `#anchor/…`; the click opens the page that already shows
  // them, and the way back is the session it was opened from. Captured on the shell so every
  // rendered reply is covered without teaching the Markdown renderer about Anchor.
  const followReference = (event: MouseEvent<HTMLElement>) => {
    const element = event.target as HTMLElement | null;
    const link = element?.closest?.('a');
    const ref = anchorRef(link?.getAttribute('href'));
    if (!ref) return;
    event.preventDefault();
    openReference(ref);
  };

  const trigger = () => perform('触发', async () => {
    if (name === 'weekly-work-report') {
      const body = await api<{ run: string }>('/trigger', 'POST', { graph: name, input: doc?.input ?? {} });
      setRun(body.run); setView('runDetail');
      return;
    }
    const raw = window.prompt('本次运行输入（JSON object，可留空）', JSON.stringify(doc?.input ?? {}, null, 2));
    if (raw === null) return;
    let input: Record<string, unknown>;
    try { input = raw.trim() ? JSON.parse(raw) : {}; }
    catch { throw new Error('运行输入必须是有效 JSON'); }
    if (!input || Array.isArray(input) || typeof input !== 'object') throw new Error('运行输入必须是 JSON object');
    const body = await api<{ run: string }>('/trigger', 'POST', { graph: name, input });
    setRun(body.run); setView('runDetail');
  });

  const openRun = (item: OurRun) => {
    if (item.graph !== name && !selectGraph(item.graph)) return;
    setRun(item.run); setNode(''); setTargetPath(''); setView('runDetail');
  };

  const navigateGraph = (target: string, targetNode?: string, remember = false, sourceNode?: string) => {
    const previous = { graph: name, node: sourceNode ?? selectedNode?.id };
    if (!selectGraph(target)) return;
    if (remember) setGraphReturn(previous);
    pendingPick.current = target === name ? '' : targetNode ?? '';
    setPick(targetNode ? { kind: 'node', id: targetNode } : null);
    setInspectorOpen(true); setView('graph'); setRelations(false);
  };
  const navigateRun = async (targetRun: string, targetGraph: string, targetNode = '') => {
    if (!selectGraph(targetGraph)) return;
    setDetail(null); setRun(targetRun); setNode(targetNode); setTargetPath(''); setView('runDetail');
  };

  const controlRun = (what: 'pause' | 'stop' | 'resume') => perform(what, async () => {
    await api(`/runs/${run}/${what}`, 'POST');
    await refresh();
  });

  const deleteRun = () => perform('删除运行记录', async () => {
    if (!run || !window.confirm(`确定彻底删除运行记录 ${run} 及其所有文件吗？此操作不可恢复。`)) return;
    await api(`/runs/${encodeURIComponent(run)}`, 'DELETE');
    setRun(''); setDetail(null); setNode('');
    await refresh();
    setNotice({ kind: 'ok', text: '运行记录及其文件已删除。' });
  });

  const deleteGraph = async (target: string) => {
    setBusy(true);
    try {
      await api(`/graphs/${encodeURIComponent(target)}`, 'DELETE');
      setGraphs(items => items.filter(item => item.graph !== target));
      setRuns(items => items.filter(item => item.graph !== target));
      if (nameRef.current === target) {
        const next = graphs.find(item => item.graph !== target)?.graph ?? '';
        nameRef.current = next;
        setName(next); setRun(runs.find(item => item.graph === next)?.run ?? '');
        setDetail(null); setNode(''); setDoc(null); setSavedDoc('');
        setPast([]); setFuture([]); setPick(null); setJson(null); setPalette(false);
      }
      setNotice({ kind: 'ok', text: `工作流“${target}”及其运行历史已删除。` });
      await refresh();
    } finally { setBusy(false); }
    requestAnimationFrame(() => newGraphButton.current?.focus());
  };

  const selectedNode = pick?.kind === 'node' ? doc?.nodes.find(item => item.id === pick.id) : undefined;
  const selectedAgent = pick?.kind === 'agent' && doc ? doc.agents?.[pick.id] : undefined;
  const selectedEdge = pick?.kind === 'edge' && doc ? doc.edges[Number(pick.id)] : undefined;

  const patchNode = (fields: Partial<{ id: string; agent: string; with: string; plugins: string[] }>) => {
    if (!doc || !selectedNode) return;
    patch({ ...doc, nodes: doc.nodes.map(item =>
      item.id === selectedNode.id ? { ...item, ...fields } : item) });
  };
  const patchAgent = (fields: Partial<{ model: string; network: boolean; instructions: string }>) => {
    if (!doc || pick?.kind !== 'agent') return;
    const current = doc.agents?.[pick.id] ?? { model: '' };
    patch({ ...doc, agents: { ...doc.agents, [pick.id]: { ...current, ...fields } } });
  };
  const patchEdge = (fields: Partial<{ from: string; to: string }>) => {
    if (!doc || pick?.kind !== 'edge') return;
    const at = Number(pick.id);
    patch({ ...doc, edges: doc.edges.map((item, index) =>
      index === at ? { ...item, ...fields } : item) });
  };

  const addNode = (agentName: string) => {
    if (!doc) return;
    let id = 'node';
    let suffix = 2;
    while (doc.nodes.some(item => item.id === id)) id = `node${suffix++}`;
    patch({ ...doc, nodes: [...doc.nodes, { id, agent: agentName }] });
    setPalette(false);
    setPick({ kind: 'node', id });
  };

  const addOperation = (calling: boolean) => {
    if (!doc) return;
    let id = calling ? 'call' : 'command';
    let suffix = 2;
    while (doc.nodes.some(item => item.id === id) || doc.ops?.[id]) id = `${calling ? 'call' : 'command'}${suffix++}`;
    patch({ ...doc, entry: doc.entry || id, nodes: [...doc.nodes, { id, op: id }], ops: { ...doc.ops,
      [id]: calling ? { call: { graph: graphs.find(item => item.graph !== name)?.graph ?? '', mode: 'wait' } } : { run: 'true' } } });
    setPalette(false); setPick({ kind: 'node', id }); setInspectorOpen(true);
  };

  const addAgent = () => {
    if (!doc) return;
    let id = 'agent';
    let suffix = 2;
    while (doc.agents?.[id]) id = `agent${suffix++}`;
    patch({ ...doc, agents: { ...doc.agents,
      [id]: { model: 'models.academic', network: false, instructions: '' } } });
    setPick({ kind: 'agent', id });
  };

  const duplicateNode = () => {
    if (!doc || !selectedNode) return;
    let id = `${selectedNode.id}-copy`;
    let suffix = 2;
    while (doc.nodes.some(item => item.id === id)) id = `${selectedNode.id}-copy${suffix++}`;
    const at = doc.layout?.positions?.[selectedNode.id];
    patch({ ...doc,
      nodes: [...doc.nodes, { ...selectedNode, id }],
      layout: at ? { ...doc.layout,
        positions: { ...doc.layout?.positions, [id]: { x: at.x + 45, y: at.y + 140 } } } : doc.layout });
    setPick({ kind: 'node', id });
  };

  const removePicked = () => {
    if (!doc || !pick) return;
    if (pick.kind === 'node') {
      patch({ ...doc, nodes: doc.nodes.filter(item => item.id !== pick.id),
              edges: doc.edges.filter(edge => edge.from !== pick.id && edge.to !== pick.id) });
    } else if (pick.kind === 'edge') {
      patch({ ...doc, edges: doc.edges.filter((_, index) => index !== Number(pick.id)) });
    } else {
      const used = doc.nodes.some(item => item.agent === pick.id);
      if (used) { setNotice({ kind: 'bad', text: '还有节点在用这个角色，先改掉它们。' }); return; }
      const agents = { ...doc.agents };
      delete agents[pick.id];
      patch({ ...doc, agents });
    }
    setPick(null);
  };

  const entry = doc?.entry ?? '';
  const routing = useMemo(() => {
    if (!doc) return new Set<string>();
    const counts: Record<string, number> = {};
    for (const edge of doc.edges) counts[edge.from] = (counts[edge.from] ?? 0) + 1;
    return new Set(Object.keys(counts).filter(id => counts[id] > 1));
  }, [doc]);

  return (
    <div className={`app-shell ${libraryOpen ? '' : 'hide-library'} ${inspectorOpen ? '' : 'hide-inspector'}`}
         onClickCapture={followReference}>
      <header className="topbar">
        <div className="brand"><span className="brand-mark"><Anchor size={22} /></span>Anchor<span className="brand-caption">WORKSPACE</span></div>
        <nav className="product-switch" aria-label="工作台">
          <button aria-pressed={view === 'graph'} className={view === 'graph' ? 'chosen' : ''} onClick={() => setView('graph')}><GitBranch size={15} />图编排</button>
          <button aria-pressed={view === 'runs' || view === 'runDetail'} className={view === 'runs' || view === 'runDetail' ? 'chosen' : ''} onClick={() => setView('runs')}><Activity size={15} />运行看板</button>
          <button aria-pressed={view === 'pilot'} className={view === 'pilot' ? 'chosen' : ''} onClick={() => setView('pilot')}><MessageSquare size={15} />Pilot</button>
        </nav>
        <div className="connection" title={problem || '服务已连接'}><span className={`status-dot ${problem ? 'bad' : ''}`} />
          {problem ? '连接中断' : '服务在线'}
        </div>
        {view !== 'pilot' && chat && <button onClick={() => setView('pilot')} title={`回到会话 ${chat}`}>
          <MessageSquare size={14} />返回会话
        </button>}
        {(view === 'graph' || view === 'runDetail') && <div className="panel-toggles">
          <ToolButton icon={PanelLeftClose} label="切换侧边栏" aria-pressed={libraryOpen} onClick={() => setLibraryOpen(!libraryOpen)} />
          <ToolButton icon={PanelRightClose} label="切换详情面板" aria-pressed={inspectorOpen} onClick={() => setInspectorOpen(!inspectorOpen)} />
        </div>}
      </header>
      {problem && <div className="connection-error" role="alert">{problem}</div>}
      {view === 'runs' && notice && <p className={`notice ${notice.kind}`} role="status">{notice.text}</p>}

      {view === 'pilot' ? <Pilot session={chat} onSession={setChat} /> : view === 'runs' ? (
        <Workspace running>
          <Timeline data={timeline} graphs={graphs.map(item => item.graph)} page={timelinePage} onPage={setTimelinePage}
            onRefresh={() => void refresh()} onSelect={item => {
            setRun(item.run); setName(item.graph); setNode(''); setView('runDetail');
          }} />
        </Workspace>
      ) : view === 'runDetail' ? (
        <Workspace running>
          <main className="main timeline-inspector">
            <div className="document-header"><div className="document-title"><span className="eyebrow">EXECUTION</span><h2>{name || '运行概览'}</h2></div>
              <div className="document-context-actions">
                <select aria-label="当前工作流" value={name} onChange={event => selectGraph(event.target.value)}>
                  {graphs.map(item => <option key={item.graph} value={item.graph}>{item.graph}{item.running ? '（执行中）' : ''}</option>)}
                </select>
                <button className="primary" title={dirty ? '请先保存工作流' : '运行已保存的工作流'} onClick={() => void trigger()} disabled={busy || !name || dirty}><Play size={14} fill="currentColor" />运行工作流</button>
                <button onClick={() => setView('runs')}><Activity size={14} />返回时间线</button>
              </div>
            </div>
            {detail?.state.trigger?.source === 'graph_call' && detail.state.trigger.run && <div className="run-origin">
              由 {detail.state.trigger.graph} / {detail.state.trigger.node} · 第 {detail.state.trigger.invocation} 轮发起
              <button onClick={() => void navigateRun(detail.state.trigger!.run!, detail.state.trigger!.graph!, detail.state.trigger!.node)}>返回来源运行 ←</button>
            </div>}
            <div className="canvas-head">
              {detail ? <>
                <span className={`pill ${detail.state.status}`}>
                  {detail.state.status === 'running' ? '执行中' : label(detail.state.status)}
                </span>
                <span className="objective">{detail.state.objective}</span>
                {detail.state.cursor && <span className="hint">
                  {detail.state.status === 'running' ? '正在执行' :
                    detail.state.status === 'stopped' ? '停止于' : '中断于'} {detail.state.cursor.node}
                  （第 {detail.state.cursor.pass} 轮）</span>}
                {['running', 'paused'].includes(detail.state.status) &&
                  <span className="run-controls">
                    {detail.state.status === 'running' ? <>
                      <button onClick={() => void controlRun('pause')} disabled={busy}
                              title="当前节点完成后暂停">暂停</button>
                      <button className="danger-link" onClick={() => void controlRun('stop')}
                              disabled={busy} title="停止本次运行及等待模式创建的目标；已接纳的启动后继续调用不受影响">停止</button>
                    </> : <button onClick={() => void controlRun('resume')} disabled={busy}>
                      继续
                    </button>}
                  </span>}
                {detail.state.reason === 'asked' && <span className="hint">
                  {detail.state.status === 'paused' ? '已按请求暂停' : '已按请求停止'}
                </span>}
                {detail.state.error && <span className="problem">{detail.state.error}</span>}
                {detail.state.status !== 'running' ?
                  <button className="danger-link" onClick={() => void deleteRun()} disabled={busy}>
                    <Trash2 size={14} />删除运行记录
                  </button> : null}
              </> : <span className="hint">选一次运行，或触发一次。</span>}
            </div>
            {doc && <WorkflowCanvas graph={doc} name={name} mode="run" state={detail?.state}
              onPick={value => { if (value?.kind === 'node') { setNode(value.id); setInspectorOpen(true); } }} />}
            <div className="canvas-bottom"><span className="legend"><i className="dot running" />执行中<i className="dot finished" />已完成<i className="dot failed" />失败</span><span className="canvas-caption">点击节点查看对话与产物</span></div>
          </main>
          <RunInspector key={`${run}/${node}/${targetPath}`} run={run} node={node} detail={detail} targetPath={targetPath} onOpenRun={navigateRun} />
        </Workspace>
      ) : (
        <Workspace>
          <aside className="library">
            <div className="section-heading"><h3><FolderOpen size={15} />工作流</h3><span className="count">{graphs.length}</span></div>
            <label className="search-field"><Search size={15} /><input aria-label="搜索工作流" placeholder="搜索工作流…" value={search} onChange={event => setSearch(event.target.value)} /></label>
            <button ref={newGraphButton} className="new-graph" onClick={() => void create()} disabled={busy}>
              <Plus size={16} />新建图
            </button>
            <div className="library-list">
              {graphs.filter(item => item.graph.toLowerCase().includes(search.toLowerCase())).map(item => (
                <div key={item.graph} className={`library-item ${item.graph === name ? 'chosen' : ''}`}>
                  <button className="library-row" aria-pressed={item.graph === name}
                          onClick={() => selectGraph(item.graph)}>
                    <GitBranch size={16} /><span className="library-name" title={item.graph}>{item.graph}</span>
                    {(item.active_runs?.length || item.running) && <span className="pill running">执行中{(item.active_runs?.length ?? 0) > 1 ? ` ${item.active_runs!.length}` : ''}</span>}
                  </button>
                  <GraphActions graph={item.graph} running={Boolean(item.active_runs?.length || item.running)} busy={busy}
                                runCount={runs.filter(run => run.graph === item.graph).length} onDelete={deleteGraph} />
                </div>
              ))}
            </div>
            {!graphs.length && <EmptyState icon={GitBranch} title="从一个想法开始">新建工作流，连接你的第一个节点。</EmptyState>}
            {graphs.length > 0 && !graphs.some(item => item.graph.toLowerCase().includes(search.toLowerCase())) && <p className="hint">没有匹配的工作流。</p>}
            <div className="library-footer">
              <input ref={upload} type="file" accept="application/json,.json" hidden
                     aria-label="导入 Graph JSON"
                     onChange={event => {
                       const file = event.target.files?.[0];
                       if (!file || !doc) return;
                       void file.text().then(text => {
                         try { patch(JSON.parse(text) as OurGraph); setNotice({ kind: 'ok', text: '已导入，记得保存。' }); }
                         catch (problem) { setNotice({ kind: 'bad', text: `导入失败：${(problem as Error).message}` }); }
                       });
                     }} />
              <button onClick={() => upload.current?.click()} disabled={!editable}>
                <Upload size={15} />导入
              </button>
              <button disabled={!doc} onClick={() => {
                const blob = new Blob([JSON.stringify(doc, null, 2) + '\n'], { type: 'application/json' });
                const link = document.createElement('a');
                link.href = URL.createObjectURL(blob);
                link.download = `${name}.json`;
                link.click();
                URL.revokeObjectURL(link.href);
              }}><Download size={15} />导出</button>
            </div>
          </aside>

          <main className="main">
            <div className="document-header graph-document-header">
              <div className="document-title">
                <span className="eyebrow">WORKFLOW / EDITOR</span>
                <h2>{name || '未选择图'}</h2>
                <span className={`document-state ${dirty ? 'unsaved' : ''}`}>{dirty ? '● 未保存' : doc ? '所有更改已保存' : '选择或创建工作流'}</span>
              </div>
              <div className="document-actions">
                <button onClick={() => setRelations(true)}>工作流关系</button>
                {graphReturn && <button onClick={() => { const back = graphReturn; navigateGraph(back.graph, back.node); setGraphReturn(null); }}>返回来源工作流 ←</button>}
                <select aria-label="当前工作流" value={name} onChange={event => selectGraph(event.target.value)}>
                  {graphs.map(item => <option key={item.graph} value={item.graph}>{item.graph}{item.running ? '（执行中）' : ''}</option>)}
                </select>
                <button className="primary" title={dirty ? '请先保存工作流' : '运行已保存的工作流'} onClick={() => void trigger()} disabled={busy || !name || dirty}><Play size={14} fill="currentColor" />运行工作流</button>
                <button disabled={!editable} onClick={() => void check()}>
                  <CheckCheck size={16} />校验并保存</button>
                <button className="primary" disabled={!editable || !dirty} onClick={() => void save()}>
                  <Save size={16} />保存</button>
              </div>
              {graphRuns.filter(item => item.running).length > 1 && <div className="active-run-strip" aria-label="活动运行">
                <strong>{graphRuns.filter(item => item.running).length} 个活动运行</strong>
                {graphRuns.filter(item => item.running).map(item => <button key={item.run} onClick={() => openRun(item)}>{item.run} →</button>)}
              </div>}
              {name && <section className="graph-execution-summary" aria-label={`${name} 的执行概览`}>
                <div className="graph-execution-status">
                  <Activity size={14} aria-hidden="true" />
                  <span>{activeRun?.running ? '当前运行' : '最近运行'}</span>
                  {activeRun ? <>
                    <span className={`pill ${activeRun.running ? 'running' : activeRun.status}`}>
                      {activeRun.running ? '执行中' : label(activeRun.status)}
                    </span>
                    <time dateTime={activeRun.started} title="开始时间">{runTime(activeRun.started)} 开始</time>
                    <button className="graph-run-link" onClick={() => openRun(activeRun)}
                      aria-label={activeRun.running ? '查看当前运行' : '查看最近运行'}>查看详情 →</button>
                  </> : <span>尚未运行 · 保存后即可运行</span>}
                </div>
                {graphRuns.length > 0 && <div key={name} className="graph-run-history">
                  <button popoverTarget="graph-run-history" onClick={event => {
                    const rect = event.currentTarget.getBoundingClientRect();
                    if (runHistory.current) {
                      const width = Math.min(560, window.innerWidth - 32);
                      runHistory.current.style.left = `${Math.max(16, rect.right - width)}px`;
                      runHistory.current.style.top = `${rect.bottom + 6}px`;
                      runHistory.current.style.maxHeight = `${Math.max(120, window.innerHeight - rect.bottom - 22)}px`;
                    }
                  }}>运行历史 <span className="count">{graphRuns.length}</span><ChevronDown size={13} /></button>
                  <div ref={runHistory} id="graph-run-history" popover="auto" className="graph-run-list" aria-label="此图的运行历史">
                    <div className="graph-run-list-heading">{name}<span>{graphRuns.length} 次运行</span></div>
                    {graphRuns.map(item => <button key={item.run} className="graph-run-row" onClick={() => openRun(item)}>
                      <span className={`run-status-dot ${item.running ? 'running' : item.status}`}
                        style={{ '--graph-color': graphColor(name) } as CSSProperties} />
                      <span className="graph-run-row-main"><strong>{item.running ? '执行中' : label(item.status)}</strong><small>{runTime(item.started)} 开始</small></span>
                      <span className="graph-run-row-id" title={item.run}>{item.run}</span>
                      <span className="graph-run-row-open">查看详情 →</span>
                    </button>)}
                  </div>
                </div>}
              </section>}
            </div>
            {notice && <p className={`notice ${notice.kind}`}>{notice.text}</p>}

            <div className="canvas-top">
              <button className="add-node" disabled={!editable} onClick={() => setPalette(true)}>
                <Plus size={17} />添加节点
              </button>
              <div className="tool-group">
                <ToolButton icon={Undo2} label="撤销" disabled={!editable || !past.length}
                            onClick={() => {
                              const [last, ...rest] = [...past].reverse();
                              if (!last || !doc) return;
                              setFuture([doc, ...future]); setPast(rest.reverse()); setDoc(last);
                            }} />
                <ToolButton icon={Redo2} label="重做" disabled={!editable || !future.length}
                            onClick={() => {
                              const [first, ...rest] = future;
                              if (!first || !doc) return;
                              setPast([...past, doc]); setFuture(rest); setDoc(first);
                            }} />
              </div>
            </div>

            <div className="canvas">
              {doc
                ? <WorkflowCanvas graph={doc} name={name} mode="edit" editable={editable} targets={graphs.map(item => item.graph)} onOpenTarget={(target, sourceNode) => navigateGraph(target, undefined, true, sourceNode)}
                               onPick={value => { setPick(value); if (value) setInspectorOpen(true); }}
                               onPositions={positions => patch({ ...doc, layout: { ...doc.layout, positions } })}
                               onConnect={(from, to) => {
                                 if (doc.edges.some(edge => edge.from === from && edge.to === to)) {
                                   setNotice({ kind: 'bad', text: '这条连线已经有了。' }); return;
                                 }
                                 patch({ ...doc, edges: [...doc.edges, { from, to }] });
                               }} />
                : <EmptyState icon={GitBranch} title="编排你的下一个工作流">从左侧选择一个工作流，或新建图开始连接节点。</EmptyState>}
            </div>

            <div className="canvas-bottom">
              <span className="canvas-help">拖动节点 · 连接端点 · 滚轮缩放</span>
              <span className="canvas-caption">
                graph.json · {doc?.nodes.length ?? 0} 个节点 · {doc?.edges.length ?? 0} 条边 ·{' '}
                {Object.keys(doc?.agents ?? {}).length} 个角色
              </span>
            </div>
          </main>

          <aside className="inspector">
            <div className="section-heading"><h3><SlidersHorizontal size={15} />{pick ? '选中项属性' : '工作流设置'}</h3></div>
            <fieldset disabled={!editable}>
              {selectedNode && doc && <>
                <div className="inspector-kind">
                  节点 · {selectedNode.graph ? `模块 ${selectedNode.graph}` : selectedNode.op ?? selectedNode.agent}
                </div>
                <label>节点 ID
                  <input value={selectedNode.id} maxLength={64}
                         onChange={event => {
                           const next = event.target.value;
                           setPick({ kind: 'node', id: next });
                           patch({ ...doc,
                             nodes: doc.nodes.map(item => item.id === selectedNode.id
                               ? { ...item, id: next } : item),
                             edges: doc.edges.map(edge => ({
                               from: edge.from === selectedNode.id ? next : edge.from,
                               to: edge.to === selectedNode.id ? next : edge.to })) });
                         }} />
                </label>
                {selectedNode.op ? doc.ops?.[selectedNode.op]?.call ? <CallEditor
                  key={`${name}/${selectedNode.op}`} call={doc.ops[selectedNode.op].call!} graph={doc} node={selectedNode.id}
                  targets={graphs.map(item => item.graph)} onOpen={target => navigateGraph(target, undefined, true)}
                  onChange={call => patch({ ...doc, ops: { ...doc.ops, [selectedNode.op!]: { ...doc.ops![selectedNode.op!], call } } })} /> : <>
                  <div className="inspector-kind">命令</div><p className="inspector-note">退出码 0 完成，非 0 失败。</p>
                  <label>命令<textarea className="instructions" value={doc.ops?.[selectedNode.op]?.run ?? ''}
                    onChange={event => patch({ ...doc, ops: { ...doc.ops, [selectedNode.op!]: { ...doc.ops?.[selectedNode.op!], run: event.target.value } } })} /></label>
                  {(['reads', 'writes'] as const).map(field => <label key={field}>{field === 'reads' ? '读' : '写'}<input
                    value={(doc.ops?.[selectedNode.op!]?.[field] ?? []).join(', ')} onChange={event => patch({ ...doc,
                    ops: { ...doc.ops, [selectedNode.op!]: { ...doc.ops?.[selectedNode.op!], [field]: event.target.value.split(',').map(item => item.trim()).filter(Boolean) } } })} /></label>)}
                </> : selectedNode.graph ? <p className="inspector-note">
                  本次运行内执行。这个节点运行的是文件里声明的 <code>{selectedNode.graph}</code>。展开之后它的节点
                  以 <code>{selectedNode.id}/…</code> 命名，各自在自己的目录里，父图只看得到它的
                  <code>exit</code> 节点。
                </p> : <>
                <Plugins selected={selectedNode.plugins} disabled={!editable}
                  onChange={plugins => patchNode({ plugins })} />
                <label>使用角色
                  <select value={selectedNode.agent}
                          onChange={event => patchNode({ agent: event.target.value })}>
                    {Object.keys(doc.agents ?? {}).map(agent => (
                      <option key={agent} value={agent}>{agent}</option>))}
                  </select>
                </label>
                <label>这一步额外要求
                  <input value={selectedNode.with ?? ''} maxLength={2000}
                         placeholder="附加在这个角色自己的指令之后"
                         onChange={event => patchNode({ with: event.target.value })} />
                </label>
                </>}
                {!selectedNode.op && routing.has(selectedNode.id) && <p className="inspector-note">
                  这个节点有多条出边，所以它必须用 <code>anchor-route</code> 结束，
                  不能用 <code>anchor-done</code>。这一点要写进它的指令里。
                </p>}
                <div className="selection-tools">
                  <ToolButton icon={Copy} label="复制节点" onClick={duplicateNode} />
                </div>
                <button className="full-button"
                        disabled={Boolean(selectedNode.graph) || Boolean(selectedNode.op)}
                        onClick={() => setPick({ kind: 'agent', id: selectedNode.agent ?? '' })}>
                  {selectedNode.graph ? '模块的角色在它自己的图里'
                    : selectedNode.op ? 'Op 没有角色' : '编辑它的角色'}</button>
                <button className="full-button" onClick={() => setJson('node')}>完整节点 JSON</button>
              </>}

              {selectedAgent && pick?.kind === 'agent' && <>
                <div className="inspector-kind">角色 · {pick.id}</div>
                <label>模型
                  <input value={selectedAgent.model} maxLength={128}
                         onChange={event => patchAgent({ model: event.target.value })} />
                </label>
                <label className="check-label">
                  <input type="checkbox" checked={selectedAgent.network}
                         onChange={event => patchAgent({ network: event.target.checked })} />
                  允许联网（检索文献的节点需要）
                </label>
                <label>指令
                  <textarea className="instructions" value={selectedAgent.instructions}
                            onChange={event => patchAgent({ instructions: event.target.value })} />
                </label>
                <button className="full-button" onClick={() => setJson('agent')}>角色 JSON</button>
              </>}

              {selectedEdge && pick?.kind === 'edge' && <>
                <div className="inspector-kind">连线</div>
                <label>起点
                  <select value={selectedEdge.from}
                          onChange={event => patchEdge({ from: event.target.value })}>
                    {doc?.nodes.map(item => <option key={item.id} value={item.id}>{item.id}</option>)}
                  </select>
                </label>
                <label>终点
                  <select value={selectedEdge.to}
                          onChange={event => patchEdge({ to: event.target.value })}>
                    {doc?.nodes.map(item => <option key={item.id} value={item.id}>{item.id}</option>)}
                  </select>
                </label>
                <label>分支说明
                  <input value={doc?.layout?.edgeLabels?.[`${selectedEdge.from}|${selectedEdge.to}`] ?? ''}
                    placeholder="例如：需要补证（仅用于显示）" maxLength={80}
                    onChange={event => doc && patch({ ...doc, layout: { ...doc.layout,
                      edgeLabels: { ...doc.layout?.edgeLabels, [`${selectedEdge.from}|${selectedEdge.to}`]: event.target.value } } })} />
                </label>
                <button className="full-button" onClick={() => setJson('edge')}>连线 JSON</button>
              </>}

              {pick && <button className="full-button danger" onClick={removePicked}>
                <Trash2 size={15} />删除这个{pick.kind === 'node' ? '节点' : pick.kind === 'edge' ? '连线' : '角色'}
              </button>}

              {!pick && doc && <>
                <label>目标
                  <textarea aria-label="目标" value={doc.objective ?? ''} maxLength={2000}
                            onChange={event => patch({ ...doc, objective: event.target.value })} />
                </label>
                <label>入口节点
                  <select value={entry} onChange={event => patch({ ...doc, entry: event.target.value })}>
                    <option value="">（未设置）</option>
                    {doc.nodes.map(item => <option key={item.id} value={item.id}>{item.id}</option>)}
                  </select>
                </label>
                <label>节点轮数上限（可选，留空不限制）
                  <input type="number" min={1} placeholder="不限制" value={doc.max_rounds ?? ''}
                         onChange={event => {
                           const next = { ...doc };
                           if (event.target.value === '') delete next.max_rounds;
                           else next.max_rounds = Number(event.target.value);
                           patch(next);
                         }} />
                </label>
                <button className="full-button" onClick={() => setJson('graph')}>完整 Graph JSON</button>
              </>}

              <section className="connections">
                <h3>连接节点</h3>
                <label>起点
                  <select value={connect.source}
                          onChange={event => setConnect({ ...connect, source: event.target.value })}>
                    <option value="">选择节点</option>
                    {doc?.nodes.map(item => <option key={item.id} value={item.id}>{item.id}</option>)}
                  </select>
                </label>
                <label>终点
                  <select value={connect.target}
                          onChange={event => setConnect({ ...connect, target: event.target.value })}>
                    <option value="">选择节点</option>
                    {doc?.nodes.map(item => <option key={item.id} value={item.id}>{item.id}</option>)}
                  </select>
                </label>
                <button className="full-button" disabled={!doc || !connect.source || !connect.target}
                        onClick={() => {
                          if (!doc) return;
                          if (doc.edges.some(edge => edge.from === connect.source && edge.to === connect.target)) {
                            setNotice({ kind: 'bad', text: '这条连线已经有了。' }); return;
                          }
                          patch({ ...doc, edges: [...doc.edges, { from: connect.source, to: connect.target }] });
                        }}><GitBranch size={16} />添加连线</button>
              </section>

              <section className="agents-list">
                <h3>角色（{Object.keys(doc?.agents ?? {}).length}）</h3>
                {Object.keys(doc?.agents ?? {}).map(agent => (
                  <button key={agent} className="library-row"
                          onClick={() => setPick({ kind: 'agent', id: agent })}>
                    <span className="library-name">{agent}</span>
                  </button>
                ))}
                <button className="full-button" disabled={!editable} onClick={addAgent}>
                  <Plus size={15} />新增角色</button>
              </section>
            </fieldset>
            <div className="inspector-status">
              {pick ? `已选中 ${pick.kind} ${pick.id}` : '未选中 · 显示图本身的字段'}
            </div>
          </aside>
        </Workspace>
      )}

      {relations && <GraphRelations graph={name} close={() => setRelations(false)} onOpen={navigateGraph} />}

      {palette && doc && <Modal title="添加节点" close={() => setPalette(false)}>
        <div className="node-palette">
          <button onClick={() => addOperation(true)}><strong>调用工作流</strong><small>启动独立运行，等待完成或启动后继续</small></button>
          <button onClick={() => addOperation(false)}><strong>命令</strong><small>确定性操作，无需模型</small></button>
          {Object.keys(doc.agents ?? {}).map(agent => (
            <button key={agent} onClick={() => addNode(agent)}>
              <strong>{agent}</strong><small>用这个角色</small>
            </button>
          ))}
          <button onClick={() => { addAgent(); setPalette(false); }}>
            <strong>新建角色</strong><small>再决定它做什么</small>
          </button>
        </div>
      </Modal>}

      {json && doc && <JsonDialog
        title={json === 'graph' ? '完整 Graph JSON' : json === 'node' ? '完整节点 JSON'
          : json === 'edge' ? '连线 JSON' : '角色 JSON'}
        value={json === 'graph' ? doc : json === 'node' ? selectedNode
          : json === 'edge' ? selectedEdge : doc.agents?.[pick?.id ?? '']}
        close={() => setJson(null)}
        apply={value => {
          if (json === 'graph') patch(value as OurGraph);
          else if (json === 'node' && selectedNode) patch({ ...doc,
            nodes: doc.nodes.map(item => item.id === selectedNode.id ? value as typeof item : item) });
          else if (json === 'edge' && selectedEdge) patch({ ...doc,
            edges: doc.edges.map((item, index) => index === Number(pick?.id) ? value as typeof item : item) });
          else if (pick?.kind === 'agent') patch({ ...doc,
            agents: { ...doc.agents, [pick.id]: value as OurAgent } });
        }} />}

      {apiKeyDialog && <Modal title="连接 Anchor 服务" close={() => finishApiKeyPrompt('')}>
        <form className="api-key-form" onSubmit={event => {
          event.preventDefault();
          finishApiKeyPrompt(apiKeyDraft.trim());
        }}>
          <p>此 Anchor 服务需要 API key。密钥只保存在当前浏览器标签页中。</p>
          <label htmlFor="anchor-api-key">Anchor API key</label>
          <input id="anchor-api-key" type="password" autoComplete="current-password" autoFocus
            value={apiKeyDraft} onChange={event => setApiKeyDraft(event.target.value)} />
          <div className="modal-actions">
            <button type="button" onClick={() => finishApiKeyPrompt('')}>取消</button>
            <button className="primary" type="submit" disabled={!apiKeyDraft.trim()}>连接</button>
          </div>
        </form>
      </Modal>}
    </div>
  );
}
