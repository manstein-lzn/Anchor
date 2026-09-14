/** One screen: the graphs, their runs, the graph a run walked, and what each node said.
 *
 * There is no state of its own beyond what is selected. Everything shown comes from the API on a
 * poll, because the API reads the run's directory — so the page cannot disagree with the run, and
 * reloading it costs nothing.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ExecutionCanvas } from './ExecutionCanvas';
import { label } from './execution';
import {
  toFlowEdges, toFlowNodes,
  type OurGraph, type OurRun, type OurRunDetail,
} from './model';

const POLL_MS = 3000;

async function get<T>(path: string): Promise<T> {
  const response = await fetch(path, { headers: { Accept: 'application/json' } });
  if (!response.ok) throw new Error(`${path} → ${response.status}`);
  return response.json() as Promise<T>;
}

const when = (iso: string) => (iso ? iso.replace('T', ' ').replace('Z', '') : '');

export function App() {
  const [graphs, setGraphs] = useState<{ graph: string; running: string | null }[]>([]);
  const [runs, setRuns] = useState<OurRun[]>([]);
  const [graph, setGraph] = useState('');
  const [run, setRun] = useState('');
  const [definition, setDefinition] = useState<OurGraph | null>(null);
  const [detail, setDetail] = useState<OurRunDetail | null>(null);
  const [node, setNode] = useState('');
  const [pass, setPass] = useState('');           // '' means the node's first pass
  const [problem, setProblem] = useState('');
  const [busy, setBusy] = useState(false);
  const graphRef = useRef(graph); graphRef.current = graph;
  const runRef = useRef(run); runRef.current = run;

  const refresh = useCallback(async () => {
    try {
      const [graphList, runList] = await Promise.all([
        get<{ graphs: { graph: string; running: string | null }[] }>('/graphs'),
        get<{ runs: OurRun[] }>('/runs'),
      ]);
      setGraphs(graphList.graphs);
      setRuns(runList.runs);
      setProblem('');
      if (!graphRef.current && graphList.graphs.length) setGraph(graphList.graphs[0].graph);
      // Follow whichever run is going, unless someone has picked one to look at.
      const live = runList.runs.find(item => item.running);
      if (live && !runRef.current) { setRun(live.run); setGraph(live.graph); }
      if (!runRef.current && runList.runs.length) {
        const newest = runList.runs[0];
        setRun(newest.run); setGraph(newest.graph);
      }
    } catch (error) {
      setProblem(`连不上服务：${(error as Error).message}`);
    }
  }, []);

  useEffect(() => { void refresh(); }, [refresh]);
  useEffect(() => {
    const timer = window.setInterval(() => { void refresh(); }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [refresh]);

  useEffect(() => {
    if (!graph) { setDefinition(null); return; }
    void get<{ definition: OurGraph }>(`/graphs/${encodeURIComponent(graph)}`)
      .then(value => setDefinition(value.definition))
      .catch(() => setDefinition(null));
  }, [graph]);

  useEffect(() => {
    if (!run) { setDetail(null); return; }
    void get<OurRunDetail>(`/runs/${encodeURIComponent(run)}`)
      .then(setDetail)
      .catch(() => setDetail(null));
  }, [run, runs]);

  const nodes = useMemo(
    () => (definition ? toFlowNodes(definition, graph, detail?.state ?? null) : []),
    [definition, graph, detail]);
  const edges = useMemo(
    () => (definition ? toFlowEdges(definition, detail?.state ?? null) : []),
    [definition, detail]);

  const trigger = async () => {
    setBusy(true);
    try {
      const response = await fetch('/trigger', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ graph }),
      });
      const body = await response.json();
      if (!response.ok) setProblem(body.error ?? `触发失败 (${response.status})`);
      else { setRun(body.run); setProblem(''); }
    } finally { setBusy(false); void refresh(); }
  };

  // A node that was run twice leaves two conversations — `draft` and `draft-2` — because each pass
  // gets its own directory. Showing only the first would hide exactly the round a revision loop is
  // about, so every pass the node has is offered.
  const passes = useMemo(() => {
    const names = Object.keys(detail?.traces ?? {})
      .filter(name => name === node || name.startsWith(`${node}-`))
      .filter(name => name === node || /^\d+$/.test(name.slice(node.length + 1)));
    return names.sort((a, b) => (a === node ? 0 : Number(a.slice(node.length + 1)))
                             - (b === node ? 0 : Number(b.slice(node.length + 1))));
  }, [detail, node]);
  const shown = passes.includes(pass) ? pass : passes[0] ?? '';
  const messages = detail?.traces?.[shown] ?? [];

  return (
    <div className="shell">
      <header className="shell-top">
        <strong className="brand">Anchor</strong>
        <select value={graph} onChange={event => { setGraph(event.target.value); setRun(''); }}>
          {graphs.map(item => (
            <option key={item.graph} value={item.graph}>
              {item.graph}{item.running ? '（正在运行）' : ''}
            </option>
          ))}
        </select>
        <button onClick={trigger} disabled={busy || !graph}>触发一次运行</button>
        {problem && <span className="problem">{problem}</span>}
      </header>

      <div className="shell-body">
        <aside className="runs">
          <h3>运行历史</h3>
          {runs.filter(item => !graph || item.graph === graph).map(item => (
            <button key={`${item.graph}-${item.run}`}
                    className={`run-row ${item.run === run ? 'chosen' : ''}`}
                    onClick={() => { setRun(item.run); setGraph(item.graph); setNode(''); }}>
              <span className={`dot ${item.status}`} />
              <span className="run-when">{when(item.started)}</span>
              <span className={`pill ${item.status}`}>{item.running ? '执行中' : label(item.status)}</span>
              <span className="run-nodes">{item.executed.length} 步</span>
            </button>
          ))}
          {!runs.length && <p className="hint">还没有运行过。</p>}
        </aside>

        <main className="canvas">
          <div className="canvas-head">
            {detail
              ? <>
                  <span className={`pill ${detail.state.status}`}>
                    {detail.state.status === 'running' ? '执行中' : label(detail.state.status)}
                  </span>
                  <span className="objective">{detail.state.objective}</span>
                  {detail.state.cursor && <span className="hint">
                    正在执行 {detail.state.cursor.node}（第 {detail.state.cursor.pass} 轮）
                  </span>}
                  {detail.state.error && <span className="problem">{detail.state.error}</span>}
                </>
              : <span className="hint">选一次运行，或触发一次。</span>}
          </div>
          <ExecutionCanvas
            instanceKey={`${graph}-${run}`}
            nodes={nodes}
            edges={edges}
            onSelectNode={id => { setNode(id); setPass(''); }}
          />
          {!nodes.length && <p className="hint canvas-empty">这个图还没有可显示的内容。</p>}
        </main>

        <aside className="trace">
          <h3>{node ? `${node} 的对话` : '节点对话'}</h3>
          {passes.length > 1 && <div className="passes">
            {passes.map((name, index) => (
              <button key={name} className={name === shown ? 'chosen' : ''}
                      onClick={() => setPass(name)}>
                第 {index + 1} 轮
              </button>
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
    </div>
  );
}
