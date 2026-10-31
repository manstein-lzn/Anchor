import { Activity, CheckCheck, ChevronDown, Copy, Download, FolderOpen, GitBranch, Plus, Redo2, Save, Search, SlidersHorizontal, Trash2, Undo2, Upload } from 'lucide-react';
import { lazy, Suspense, useEffect, useRef, useState, type CSSProperties } from 'react';
import { WorkflowCanvas } from './WorkflowCanvas';
import { GraphActions } from './GraphActions';
import { GraphRunButton } from './GraphRunButton';
import { CallEditor } from './CallEditor';
import { ParallelEditor } from './ParallelEditor';
import { Workspace } from './Workspace';
import { EmptyState, JsonDialog, Modal, ToolButton } from './ui';
import { runPhase, runPhaseLabel } from './runPresentation';
import { parallelControl, renameGraphNode } from './parallel';
import { listenerLabel, type GraphSummary, type OurAgent, type OurGraph, type OurRun } from './model';
import type { GraphEditor, WorkbenchNotice } from './useGraphEditor';

const Plugins = lazy(() => import('./Plugins').then(module => ({ default: module.Plugins })));

const runTime = (value: string) => {
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? '时间未知' : date.toLocaleString('zh-CN', {
    month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit', hour12: false,
  });
};
type Props = {
  name: string; editor: GraphEditor; graphs: GraphSummary[]; runs: OurRun[]; graphColors: Record<string, string>;
  busy: boolean; notice: WorkbenchNotice; setNotice: (notice: WorkbenchNotice) => void;
  selectGraph: (graph: string) => boolean; create: () => Promise<void>; deleteGraph: (graph: string, precondition: string) => Promise<void>;
  save: () => Promise<void>; check: () => Promise<void>; trigger: () => Promise<void>;
  openRun: (run: OurRun) => void; navigateGraph: (graph: string, node?: string, remember?: boolean, sourceNode?: string) => void;
  onPilot: () => void; onRelations: () => void; graphReturn: { graph: string; node?: string } | null; onReturn: () => void;
};
/** Graph-only controls are local; the editor owner survives cross-page navigation. */
export function GraphEditorPage({ name, editor, graphs, runs, graphColors, busy, notice, setNotice,
  selectGraph, create, deleteGraph, save, check, trigger, openRun, navigateGraph,
  onRelations, graphReturn, onReturn, onPilot }: Props) {
  const { doc, dirty, pick, setPick, patch, selectedNode, selectedOp, selectedAgent, selectedEdge,
    selectedPlugins, setPlugins, patchNode, patchAgent, patchEdge, patchOperation, addNode, addOperation,
    addParallel, addAgent, duplicateNode, removePicked, entry, routing } = editor;
  const editable = doc !== null && !busy;
  const [search, setSearch] = useState('');
  const [palette, setPalette] = useState(false);
  const [json, setJson] = useState<'graph' | 'node' | 'edge' | 'agent' | null>(null);
  const [connect, setConnect] = useState({ source: '', target: '' });
  const upload = useRef<HTMLInputElement>(null);
  const newGraphButton = useRef<HTMLButtonElement>(null);
  const runHistory = useRef<HTMLDivElement>(null);
  const currentName = useRef(name); currentName.current = name;
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const graphRuns = runs.filter(item => item.graph === name)
    .sort((a, b) => Date.parse(b.updated || b.started) - Date.parse(a.updated || a.started));
  const activeRun = graphRuns.find(item => item.running) ?? graphRuns[0];
  useEffect(() => {
    setPalette(false); setJson(null); setConnect({ source: '', target: '' });
  }, [name]);
  useEffect(() => {
    const dismiss = () => runHistory.current?.hidePopover();
    window.addEventListener('resize', dismiss);
    return () => window.removeEventListener('resize', dismiss);
  }, []);
  return <>
        <Workspace sidebar selection={pick}>{({ openInspector, sidebarControl, inspectorControls, restoreControls }) => <>
          <aside className="library">
            <div className="section-heading"><h3><FolderOpen size={15} />工作流</h3><div className="panel-heading-actions"><span className="count">{graphs.length}</span>{sidebarControl}</div></div>
            <label className="search-field"><Search size={15} /><input aria-label="搜索工作流" placeholder="搜索工作流…" value={search} onChange={event => setSearch(event.target.value)} /></label>
            <button ref={newGraphButton} className="new-graph" onClick={() => void create()} disabled={busy}>
              <Plus size={16} />新建图
            </button>
            <div className="library-list">
              {graphs.filter(item => item.graph.toLowerCase().includes(search.toLowerCase())).map(item => (
                <div key={item.graph} className={`library-item ${item.graph === name ? 'chosen' : ''} ${item.listener ? item.active_runs?.length || item.running ? 'listener-working' : item.listener.status === 'authenticated' ? 'listener-listening' : 'listener-unavailable' : ''}`}>
                  <button className="library-row" aria-pressed={item.graph === name}
                          title={item.listener ? `${item.listener.platform} · ${listenerLabel(item.listener.status)}` : undefined}
                          onClick={() => selectGraph(item.graph)}>
                    <GitBranch size={16} /><span className="library-name" title={item.graph}>{item.graph}</span>
                    {(item.active_runs?.length || item.running) && <span className="pill running">执行中{(item.active_runs?.length ?? 0) > 1 ? ` ${item.active_runs!.length}` : ''}</span>}
                    {item.listener && !item.active_runs?.length && !item.running && <span className={`pill listener-${item.listener.status}`}>{listenerLabel(item.listener.status)}</span>}
                  </button>
                  <GraphActions graph={item.graph} running={Boolean(item.active_runs?.length || item.running)} busy={busy}
                                runCount={runs.filter(run => run.graph === item.graph).length} onDelete={async (target, precondition) => { await deleteGraph(target, precondition); requestAnimationFrame(() => newGraphButton.current?.focus()); }} />
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
                       const importing = name;
                       void file.text().then(text => {
                         if (!mounted.current || currentName.current !== importing) return;
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
                <h2>{name || '未选择图'}</h2>
                <span className={`document-state ${dirty ? 'unsaved' : ''}`}>{dirty ? '● 未保存' : doc ? '所有更改已保存' : '选择或创建工作流'}</span>
              </div>
              <div className="document-actions">
                {restoreControls}
                <button disabled={!name} onClick={onPilot}>在 Pilot 中协助</button>
                <button onClick={() => onRelations()}>工作流关系</button>
                {graphReturn && <button onClick={onReturn}>返回来源工作流 ←</button>}
                <select aria-label="当前工作流" value={name} onChange={event => selectGraph(event.target.value)}>
                  {graphs.map(item => <option key={item.graph} value={item.graph}>{item.graph}{item.running ? '（执行中）' : ''}</option>)}
                </select>
                <GraphRunButton graph={graphs.find(item => item.graph === name)} name={name} dirty={dirty} busy={busy} onClick={trigger} />
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
                    <span className={`pill ${runPhase(activeRun)}`}>
                      {runPhaseLabel(activeRun.projection)}
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
                      <span className={`run-status-dot ${runPhase(item)}`}
                        style={{ '--graph-color': graphColors[name] } as CSSProperties} />
                      <span className="graph-run-row-main"><strong>{runPhaseLabel(item.projection)}</strong><small>{runTime(item.started)} 开始</small></span>
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
                <ToolButton icon={Undo2} label="撤销" disabled={!editable || !editor.canUndo}
                            onClick={editor.undo} />
                <ToolButton icon={Redo2} label="重做" disabled={!editable || !editor.canRedo}
                            onClick={editor.redo} />
              </div>
            </div>

            <div className="canvas">
              {doc
                ? <WorkflowCanvas graph={doc} name={name} mode="edit" editable={editable} activity={graphs.find(item => item.graph === name)} targets={graphs.map(item => item.graph)} onOpenTarget={(target, sourceNode) => navigateGraph(target, undefined, true, sourceNode)}
                               onPick={value => { setPick(value); if (value) openInspector(); }}
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
            <div className="section-heading"><h3><SlidersHorizontal size={15} />{pick ? '选中项属性' : '工作流设置'}</h3>{inspectorControls}</div>
            {selectedNode && !selectedNode.op && !selectedNode.graph && <Suspense fallback={<p role="status">正在打开插件设置…</p>}><Plugins
              key={`${name}/${selectedNode.id}`} selected={selectedPlugins} disabled={!editable}
              activationMode={editor.pluginActivation?.mode} onChange={setPlugins} graph={{ name, saved: !dirty }} /></Suspense>}
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
                           patch(renameGraphNode(doc, selectedNode.id, next));
                         }} />
                </label>
                {selectedNode.op ? parallelControl(doc, selectedNode.id) ? <ParallelEditor graph={doc} node={selectedNode.id}
                  onChange={join => { if (selectedOp?.fanout) patchOperation({ ...selectedOp, fanout: { join } }); }} /> : doc.ops?.[selectedNode.op]?.call ? <CallEditor
                  key={`${name}/${selectedNode.op}`} call={doc.ops[selectedNode.op].call!} graph={doc} node={selectedNode.id}
                  targets={graphs.map(item => item.graph)} onOpen={target => navigateGraph(target, undefined, true)}
                  onChange={call => { if (selectedOp?.call) patchOperation({ ...selectedOp, call }); }} /> : <>
                  <div className="inspector-kind">命令</div><p className="inspector-note">退出码 0 完成，非 0 失败。</p>
                  <label>命令<textarea className="instructions" value={doc.ops?.[selectedNode.op]?.run ?? ''}
                    onChange={event => { if (selectedOp?.run !== undefined) patchOperation({ ...selectedOp, run: event.target.value }); }} /></label>
                  {(['reads', 'writes'] as const).map(field => <label key={field}>{field === 'reads' ? '读' : '写'}<input
                    value={(doc.ops?.[selectedNode.op!]?.[field] ?? []).join(', ')} onChange={event => { if (selectedOp) patchOperation({ ...selectedOp, [field]: event.target.value.split(',').map(item => item.trim()).filter(Boolean) }); }} /></label>)}
                </> : selectedNode.graph ? <p className="inspector-note">
                  本次运行内执行。这个节点运行的是文件里声明的 <code>{selectedNode.graph}</code>。展开之后它的节点
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
                  <ToolButton icon={Copy} label="复制节点" onClick={duplicateNode} disabled={!!parallelControl(doc, selectedNode.id)} />
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
        </>}</Workspace>
      {palette && doc && <Modal title="添加节点" close={() => setPalette(false)}>
        <div className="node-palette">
          <button onClick={() => { addParallel(); setPalette(false); }}><strong>并行分支</strong><small>在入口前添加展开、两个智能体分支和配对收束</small></button>
          <button onClick={() => { addOperation(true); setPalette(false); }}><strong>调用工作流</strong><small>启动独立运行，等待完成或启动后继续</small></button>
          <button onClick={() => { addOperation(false); setPalette(false); }}><strong>命令</strong><small>确定性操作，无需模型</small></button>
          {Object.keys(doc.agents ?? {}).map(agent => (
            <button key={agent} onClick={() => { addNode(agent); setPalette(false); }}>
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

  </>;
}
