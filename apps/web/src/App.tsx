/** Workbench composition and graph editing. Run inspection and shared dialogs own their local UI
 * state; the model adapter and canvas layout stay independent of the page. */

import {
  Anchor, Activity, CheckCheck, Copy, Download, GitBranch, Plus, Redo2, Save, Trash2, Undo2, Upload,
  Play, Search, SlidersHorizontal, FolderOpen, PanelLeftClose, PanelRightClose,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ExecutionCanvas } from './ExecutionCanvas';
import { GraphCanvas } from './GraphCanvas';
import { label } from './execution';
import {
  toFlowEdges, toFlowNodes, type OurAgent, type OurGraph, type OurRun, type OurRunDetail,
} from './model';
import { RunInspector } from './RunInspector';
import { Workspace } from './Workspace';
import { api } from './api';
import { EmptyState, JsonDialog, Modal, ToolButton } from './ui';

const POLL_MS = 3000;
type Pick = { kind: 'node' | 'edge' | 'agent'; id: string } | null;


const when = (iso: string) => (iso ? iso.replace('T', ' ').replace('Z', '') : '');

export function App() {
  const [search, setSearch] = useState('');
  const [libraryOpen, setLibraryOpen] = useState(true);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [view, setView] = useState<'graph' | 'runs'>('graph');
  const [graphs, setGraphs] = useState<{ graph: string; running: string | null }[]>([]);
  const [runs, setRuns] = useState<OurRun[]>([]);
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
  const [notice, setNotice] = useState<{ kind: 'ok' | 'bad'; text: string } | null>(null);
  const [problem, setProblem] = useState('');
  const [busy, setBusy] = useState(false);
  const nameRef = useRef(name); nameRef.current = name;
  const upload = useRef<HTMLInputElement>(null);

  const dirty = doc !== null && JSON.stringify(doc, null, 2) + '\n' !== savedDoc;
  const editable = doc !== null && !busy;

  useEffect(() => {
    if (!dirty) return;
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ''; };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty]);

  const selectGraph = (next: string) => {
    if (next === name) return;
    if (dirty && !window.confirm('当前工作流有未保存的修改，确定切换并放弃修改吗？')) return;
    setName(next);
    setRun(runs.find(item => item.graph === next)?.run ?? '');
    setNode('');
  };

  const refresh = useCallback(async () => {
    try {
      const [graphList, runList] = await Promise.all([
        api<{ graphs: { graph: string; running: string | null }[] }>('/graphs'),
        api<{ runs: OurRun[] }>('/runs'),
      ]);
      setGraphs(graphList.graphs);
      setRuns(runList.runs);
      setProblem('');
      if (!nameRef.current) {
        const initial = runList.runs.find(item => item.running)?.graph ?? graphList.graphs[0]?.graph;
        if (initial) {
          setName(initial);
          setRun(runList.runs.find(item => item.graph === initial)?.run ?? '');
        }
      }
    } catch (error) {
      setProblem(`服务未连接：${(error as Error).message}`);
    }
  }, []);

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
        setPast([]); setFuture([]); setPick(null); setNotice(null);
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
    await api(`/graphs/${encodeURIComponent(name)}`, 'PUT', { definition: doc });
    setSavedDoc(JSON.stringify(doc, null, 2) + '\n');
    setNotice({ kind: 'ok', text: '已保存。' });
  });

  // The server validates on PUT; this action also saves and must say so.
  const check = () => perform('校验', async () => {
    if (!doc) return;
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

  const trigger = () => perform('触发', async () => {
    const body = await api<{ run: string }>('/trigger', 'POST', { graph: name });
    setRun(body.run); setView('runs');
  });

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

  const selectedNode = pick?.kind === 'node' ? doc?.nodes.find(item => item.id === pick.id) : undefined;
  const selectedAgent = pick?.kind === 'agent' && doc ? doc.agents?.[pick.id] : undefined;
  const selectedEdge = pick?.kind === 'edge' && doc ? doc.edges[Number(pick.id)] : undefined;

  const patchNode = (fields: Partial<{ id: string; agent: string; with: string }>) => {
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

  const nodes = useMemo(
    () => (doc ? toFlowNodes(doc, name, view === 'runs' ? detail?.state ?? null : null) : []),
    [doc, name, detail, view]);
  const edges = useMemo(
    () => (doc ? toFlowEdges(doc, view === 'runs' ? detail?.state ?? null : null) : []),
    [doc, detail, view]);

  const entry = doc?.entry ?? '';
  const routing = useMemo(() => {
    if (!doc) return new Set<string>();
    const counts: Record<string, number> = {};
    for (const edge of doc.edges) counts[edge.from] = (counts[edge.from] ?? 0) + 1;
    return new Set(Object.keys(counts).filter(id => counts[id] > 1));
  }, [doc]);

  return (
    <div className={`app-shell ${libraryOpen ? '' : 'hide-library'} ${inspectorOpen ? '' : 'hide-inspector'}`}>
      <header className="topbar">
        <div className="brand"><span className="brand-mark"><Anchor size={22} /></span>Anchor<span className="brand-caption">WORKSPACE</span></div>
        <nav className="product-switch" aria-label="工作台">
          <button aria-pressed={view === 'graph'} className={view === 'graph' ? 'chosen' : ''} onClick={() => setView('graph')}><GitBranch size={15} />图编排</button>
          <button aria-pressed={view === 'runs'} className={view === 'runs' ? 'chosen' : ''} onClick={() => setView('runs')}><Activity size={15} />运行记录</button>
        </nav>
        <select aria-label="当前工作流" value={name} onChange={event => selectGraph(event.target.value)}>
          {graphs.map(item => (
            <option key={item.graph} value={item.graph}>
              {item.graph}{item.running ? '（执行中）' : ''}
            </option>
          ))}
        </select>
        <div className="connection" title={problem || '服务已连接'}><span className={`status-dot ${problem ? 'bad' : ''}`} />
          {problem ? '连接中断' : '服务在线'}
        </div>
        <button className="primary" title={dirty ? '请先保存工作流' : '运行已保存的工作流'} onClick={() => void trigger()} disabled={busy || !name || dirty}><Play size={14} fill="currentColor" />运行工作流</button>
        <div className="panel-toggles">
          <ToolButton icon={PanelLeftClose} label="切换侧边栏" aria-pressed={libraryOpen} onClick={() => setLibraryOpen(!libraryOpen)} />
          <ToolButton icon={PanelRightClose} label="切换详情面板" aria-pressed={inspectorOpen} onClick={() => setInspectorOpen(!inspectorOpen)} />
        </div>
      </header>
      {problem && <div className="connection-error" role="alert">{problem}</div>}
      {view === 'runs' && notice && <p className={`notice ${notice.kind}`} role="status">{notice.text}</p>}

      {view === 'runs' ? (
        <Workspace running>
          <aside className="runs">
            <div className="section-heading"><h3><Activity size={15} />运行历史</h3><span className="count">{runs.filter(item => item.graph === name).length}</span></div>
            {runs.filter(item => !name || item.graph === name).map(item => (
              <button key={`${item.graph}-${item.run}`}
                      className={`run-row ${item.run === run ? 'chosen' : ''}`}
                      onClick={() => { setRun(item.run); setName(item.graph); setNode(''); }}>
                <span className={`dot ${item.status}`} />
                <span className="run-when">{when(item.started)}</span>
                <span className={`pill ${item.status}`}>
                  {item.running ? '执行中' : label(item.status)}
                </span>
                <span className="run-nodes">{item.objective || '未设置目标'}</span>
                <span className="run-meta">{item.executed.length} 步执行 · {item.run}</span>
              </button>
            ))}
            {!runs.some(item => item.graph === name) && <EmptyState icon={Activity} title="还没有运行记录">运行这个工作流后，可以在这里追踪每一步。</EmptyState>}
          </aside>
          <main className="main">
            <div className="document-header"><div className="document-title"><span className="eyebrow">EXECUTION</span><h2>{name || '运行概览'}</h2></div><span className="hint">每一步，都有迹可循</span></div>
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
                              disabled={busy} title="立即停止当前节点">停止</button>
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
            <ExecutionCanvas instanceKey={`run-${name}-${run}`} nodes={nodes} edges={edges}
                             onSelectNode={id => { setNode(id); setInspectorOpen(true); }} />
            <div className="canvas-bottom"><span className="legend"><i className="dot running" />执行中<i className="dot finished" />已完成<i className="dot failed" />失败</span><span className="canvas-caption">点击节点查看对话与产物</span></div>
          </main>
          <RunInspector key={`${run}/${node}`} run={run} node={node} detail={detail} />
        </Workspace>
      ) : (
        <Workspace>
          <aside className="library">
            <div className="section-heading"><h3><FolderOpen size={15} />工作流</h3><span className="count">{graphs.length}</span></div>
            <label className="search-field"><Search size={15} /><input aria-label="搜索工作流" placeholder="搜索工作流…" value={search} onChange={event => setSearch(event.target.value)} /></label>
            <button className="new-graph" onClick={() => void create()} disabled={busy}>
              <Plus size={16} />新建图
            </button>
            <div className="library-list">
              {graphs.filter(item => item.graph.toLowerCase().includes(search.toLowerCase())).map(item => (
                <button key={item.graph}
                        className={`library-row ${item.graph === name ? 'chosen' : ''}`}
                        onClick={() => selectGraph(item.graph)}>
                  <GitBranch size={16} /><span className="library-name" title={item.graph}>{item.graph}</span>
                  {item.running && <span className="pill running">执行中</span>}
                </button>
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
            <div className="document-header">
              <div className="document-title">
                <span className="eyebrow">WORKFLOW / EDITOR</span>
                <h2>{name || '未选择图'}</h2>
                <span className={`document-state ${dirty ? 'unsaved' : ''}`}>{dirty ? '● 未保存' : doc ? '所有更改已保存' : '选择或创建工作流'}</span>
              </div>
              <div className="document-actions">
                <button disabled={!editable} onClick={() => void check()}>
                  <CheckCheck size={16} />校验并保存</button>
                <button className="primary" disabled={!editable || !dirty} onClick={() => void save()}>
                  <Save size={16} />保存</button>
              </div>
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
                ? <GraphCanvas graph={doc} name={name} editable={editable}
                               onPick={value => { setPick(value); if (value) setInspectorOpen(true); }}
                               onMove={(id, position) => patch({ ...doc,
                                 layout: { ...doc.layout,
                                   positions: { ...doc.layout?.positions, [id]: position } } })}
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
                {selectedNode.op ? <>
                <div className="inspector-kind">Op</div>
                <p className="inspector-note">
                  这个节点跑的是 <code>{selectedNode.op}</code>，<strong>没有模型</strong>：
                  退出码就是判定，0 完成、非 0 失败。它和 agent 节点共用同一套沙箱、工作区、
                  每轮一个 commit 和同一份 <code>reads</code>/<code>writes</code> 契约。
                </p>
                <label>命令
                  <textarea className="instructions" readOnly
                            value={doc.ops?.[selectedNode.op]?.run ?? ''} />
                </label>
                <label>读
                  <input readOnly
                         value={(doc.ops?.[selectedNode.op]?.reads ?? []).join(', ')} />
                </label>
                <label>写
                  <input readOnly
                         value={(doc.ops?.[selectedNode.op]?.writes ?? []).join(', ')} />
                </label>
                <p className="inspector-note">
                  界面还不能编辑 op 的定义——这一版只让图能画出来、跑起来、看得见。
                </p>
                </> : selectedNode.graph ? <p className="inspector-note">
                  这个节点运行的是文件里声明的 <code>{selectedNode.graph}</code>。展开之后它的节点
                  以 <code>{selectedNode.id}/…</code> 命名，各自在自己的目录里，父图只看得到它的
                  <code>exit</code> 节点。
                </p> : <>
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

      {palette && doc && <Modal title="添加节点" close={() => setPalette(false)}>
        <div className="node-palette">
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
    </div>
  );
}
