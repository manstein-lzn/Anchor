import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import {
  Background, BackgroundVariant, getViewportForBounds, Handle, MarkerType, Position, ReactFlow, ReactFlowProvider,
  useReactFlow, type Connection, type NodeProps,
} from '@xyflow/react';
import {
  Anchor, Bot, Wrench, GitBranch, Split, Merge, ShieldCheck, UserCheck,
  Clock, UserRound, FileBox, Repeat2, Boxes, Plus, Save, Upload, Download,
  CheckCheck, Send, History, Undo2, Redo2, Trash2, Copy, X, Settings2,
  ChevronLeft, ChevronRight, LogOut, ZoomIn, ZoomOut,
  Maximize, RefreshCw, KeyRound, Circle, Code2, ArrowRight, LoaderCircle,
  type LucideIcon,
} from 'lucide-react';
import { ApiError, request } from './api';
import { RunConsole } from './RunConsole';
import { GraphExecution } from './GraphExecution';
import {
  kinds, emptyDocument, fingerprint, parseDocument, project, removeSelection,
  downloadDocument, type CanvasNode, type Definition, type Document,
  type Draft, type Kind, type NodeSpec, type Version,
} from './graph';

const icons: Record<Kind, LucideIcon> = {
  agent: Bot, tool: Wrench, router: GitBranch, parallel: Split, join: Merge,
  verifier: ShieldCheck, approval: UserCheck, wait_for_event: Clock,
  human_task: UserRound, artifact: FileBox, loop: Repeat2, subgraph: Boxes,
};
const pageSize = 20;
type Issue = { code: string; message: string; node_id?: string; edge_index?: number };
type Validation = { valid: boolean; issues: Issue[] };
type Capabilities = { configured: boolean; models: { ref: string; model: string; provider: string }[]; agents: { ref: string; model_ref: string }[]; tools: { ref: string; description: string }[]; verifiers: { ref: string; version: string; adapter: string; model_ref?: string | null }[] };

function ToolButton({ icon: Icon, label, onClick, disabled, active }: {
  icon: LucideIcon; label: string; onClick: () => void; disabled?: boolean; active?: boolean;
}) {
  return <button type="button" className={`icon-button ${active ? 'active' : ''}`} title={label}
    aria-label={label} aria-pressed={active} disabled={disabled} onClick={onClick}><Icon size={17} /></button>;
}

function GraphNode({ data, selected }: NodeProps<CanvasNode>) {
  const Icon = icons[data.spec.type];
  return <div className={`graph-node kind-${data.spec.type} ${selected ? 'selected' : ''}`}>
    <Handle type="target" position={Position.Left} />
    <div className="node-kind"><Icon size={16} /><span>{kinds[data.spec.type]}</span>{data.entry && <span className="entry-tag">入口</span>}</div>
    <strong>{data.spec.name || '未命名节点'}</strong>
    <span className="node-reference">{String(data.spec.agent_ref || data.spec.tool_ref || data.spec.verifier_ref || data.spec.subgraph_version_id || data.spec.id)}</span>
    <Handle type="source" position={Position.Right} />
  </div>;
}
const nodeTypes = { anchor: GraphNode };

function Modal({ title, close, children }: { title: string; close: () => void; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const modal = ref.current;
    const items = () => Array.from(modal?.querySelectorAll<HTMLElement>('button:not(:disabled), input, textarea, select, [tabindex="0"]') ?? []);
    items()[0]?.focus();
    function key(event: KeyboardEvent) {
      if (event.key === 'Escape') { event.preventDefault(); close(); }
      if (event.key === 'Tab') {
        const focusable = items();
        const first = focusable[0], last = focusable.at(-1);
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
      }
    }
    modal?.addEventListener('keydown', key);
    return () => { modal?.removeEventListener('keydown', key); previous?.focus(); };
  }, [close]);
  return <div className="modal-backdrop"><div className="modal" role="dialog" aria-modal="true" aria-label={title} ref={ref}>
    <header><h2>{title}</h2><ToolButton icon={X} label="关闭对话框" onClick={close} /></header>{children}
  </div></div>;
}

function JsonDialog({ value, title, apply, close }: { value: unknown; title: string; apply: (value: unknown) => void; close: () => void }) {
  const [text, setText] = useState(JSON.stringify(value, null, 2));
  const [error, setError] = useState('');
  return <Modal title={title} close={close}><form onSubmit={event => {
    event.preventDefault();
    try { apply(JSON.parse(text)); close(); } catch (cause) { setError((cause as Error).message); }
  }}><textarea className="json-editor" aria-label={title} spellCheck={false} value={text} onChange={event => setText(event.target.value)} />
    {error && <p className="inline-error" role="alert">{error}</p>}
    <footer><button type="button" onClick={close}>取消</button><button className="primary" type="submit"><CheckCheck size={16} />应用</button></footer>
  </form></Modal>;
}

export function App() {
  const [token, setToken] = useState(() => sessionStorage.getItem('anchor-token') ?? '');
  const [credential, setCredential] = useState('');
  const [connected, setConnected] = useState(false);
  const [execution, setExecution] = useState(false);
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [doc, setDoc] = useState<Document>(emptyDocument);
  const [saved, setSaved] = useState(() => fingerprint(doc));
  const [revision, setRevision] = useState(0);
  const [selection, setSelection] = useState<string | null>(null);
  const [past, setPast] = useState<Document[]>([]);
  const [future, setFuture] = useState<Document[]>([]);
  const [library, setLibrary] = useState<Draft[]>([]);
  const [offset, setOffset] = useState(0);
  const [versions, setVersions] = useState<Version[]>([]);
  const [versionOffset, setVersionOffset] = useState(0);
  const [viewVersion, setViewVersion] = useState<Version | null>(null);
  const [panel, setPanel] = useState<'canvas' | 'versions' | 'execution'>('canvas');
  const [mobilePanel, setMobilePanel] = useState<'canvas' | 'library' | 'inspector'>('canvas');
  const [productView, setProductView] = useState<'graphs' | 'runs'>('graphs');
  const [validation, setValidation] = useState<Validation | null>(null);
  const [notice, setNotice] = useState('');
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [busy, setBusy] = useState('');
  const busyRef = useRef(false);
  const [palette, setPalette] = useState(false);
  const [jsonTarget, setJsonTarget] = useState<'node' | 'edge' | 'graph' | null>(null);
  const [source, setSource] = useState('');
  const [target, setTarget] = useState('');
  const input = useRef<HTMLInputElement>(null);
  const bundleInput = useRef<HTMLInputElement>(null);
  const dragging = useRef<Document | null>(null);
  const flow = useReactFlow<CanvasNode>();
  const effective = viewVersion ? { ...doc, definition: viewVersion.definition } : doc;
  const dirty = fingerprint(doc) !== saved;
  const editable = !busy && !viewVersion && panel !== 'execution';
  const canvas = project(effective, selection, !editable);
  const selectedNode = selection?.startsWith('node:') ? effective.definition.nodes.find(node => node.id === selection.slice(5)) : undefined;
  const edgeIndex = selection?.startsWith('edge:') ? Number(selection.slice(5)) : -1;
  const selectedEdge = effective.definition.edges[edgeIndex];

  const api = <T,>(path: string, method?: string, body?: unknown) => request<T>(token, path, method, body);
  const perform = async (label: string, action: () => Promise<void>) => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(label); setError(''); setNotice('');
    try { await action(); }
    catch (cause) {
      setError((cause as Error).message);
      if (cause instanceof ApiError && cause.status === 409) setConflict(true);
      if (cause instanceof ApiError && cause.status === 401) {
        setConnected(false); sessionStorage.removeItem('anchor-token');
      }
    } finally { busyRef.current = false; setBusy(''); }
  };
  const listGraphs = async (next = offset) => {
    const result = await api<Draft[]>(`/api/graphs?limit=${pageSize}&offset=${next}`);
    setLibrary(result); setOffset(next);
  };
  const listVersions = async (id: string, next = 0) => {
    const result = await api<Version[]>(`/api/graphs/${encodeURIComponent(id)}/versions?limit=${pageSize}&offset=${next}`);
    setVersions(result); setVersionOffset(next);
  };
  useEffect(() => {
    if (!token) return;
    let active = true;
    request<{ execution_connected: boolean }>(token, '/health/ready').then(async health => {
      const list = await request<Draft[]>(token, `/api/graphs?limit=${pageSize}&offset=0`);
      const caps = await request<Capabilities>(token, '/api/runtime/capabilities');
      if (active) { setConnected(true); setExecution(health.execution_connected); setLibrary(list); setOffset(0); setCapabilities(caps); setError(''); }
    }).catch(cause => { if (active) { setConnected(false); setError(cause.message); } });
    return () => { active = false; };
  }, [token]);
  useEffect(() => {
    if (!dirty) return;
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ''; };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty]);
  useEffect(() => {
    const element = document.querySelector('.canvas');
    if (!element || panel !== 'canvas' || productView !== 'graphs') return;
    let firstFrame = 0;
    let secondFrame = 0;
    let previousWidth = element.getBoundingClientRect().width;
    const fit = () => {
      cancelAnimationFrame(firstFrame);
      cancelAnimationFrame(secondFrame);
      // React Flow measures the resized pane asynchronously; fit after that commit.
      firstFrame = requestAnimationFrame(() => {
        secondFrame = requestAnimationFrame(() => {
          const nodes = flow.getNodes();
          const area = element.getBoundingClientRect();
          if (!nodes.length || area.width <= 0 || area.height <= 0) return;
          const bounds = flow.getNodesBounds(nodes);
          const viewport = getViewportForBounds(bounds, area.width, area.height, 0.15, 1, 0.25);
          void flow.setViewport(viewport);
        });
      });
    };
    const observer = new ResizeObserver(entries => {
      const width = entries[0]?.contentRect.width ?? previousWidth;
      if (Math.abs(width - previousWidth) < 1) return;
      previousWidth = width;
      fit();
    });
    observer.observe(element);
    window.addEventListener('resize', fit);
    return () => {
      cancelAnimationFrame(firstFrame);
      cancelAnimationFrame(secondFrame);
      observer.disconnect();
      window.removeEventListener('resize', fit);
    };
  }, [flow, panel, productView, effective.definition.nodes.length]);

  function edit(next: Document, record = true) {
    if (!editable || fingerprint(next) === fingerprint(doc)) return;
    if (record) setPast(items => [...items, doc]);
    setFuture([]); setDoc(next); setValidation(null); setNotice('');
  }
  function patchDefinition(patch: Partial<Definition>) { edit({ ...doc, definition: { ...doc.definition, ...patch } }); }
  function patchNode(patch: Partial<NodeSpec>) {
    patchDefinition({ nodes: doc.definition.nodes.map(node => node.id === selectedNode?.id ? { ...node, ...patch } : node) });
  }
  function confirmDiscard() { return !dirty || window.confirm('当前草稿尚未保存。放弃本地修改？'); }
  function reset(next: Document, rev = 0) {
    setDoc(next); setRevision(rev); setSaved(fingerprint(next)); setSelection(null); setSource(''); setTarget('');
    setPast([]); setFuture([]); setValidation(null); setViewVersion(null); setConflict(false);
    setPanel('canvas'); setMobilePanel('canvas'); setNotice(''); setError('');
    window.setTimeout(() => void flow.fitView({ padding: 0.25, maxZoom: 1 }), 80);
  }
  const loadDraft = async (id: string) => {
    const result = await api<Draft>(`/api/graphs/${encodeURIComponent(id)}/draft`);
    const next = parseDocument(result);
    reset(next, result.revision);
    await listVersions(id);
    const runs = await api<unknown[]>(`/api/runs?graph_id=${encodeURIComponent(id)}&limit=1`);
    if (runs.length) setPanel('execution');
  };
  const saveDraft = async () => {
    if (revision > 0 && !dirty) return revision;
    if (!/^[A-Za-z][A-Za-z0-9_-]{0,127}$/.test(doc.definition.graph_id)) throw new Error('Graph ID 必须以字母开头，仅含字母、数字、下划线或连字符，最长 128 位');
    const result = await api<Draft>(`/api/graphs/${encodeURIComponent(doc.definition.graph_id)}/draft`, 'PUT', {
      expected_revision: revision, definition: doc.definition, layout: doc.layout,
    });
    setRevision(result.revision); setSaved(fingerprint(doc)); setConflict(false);
    setNotice(`草稿已保存 · r${result.revision}`);
    return result.revision;
  };
  async function publish() {
    const result = await api<Validation>('/api/graphs/validate', 'POST', doc.definition);
    setValidation(result);
    if (!result.valid) return;
    const capabilityResult = await api<Validation>('/api/graphs/capabilities/validate', 'POST', doc.definition);
    if (!capabilityResult.valid) {
      setValidation(capabilityResult);
      setError('能力校验未通过：请配置或修正 Agent / Tool 引用');
      return;
    }
    const rev = await saveDraft();
    const version = await api<Version>(`/api/graphs/${encodeURIComponent(doc.definition.graph_id)}/publish`, 'POST', { expected_revision: rev });
    setNotice(`已发布 v${version.version} · 接收器${execution ? '已连接' : '未连接'}`);
    await listVersions(doc.definition.graph_id); await listGraphs();
  }
  function addNode(kind: Kind) {
    const id = `${kind}-${crypto.randomUUID().slice(0, 8)}`;
    const spec: NodeSpec = { id, type: kind, name: kinds[kind] };
    const area = document.querySelector('.canvas')?.getBoundingClientRect();
    const position = flow.screenToFlowPosition({ x: (area?.left ?? 0) + (area?.width ?? 700) / 2 - 100, y: (area?.top ?? 0) + (area?.height ?? 500) / 2 - 40 });
    const startX = position.x;
    let slot = 0;
    while (canvas.nodes.some(node => Math.abs(node.position.x - position.x) < 250 && Math.abs(node.position.y - position.y) < 130)) {
      slot += 1; position.x = startX + (slot % 3) * 280;
      if (slot % 3 === 0) position.y += 170;
    }
    edit({ ...doc, definition: { ...doc.definition, nodes: [...doc.definition.nodes, spec] },
      layout: { ...doc.layout, positions: { ...doc.layout.positions, [id]: position } } });
    setSelection(`node:${id}`); setPalette(false); setMobilePanel('inspector');
    window.setTimeout(() => void flow.fitView({ padding: 0.25, maxZoom: 1 }), 80);
  }
  function connect(connection: Connection) {
    if (!editable || !connection.source || !connection.target) return;
    if (doc.definition.edges.some(edge => edge.source === connection.source && edge.target === connection.target && !edge.condition)) return;
    patchDefinition({ edges: [...doc.definition.edges, { source: connection.source, target: connection.target }] });
    setSelection(`edge:${doc.definition.edges.length}`);
  }
  function duplicateNode() {
    if (!selectedNode) return;
    const id = `${selectedNode.type}-${crypto.randomUUID().slice(0, 8)}`;
    const position = canvas.nodes.find(node => node.id === selectedNode.id)!.position;
    edit({ ...doc, definition: { ...doc.definition, nodes: [...doc.definition.nodes, { ...structuredClone(selectedNode), id, name: `${selectedNode.name} 副本` }] },
      layout: { ...doc.layout, positions: { ...doc.layout.positions, [id]: { x: position.x + 45, y: position.y + 140 } } } });
    setSelection(`node:${id}`);
  }
  function undo() {
    const previous = past.at(-1); if (!previous) return;
    setFuture(items => [...items, doc]); setPast(past.slice(0, -1)); setDoc(previous); setValidation(null); setSelection(null);
  }
  function redo() {
    const next = future.at(-1); if (!next) return;
    setPast(items => [...items, doc]); setFuture(future.slice(0, -1)); setDoc(next); setValidation(null); setSelection(null);
  }
  const closePalette = useCallback(() => setPalette(false), []);
  const closeJson = useCallback(() => setJsonTarget(null), []);

  return <div className="app-shell">
    <header className="topbar">
      <div className="brand"><Anchor size={25} /><span>Anchor</span><span className="environment">LOCAL</span></div>
      <div className="product-switch" aria-label="产品视图"><button className={productView === 'graphs' ? 'active' : ''} onClick={() => setProductView('graphs')}><GitBranch size={15} />编排</button><button className={productView === 'runs' ? 'active' : ''} onClick={() => setProductView('runs')}><History size={15} />运行</button></div>
      <div className="connection"><span className={`status-dot ${connected ? 'online' : ''}`} /><span>{connected ? 'API 已连接' : '未连接'}</span><span className="execution-state">接收器{execution ? '已连接' : '未连接'}</span></div>
      <ToolButton icon={LogOut} label="断开连接" disabled={!!busy || !connected} onClick={() => { sessionStorage.removeItem('anchor-token'); setToken(''); setCredential(''); setConnected(false); }} />
    </header>
    <nav className={`mobile-nav ${productView === 'runs' ? 'view-hidden' : ''}`} aria-label="工作区面板">
      {(['library', 'canvas', 'inspector'] as const).map((item, index) => <button key={item} className={mobilePanel === item ? 'active' : ''} onClick={() => setMobilePanel(item)}>{['工作流', '画布', '属性'][index]}</button>)}
    </nav>
    <div className={`workspace mobile-${mobilePanel} ${panel === 'execution' ? 'execution-workspace' : ''} ${productView === 'runs' ? 'view-hidden' : ''}`} inert={!connected || palette || !!jsonTarget} aria-hidden={!connected || palette || !!jsonTarget || productView === 'runs'}>
      <aside className="library">
        <div className="section-heading"><h2>工作流</h2><ToolButton icon={RefreshCw} label="刷新工作流" disabled={!!busy} onClick={() => void perform('刷新', () => listGraphs())} /></div>
        <button className="new-graph" disabled={!!busy} onClick={() => { if (confirmDiscard()) { reset(emptyDocument()); setVersions([]); } }}><Plus size={17} />新建工作流</button>
        <div className="library-list">
          {library.map(item => <button key={item.graph_id} className={`library-item ${doc.definition.graph_id === item.graph_id ? 'active' : ''}`} disabled={!!busy} onClick={() => {
            if (confirmDiscard()) void perform('载入草稿', () => loadDraft(item.graph_id));
          }}><GitBranch size={17} /><span><strong>{String(item.definition.name || item.graph_id)}</strong><small>{item.graph_id}</small></span><span className="revision">r{item.revision}</span></button>)}
          {!library.length && <div className="muted empty-library">暂无已保存草稿</div>}
        </div>
        <div className="pagination"><ToolButton icon={ChevronLeft} label="上一页工作流" disabled={!!busy || offset === 0} onClick={() => void perform('翻页', () => listGraphs(offset - pageSize))} /><span>{offset / pageSize + 1}</span><ToolButton icon={ChevronRight} label="下一页工作流" disabled={!!busy || library.length < pageSize} onClick={() => void perform('翻页', () => listGraphs(offset + pageSize))} /></div>
        <div className="library-footer"><Circle size={12} />开发工作区 <span>v0.1</span></div>
      </aside>
      <main className="main">
        <div className="document-header">
          <div className="document-title"><span className="eyebrow">WORKFLOW</span><h1>{effective.definition.name || '未命名工作流'}</h1><div className="document-state">{panel === 'execution' ? '发布版本执行 · 只读' : viewVersion ? `已发布 v${viewVersion.version} · 只读` : `草稿 ${revision ? `r${revision}` : '未保存'}${dirty ? ' · 有修改' : ''}`}{panel !== 'execution' && <span>{effective.definition.nodes.length} 节点 · {effective.definition.edges.length} 连线</span>}</div></div>
          <div className="document-actions">
            <button disabled={!editable} onClick={() => void perform('校验中', async () => setValidation(await api<Validation>('/api/graphs/validate', 'POST', doc.definition)))}><CheckCheck size={16} />校验</button>
            <button disabled={!editable || (revision > 0 && !dirty)} onClick={() => void perform('保存中', async () => { await saveDraft(); await listGraphs(); })}><Save size={16} />保存</button>
            <button className="primary" disabled={!editable || doc.definition.nodes.length === 0 || conflict} onClick={() => void perform('发布中', publish)}><Send size={16} />发布</button>
          </div>
        </div>
        <div className="view-toolbar">
          <div className="tabs"><button className={panel === 'canvas' ? 'active' : ''} onClick={() => setPanel('canvas')}><GitBranch size={15} />编排</button><button className={panel === 'execution' ? 'active' : ''} disabled={!revision || !!busy} onClick={() => setPanel('execution')}><Clock size={15} />执行</button><button className={panel === 'versions' ? 'active' : ''} disabled={!!busy} onClick={() => { setPanel('versions'); if (revision) void perform('载入版本', () => listVersions(doc.definition.graph_id)); }}><History size={15} />版本</button></div>
          <div className="toolbar-tools"><ToolButton icon={Upload} label="导入 Graph JSON" disabled={!editable} onClick={() => input.current?.click()} /><ToolButton icon={Download} label="导出 Graph JSON" onClick={() => downloadDocument(effective)} /><ToolButton icon={Settings2} label="工作流属性" onClick={() => { setSelection(null); setMobilePanel('inspector'); }} /></div>
        </div>
        {(error || notice || busy) && <div className={`message ${error ? 'error' : ''}`} role={error ? 'alert' : 'status'}>{busy && <LoaderCircle className="spin" size={15} />}<span>{error || busy || notice}</span>{!busy && <ToolButton icon={X} label="关闭通知" onClick={() => { setError(''); setNotice(''); }} />}</div>}
        {conflict && <div className="conflict-actions"><button onClick={() => downloadDocument(doc)}><Download size={15} />导出本地草稿</button><button disabled={!!busy} onClick={() => { if (window.confirm('重新载入会替换当前本地修改。已完成导出或比较？')) void perform('重新载入', () => loadDraft(doc.definition.graph_id)); }}><RefreshCw size={15} />重新载入服务器草稿</button></div>}
        {viewVersion && <div className="version-banner"><ShieldCheck size={16} /><span>发布版本 v{viewVersion.version}</span><button onClick={() => void perform('导出 Bundle', async () => { const bundle = await api<unknown>(`/api/graph-versions/${viewVersion.graph_version_id}/bundle`); const url = URL.createObjectURL(new Blob([JSON.stringify(bundle, null, 2)], { type: 'application/json' })); const link = document.createElement('a'); link.href = url; link.download = `${viewVersion.graph_id}-v${viewVersion.version}.bundle.json`; link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); })}>导出 Bundle</button><button onClick={() => { setViewVersion(null); setSelection(null); setValidation(null); }}>返回草稿</button></div>}
        {panel === 'execution' ? <ReactFlowProvider key={doc.definition.graph_id}><GraphExecution token={token} graphId={doc.definition.graph_id} layout={doc.layout} /></ReactFlowProvider> : panel === 'canvas' ? <div className="canvas" data-testid="canvas">
          <ReactFlow<CanvasNode> nodes={canvas.nodes} edges={canvas.edges} nodeTypes={nodeTypes}
            nodesDraggable={editable} nodesConnectable={editable} edgesReconnectable={false}
            deleteKeyCode={null} multiSelectionKeyCode={null} selectionKeyCode={null}
            defaultEdgeOptions={{ markerEnd: { type: MarkerType.ArrowClosed, color: '#829b96' }, style: { strokeWidth: 1.7, stroke: '#829b96' } }}
            onConnect={connect} onNodeClick={(_, node) => { setSelection(`node:${node.id}`); setMobilePanel('inspector'); }}
            onEdgeClick={(_, edge) => { setSelection(edge.id); setMobilePanel('inspector'); }}
            onPaneClick={() => setSelection(null)}
            onNodeDragStart={() => { dragging.current = doc; }}
            onNodeDrag={(_, node) => edit({ ...doc, layout: { ...doc.layout, positions: { ...doc.layout.positions, [node.id]: node.position } } }, false)}
            onNodeDragStop={(_, node) => {
              if (dragging.current && editable) {
                const next = { ...doc, layout: { ...doc.layout, positions: { ...doc.layout.positions, [node.id]: node.position } } };
                const before = dragging.current;
                if (fingerprint(before) !== fingerprint(next)) setPast(items => [...items, before]);
                setDoc(next); dragging.current = null;
              }
            }} fitView fitViewOptions={{ padding: 0.25, maxZoom: 1 }} minZoom={0.15} maxZoom={2}>
            <Background variant={BackgroundVariant.Dots} gap={22} size={1} color="#c8d4d0" />
          </ReactFlow>
          {!canvas.nodes.length && <div className="canvas-empty"><GitBranch size={35} /><h2>空白工作流</h2><button className="primary" disabled={!editable} onClick={() => setPalette(true)}><Plus size={16} />添加节点</button></div>}
          <div className="canvas-top"><button className="add-node" disabled={!editable} onClick={() => setPalette(true)}><Plus size={17} />添加节点</button><div className="tool-group"><ToolButton icon={Undo2} label="撤销" disabled={!editable || !past.length} onClick={undo} /><ToolButton icon={Redo2} label="重做" disabled={!editable || !future.length} onClick={redo} /></div></div>
          <div className="canvas-bottom"><div className="tool-group"><ToolButton icon={ZoomOut} label="缩小" onClick={() => void flow.zoomOut()} /><ToolButton icon={Maximize} label="适应画布" onClick={() => void flow.fitView({ padding: 0.25, maxZoom: 1 })} /><ToolButton icon={ZoomIn} label="放大" onClick={() => void flow.zoomIn()} /></div><span className="canvas-caption">Graph IR · {viewVersion ? `v${viewVersion.version}` : '草稿'}</span></div>
        </div> : <section className="version-list"><div className="section-heading"><h2>发布记录</h2><span className="muted">{versions.length} 个版本 · 本页</span><button disabled={!!busy} onClick={() => bundleInput.current?.click()}><Upload size={15} />导入 Bundle</button></div>
          {!versions.length && <p className="muted">暂无发布版本</p>}
          {versions.map(version => <button className="version-row" key={version.graph_version_id} onClick={() => { setViewVersion(version); setSelection(null); setValidation(null); setPanel('canvas'); window.setTimeout(() => void flow.fitView({ padding: 0.25, maxZoom: 1 }), 80); }}><ShieldCheck size={20} /><span><strong>v{version.version} · {version.definition.name}</strong><small>{new Date(version.published_at).toLocaleString()} · {version.definition.nodes.length} 节点</small><code>{version.content_hash.slice(0, 20)}</code></span><ChevronRight size={17} /></button>)}
          <div className="pagination"><ToolButton icon={ChevronLeft} label="上一页版本" disabled={!!busy || versionOffset === 0} onClick={() => void perform('载入版本', () => listVersions(doc.definition.graph_id, versionOffset - pageSize))} /><span>{versionOffset / pageSize + 1}</span><ToolButton icon={ChevronRight} label="下一页版本" disabled={!!busy || versions.length < pageSize} onClick={() => void perform('载入版本', () => listVersions(doc.definition.graph_id, versionOffset + pageSize))} /></div>
        </section>}
        {validation && <section className={`validation ${validation.valid ? 'valid' : ''}`} aria-label="校验结果"><header><CheckCheck size={16} /><strong>{validation.valid ? '结构校验通过' : `${validation.issues.length} 项待处理`}</strong><ToolButton icon={X} label="关闭校验结果" onClick={() => setValidation(null)} /></header>
          {validation.issues.map((issue, index) => <button key={index} onClick={() => {
            if (issue.node_id) { setSelection(`node:${issue.node_id}`); void flow.fitView({ nodes: [{ id: issue.node_id }], maxZoom: 1, padding: 0.5 }); }
            else if (issue.edge_index !== undefined) setSelection(`edge:${issue.edge_index}`);
            setMobilePanel('inspector');
          }}><span>{issue.code}</span>{issue.message}</button>)}
        </section>}
      </main>
      <aside className={`inspector ${panel === 'execution' ? 'view-hidden' : ''}`}><div className="section-heading"><h2>{selectedNode ? '节点属性' : selectedEdge ? '连线属性' : '工作流属性'}</h2>{selection && <ToolButton icon={X} label="取消选择" onClick={() => setSelection(null)} />}</div>
        <fieldset disabled={!editable}>
          {selectedNode ? <>
            <div className={`inspector-kind kind-${selectedNode.type}`}>{(() => { const Icon = icons[selectedNode.type]; return <Icon size={20} />; })()}<strong>{kinds[selectedNode.type]}</strong></div>
            <label>名称<input value={selectedNode.name} maxLength={200} onChange={event => patchNode({ name: event.target.value })} /></label>
            <label>节点 ID<input readOnly value={selectedNode.id} /></label>
            {(['agent', 'tool', 'verifier'] as Kind[]).includes(selectedNode.type) && <label>{selectedNode.type === 'agent' ? 'Agent 引用' : selectedNode.type === 'tool' ? '工具引用' : '验证器引用'}<input list={selectedNode.type === 'agent' ? 'anchor-agent-refs' : selectedNode.type === 'tool' ? 'anchor-tool-refs' : 'anchor-verifier-refs'} value={String(selectedNode[`${selectedNode.type}_ref`] ?? '')} onChange={event => patchNode({ [`${selectedNode.type}_ref`]: event.target.value })} />{selectedNode.type === 'agent' && <datalist id="anchor-agent-refs">{(capabilities?.agents ?? []).map(item => <option key={item.ref} value={item.ref} />)}</datalist>}{selectedNode.type === 'tool' && <datalist id="anchor-tool-refs">{(capabilities?.tools ?? []).map(item => <option key={item.ref} value={item.ref} />)}</datalist>}{selectedNode.type === 'verifier' && <datalist id="anchor-verifier-refs">{(capabilities?.verifiers ?? []).map(item => <option key={item.ref} value={item.ref} />)}</datalist>}</label>}
            {selectedNode.type === 'loop' && <label>退出条件<textarea value={selectedNode.exit_condition ?? ''} maxLength={1000} onChange={event => patchNode({ exit_condition: event.target.value })} /></label>}
            {selectedNode.type === 'subgraph' && <label>子图版本 ID<input value={String(selectedNode.subgraph_version_id ?? '')} placeholder="已发布 Graph Version UUID" onChange={event => patchNode({ subgraph_version_id: event.target.value || null })} /></label>}
            <label>进展信号<textarea value={selectedNode.progress_signal ?? ''} maxLength={1000} onChange={event => patchNode({ progress_signal: event.target.value || null })} /></label>
            <label className="check-label"><input type="checkbox" checked={!!selectedNode.approval_required} onChange={event => patchNode({ approval_required: event.target.checked })} />需要人工审批</label>
            <label className="check-label"><input type="checkbox" checked={effective.definition.entry_node_id === selectedNode.id} onChange={event => patchDefinition({ entry_node_id: event.target.checked ? selectedNode.id : null })} />设为入口</label>
            <button className="full-button" onClick={() => setJsonTarget('node')}><Code2 size={16} />完整节点 JSON</button>
          </> : selectedEdge ? <>
            <div className="edge-summary"><code>{selectedEdge.source}</code><ArrowRight size={16} /><code>{selectedEdge.target}</code></div>
            <label>条件<textarea value={selectedEdge.condition ?? ''} maxLength={1000} onChange={event => patchDefinition({ edges: doc.definition.edges.map((edge, index) => index === edgeIndex ? { ...edge, condition: event.target.value || null } : edge) })} /></label>
            <button className="full-button" onClick={() => setJsonTarget('edge')}><Code2 size={16} />连线 JSON / 输入映射</button>
          </> : <>
            <label>工作流名称<input value={effective.definition.name} maxLength={200} onChange={event => patchDefinition({ name: event.target.value })} /></label>
            <label>Graph ID<input value={effective.definition.graph_id} readOnly={revision > 0} maxLength={128} onChange={event => patchDefinition({ graph_id: event.target.value })} /></label>
            <label>入口节点<select value={effective.definition.entry_node_id ?? ''} onChange={event => patchDefinition({ entry_node_id: event.target.value || null })}><option value="">自动推断</option>{effective.definition.nodes.map(node => <option key={node.id} value={node.id}>{node.name} · {node.id}</option>)}</select></label>
            <button className="full-button" onClick={() => setJsonTarget('graph')}><Code2 size={16} />完整 Graph JSON</button>
          </>}
          {selection && <div className="selection-tools">{selectedNode && <ToolButton icon={Copy} label="复制节点" onClick={duplicateNode} />}<ToolButton icon={Trash2} label={selectedNode ? '删除节点' : '删除连线'} onClick={() => { edit(removeSelection(doc, selection)); setSelection(null); }} /></div>}
          <section className="connections"><h3>连接节点</h3><label>起点<select aria-label="连接起点" value={source} onChange={event => setSource(event.target.value)}><option value="">选择节点</option>{effective.definition.nodes.map(node => <option key={node.id} value={node.id}>{node.name}</option>)}</select></label><label>终点<select aria-label="连接终点" value={target} onChange={event => setTarget(event.target.value)}><option value="">选择节点</option>{effective.definition.nodes.map(node => <option key={node.id} value={node.id}>{node.name}</option>)}</select></label>
            <button className="full-button" disabled={!editable || !effective.definition.nodes.some(node => node.id === source) || !effective.definition.nodes.some(node => node.id === target)} onClick={() => connect({ source, target, sourceHandle: null, targetHandle: null })}><GitBranch size={16} />添加连线</button>
          </section>
        </fieldset>
        <div className="inspector-status"><ShieldCheck size={16} /><span>{viewVersion ? '已发布 · 不可变快照' : '草稿 · 未参与运行'}</span></div>
      </aside>
    </div>
    {productView === 'runs' && connected && <RunConsole token={token} onUnauthorized={() => { sessionStorage.removeItem('anchor-token'); setToken(''); setConnected(false); }} />}
    <input ref={bundleInput} type="file" accept="application/json,.json" aria-label="导入 Bundle" hidden onChange={event => {
      const file = event.target.files?.[0]; event.target.value = ''; if (!file) return;
      void perform('导入 Bundle', async () => {
        const bundle = JSON.parse(await file.text());
        const result = await api<{ version: Version | null }>(`/api/bundles/import`, 'POST', {
          bundle, expected_revision: revision, publish: true, import_triggers: true,
        });
        if (result.version) await loadDraft(result.version.graph_id);
        setNotice(`已导入 Bundle · v${result.version?.version ?? '?'}`);
      });
    }} />
    <input ref={input} type="file" aria-label="导入 Graph JSON" accept="application/json,.json" hidden onChange={event => {
      const file = event.target.files?.[0]; event.target.value = ''; if (!file) return;
      void perform('导入中', async () => {
        const next = parseDocument(JSON.parse(await file.text()));
        if (!confirmDiscard()) return;
        reset(next); setSaved(''); setVersions([]); setNotice('已导入本地草稿，尚未保存');
      });
    }} />
    {palette && <Modal title="添加节点" close={closePalette}><div className="node-palette">{Object.entries(kinds).map(([kind, name]) => { const Icon = icons[kind as Kind]; return <button key={kind} aria-label={`添加 ${name} 节点`} onClick={() => addNode(kind as Kind)} className={`kind-${kind}`}><Icon size={22} /><strong>{name}</strong><small>{kind}</small></button>; })}</div></Modal>}
    {jsonTarget && <JsonDialog title={jsonTarget === 'node' ? '完整节点 JSON' : jsonTarget === 'edge' ? '连线 JSON' : '完整 Graph JSON'} value={jsonTarget === 'node' ? selectedNode : jsonTarget === 'edge' ? selectedEdge : doc.definition} close={closeJson} apply={value => {
      const next = structuredClone(doc);
      if (jsonTarget === 'node' && selectedNode) {
        if ((value as NodeSpec)?.id !== selectedNode.id) throw new Error('节点 ID 不可在 JSON 编辑器中修改');
        next.definition.nodes = next.definition.nodes.map(node => node.id === selectedNode.id ? value as NodeSpec : node);
      } else if (jsonTarget === 'edge') next.definition.edges[edgeIndex] = value as typeof selectedEdge;
      else {
        if ((value as Definition)?.graph_id !== doc.definition.graph_id) throw new Error('Graph ID 不可在 JSON 编辑器中修改');
        next.definition = value as Definition;
      }
      edit(parseDocument(next));
    }} />}
    {!connected && <div className="auth-backdrop"><form className="auth-panel" onSubmit={event => {
      event.preventDefault();
      void perform('连接中', async () => {
        const candidate = credential.trim();
        const health = await request<{ execution_connected: boolean }>(candidate, '/health/ready');
        sessionStorage.setItem('anchor-token', candidate); setToken(candidate); setConnected(true); setExecution(health.execution_connected); setCredential('');
      });
    }}><Anchor size={32} /><h1>Anchor</h1><h2>连接本地工作区</h2><div className="endpoint"><Circle size={12} />127.0.0.1:8090</div><label>API Token<input type="password" autoComplete="off" spellCheck={false} value={credential} onChange={event => setCredential(event.target.value)} required aria-label="API Token" /></label>{error && <p role="alert" className="inline-error">{error}</p>}<button className="primary" disabled={!!busy || !credential.trim()}><KeyRound size={16} />{busy || '连接'}</button>{dirty && <button type="button" onClick={() => downloadDocument(doc)}><Download size={16} />导出未保存草稿</button>}</form></div>}
  </div>;
}
