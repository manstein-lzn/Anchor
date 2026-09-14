/** The graph is a document you edit and a run is something you watch.
 *
 * The editing is laid out the way the previous interface laid it out, because that was not the part
 * that needed changing: a library on the left, the canvas in the middle with its toolbars above and
 * below, and an inspector on the right that shows the fields of whatever is selected — a node, an
 * edge, an agent, or the graph itself when nothing is. Adding a node opens a chooser; connecting two
 * is two selects and a button; anything can be opened as JSON in a dialog. None of it needs dragging,
 * and a revision loop is easier to add with two selects than with a gesture.
 *
 * What is gone is what belonged to the previous backend: versions, publishing, the read-only view of
 * a published snapshot, the capability check and the bearer token.
 */

import {
  CheckCheck, Copy, Download, GitBranch, Plus, Redo2, Save, Trash2, Undo2, Upload,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ExecutionCanvas } from './ExecutionCanvas';
import { GraphCanvas } from './GraphCanvas';
import { label } from './execution';
import { toFlowEdges, toFlowNodes, type OurGraph, type OurRun, type OurRunDetail } from './model';

const POLL_MS = 3000;
type Pick = { kind: 'node' | 'edge' | 'agent'; id: string } | null;

async function api<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
  const response = await fetch(path, {
    method,
    headers: body === undefined ? { Accept: 'application/json' }
      : { Accept: 'application/json', 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const data = await response.json().catch(() => null);
  if (!response.ok) throw new Error(data?.error ?? `${method} ${path} → ${response.status}`);
  return data as T;
}

const when = (iso: string) => (iso ? iso.replace('T', ' ').replace('Z', '') : '');

function ToolButton({ icon: Icon, label: text, ...rest }:
  { icon: typeof Plus; label: string } & React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return <button type="button" aria-label={text} title={text} {...rest}><Icon size={16} /></button>;
}

function Modal({ title, close, children }:
  { title: string; close: () => void; children: React.ReactNode }) {
  return (
    <div className="modal-backdrop" onClick={close}>
      <div className="modal" onClick={event => event.stopPropagation()}>
        <header><h2>{title}</h2><button onClick={close}>关闭</button></header>
        {children}
      </div>
    </div>
  );
}

function JsonDialog({ title, value, apply, close }:
  { title: string; value: unknown; apply: (value: unknown) => void; close: () => void }) {
  const [text, setText] = useState(() => JSON.stringify(value, null, 2));
  const [error, setError] = useState('');
  return <Modal title={title} close={close}>
    <textarea className="json-editor" spellCheck={false} value={text}
              onChange={event => { setText(event.target.value); setError(''); }} />
    {error && <p className="notice bad">{error}</p>}
    <div className="modal-actions">
      <button onClick={() => {
        try { apply(JSON.parse(text)); close(); }
        catch (problem) { setError(`JSON 语法错误：${(problem as Error).message}`); }
      }}>应用</button>
    </div>
  </Modal>;
}

export function App() {
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
  const [pass, setPass] = useState('');
  const [notice, setNotice] = useState<{ kind: 'ok' | 'bad'; text: string } | null>(null);
  const [problem, setProblem] = useState('');
  const [busy, setBusy] = useState(false);
  const nameRef = useRef(name); nameRef.current = name;
  const runRef = useRef(run); runRef.current = run;
  const upload = useRef<HTMLInputElement>(null);

  const dirty = doc !== null && JSON.stringify(doc, null, 2) + '\n' !== savedDoc;
  const editable = doc !== null && !busy;

  const refresh = useCallback(async () => {
    try {
      const [graphList, runList] = await Promise.all([
        api<{ graphs: { graph: string; running: string | null }[] }>('/graphs'),
        api<{ runs: OurRun[] }>('/runs'),
      ]);
      setGraphs(graphList.graphs);
      setRuns(runList.runs);
      setProblem('');
      if (!nameRef.current && graphList.graphs.length) setName(graphList.graphs[0].graph);
      const live = runList.runs.find(item => item.running);
      if (live && !runRef.current) { setRun(live.run); setName(live.graph); }
      if (!runRef.current && runList.runs.length) setRun(runList.runs[0].run);
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
    void api<{ definition: OurGraph }>(`/graphs/${encodeURIComponent(name)}`)
      .then(value => {
        setDoc(value.definition);
        setSavedDoc(JSON.stringify(value.definition, null, 2) + '\n');
        setPast([]); setFuture([]); setPick(null); setNotice(null);
      })
      .catch(() => setDoc(null));
  }, [name]);

  useEffect(() => {
    if (!run) { setDetail(null); return; }
    void api<OurRunDetail>(`/runs/${encodeURIComponent(run)}`).then(setDetail)
      .catch(() => setDetail(null));
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

  // The same call as saving, minus the part that remembers it was saved — the server is the only
  // thing that knows whether a graph runs, so asking it is the check.
  const check = () => perform('校验', async () => {
    if (!doc) return;
    await api(`/graphs/${encodeURIComponent(name)}`, 'PUT', { definition: doc });
    setNotice({ kind: 'ok', text: '校验通过：这个图能跑。' });
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

  const selectedNode = pick?.kind === 'node' ? doc?.nodes.find(item => item.id === pick.id) : undefined;
  const selectedAgent = pick?.kind === 'agent' && doc ? doc.agents[pick.id] : undefined;
  const selectedEdge = pick?.kind === 'edge' && doc ? doc.edges[Number(pick.id)] : undefined;

  const patchNode = (fields: Partial<{ id: string; agent: string }>) => {
    if (!doc || !selectedNode) return;
    patch({ ...doc, nodes: doc.nodes.map(item =>
      item.id === selectedNode.id ? { ...item, ...fields } : item) });
  };
  const patchAgent = (fields: Partial<{ model: string; network: boolean; instructions: string }>) => {
    if (!doc || pick?.kind !== 'agent') return;
    patch({ ...doc, agents: { ...doc.agents, [pick.id]: { ...doc.agents[pick.id], ...fields } } });
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
    while (doc.agents[id]) id = `agent${suffix++}`;
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
      nodes: [...doc.nodes, { id, agent: selectedNode.agent }],
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

  const passes = useMemo(() => {
    const names = Object.keys(detail?.traces ?? {})
      .filter(item => item === node || item.startsWith(`${node}-`))
      .filter(item => item === node || /^\d+$/.test(item.slice(node.length + 1)));
    return names.sort((a, b) => (a === node ? 0 : Number(a.slice(node.length + 1)))
                             - (b === node ? 0 : Number(b.slice(node.length + 1))));
  }, [detail, node]);
  const shown = passes.includes(pass) ? pass : passes[0] ?? '';
  const messages = detail?.traces?.[shown] ?? [];
  const entry = doc?.entry ?? '';
  const routing = useMemo(() => {
    if (!doc) return new Set<string>();
    const counts: Record<string, number> = {};
    for (const edge of doc.edges) counts[edge.from] = (counts[edge.from] ?? 0) + 1;
    return new Set(Object.keys(counts).filter(id => counts[id] > 1));
  }, [doc]);

  return (
    <div className="app-shell">
      <header className="topbar">
        <div className="brand">Anchor</div>
        <div className="product-switch">
          <button className={view === 'graph' ? 'chosen' : ''} onClick={() => setView('graph')}>图</button>
          <button className={view === 'runs' ? 'chosen' : ''} onClick={() => setView('runs')}>运行</button>
        </div>
        <select value={name} onChange={event => setName(event.target.value)}>
          {graphs.map(item => (
            <option key={item.graph} value={item.graph}>
              {item.graph}{item.running ? '（执行中）' : ''}
            </option>
          ))}
        </select>
        <button onClick={() => void trigger()} disabled={busy || !name}>触发运行</button>
        <div className="connection"><span className={`status-dot ${problem ? 'bad' : ''}`} />
          {problem || '已连接'}
        </div>
      </header>

      {view === 'runs' ? (
        <div className="workspace running">
          <aside className="runs">
            <div className="section-heading"><h3>运行历史</h3></div>
            {runs.filter(item => !name || item.graph === name).map(item => (
              <button key={`${item.graph}-${item.run}`}
                      className={`run-row ${item.run === run ? 'chosen' : ''}`}
                      onClick={() => { setRun(item.run); setName(item.graph); setNode(''); }}>
                <span className={`dot ${item.status}`} />
                <span className="run-when">{when(item.started)}</span>
                <span className={`pill ${item.status}`}>
                  {item.running ? '执行中' : label(item.status)}
                </span>
                <span className="run-nodes">{item.executed.length} 步</span>
              </button>
            ))}
            {!runs.length && <p className="hint">还没有运行过。</p>}
          </aside>
          <main className="main">
            <div className="canvas-head">
              {detail ? <>
                <span className={`pill ${detail.state.status}`}>
                  {detail.state.status === 'running' ? '执行中' : label(detail.state.status)}
                </span>
                <span className="objective">{detail.state.objective}</span>
                {detail.state.cursor && <span className="hint">
                  正在执行 {detail.state.cursor.node}（第 {detail.state.cursor.pass} 轮）</span>}
                {detail.state.error && <span className="problem">{detail.state.error}</span>}
              </> : <span className="hint">选一次运行，或触发一次。</span>}
            </div>
            <ExecutionCanvas instanceKey={`run-${name}-${run}`} nodes={nodes} edges={edges}
                             onSelectNode={id => { setNode(id); setPass(''); }} />
          </main>
          <aside className="inspector">
            <div className="section-heading"><h3>{node ? `${node} 的对话` : '节点对话'}</h3></div>
            {passes.length > 1 && <div className="passes">
              {passes.map((item, index) => (
                <button key={item} className={item === shown ? 'chosen' : ''}
                        onClick={() => setPass(item)}>第 {index + 1} 轮</button>
              ))}
            </div>}
            {!node && <p className="hint">点一个节点看它说过什么、执行过什么。</p>}
            {node && !messages.length && <p className="hint">这次运行里它还没有留下消息。</p>}
            <ol className="messages">
              {messages.map((message, index) => (
                <li key={index} className={`message ${message.role}`}>
                  <span className="role">
                    {message.role === 'assistant' ? '它' : message.role === 'tool' ? '结果' : message.role}
                    {message.tools?.length ? ` · ${message.tools.join(' ')}` : ''}
                    {message.exit_status ? ` · ${label(message.exit_status)}` : ''}
                  </span>
                  <pre>{message.text}</pre>
                </li>
              ))}
            </ol>
          </aside>
        </div>
      ) : (
        <div className="workspace">
          <aside className="library">
            <div className="section-heading"><h3>图库</h3></div>
            <button className="new-graph" onClick={() => void create()} disabled={busy}>
              <Plus size={16} />新建图
            </button>
            <div className="library-list">
              {graphs.map(item => (
                <button key={item.graph}
                        className={`library-row ${item.graph === name ? 'chosen' : ''}`}
                        onClick={() => setName(item.graph)}>
                  <span className="library-name">{item.graph}</span>
                  {item.running && <span className="pill running">执行中</span>}
                </button>
              ))}
            </div>
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
                <h2>{name || '未选择图'}</h2>
                {dirty && <span className="document-state">有未保存的改动</span>}
              </div>
              <div className="document-actions">
                <button disabled={!editable} onClick={() => void check()}>
                  <CheckCheck size={16} />校验</button>
                <button disabled={!editable || !dirty} onClick={() => void save()}>
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
                               onPick={setPick}
                               onMove={(id, position) => patch({ ...doc,
                                 layout: { ...doc.layout,
                                   positions: { ...doc.layout?.positions, [id]: position } } })}
                               onConnect={(from, to) => {
                                 if (doc.edges.some(edge => edge.from === from && edge.to === to)) {
                                   setNotice({ kind: 'bad', text: '这条连线已经有了。' }); return;
                                 }
                                 patch({ ...doc, edges: [...doc.edges, { from, to }] });
                               }} />
                : <p className="hint canvas-empty">选一个图，或者新建一个。</p>}
            </div>

            <div className="canvas-bottom">
              <span className="canvas-caption">
                graph.json · {doc?.nodes.length ?? 0} 个节点 · {doc?.edges.length ?? 0} 条边 ·{' '}
                {Object.keys(doc?.agents ?? {}).length} 个角色
              </span>
            </div>
          </main>

          <aside className="inspector">
            <div className="section-heading"><h3>属性</h3></div>
            <fieldset disabled={!editable}>
              {selectedNode && doc && <>
                <div className="inspector-kind">节点 · {selectedNode.agent}</div>
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
                <label>使用角色
                  <select value={selectedNode.agent}
                          onChange={event => patchNode({ agent: event.target.value })}>
                    {Object.keys(doc.agents).map(agent => (
                      <option key={agent} value={agent}>{agent}</option>))}
                  </select>
                </label>
                {routing.has(selectedNode.id) && <p className="inspector-note">
                  这个节点有多条出边，所以它必须用 <code>anchor-route</code> 结束，
                  不能用 <code>anchor-done</code>。这一点要写进它的指令里。
                </p>}
                <div className="selection-tools">
                  <ToolButton icon={Copy} label="复制节点" onClick={duplicateNode} />
                </div>
                <button className="full-button" onClick={() => setPick({ kind: 'agent', id: selectedNode.agent })}>
                  编辑它的角色</button>
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
                  <textarea value={doc.objective ?? ''} maxLength={2000}
                            onChange={event => patch({ ...doc, objective: event.target.value })} />
                </label>
                <label>入口节点
                  <select value={entry} onChange={event => patch({ ...doc, entry: event.target.value })}>
                    <option value="">（未设置）</option>
                    {doc.nodes.map(item => <option key={item.id} value={item.id}>{item.id}</option>)}
                  </select>
                </label>
                <label>单个节点最多跑几轮
                  <input type="number" min={1} max={20} value={doc.max_rounds ?? 3}
                         onChange={event => patch({ ...doc, max_rounds: Number(event.target.value) })} />
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
        </div>
      )}

      {palette && doc && <Modal title="添加节点" close={() => setPalette(false)}>
        <div className="node-palette">
          {Object.keys(doc.agents).map(agent => (
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
          : json === 'edge' ? selectedEdge : doc.agents[pick?.id ?? '']}
        close={() => setJson(null)}
        apply={value => {
          if (json === 'graph') patch(value as OurGraph);
          else if (json === 'node' && selectedNode) patch({ ...doc,
            nodes: doc.nodes.map(item => item.id === selectedNode.id ? value as typeof item : item) });
          else if (json === 'edge' && selectedEdge) patch({ ...doc,
            edges: doc.edges.map((item, index) => index === Number(pick?.id) ? value as typeof item : item) });
          else if (pick?.kind === 'agent') patch({ ...doc,
            agents: { ...doc.agents, [pick.id]: value as typeof doc.agents[string] } });
        }} />}
    </div>
  );
}
