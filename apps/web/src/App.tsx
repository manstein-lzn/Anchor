/** Two views over the same workspace: the graph as something you edit, and the runs as something
 *  you watch.
 *
 * Everything shown comes from the API on a poll, because the API reads the run's directory — so the
 * page cannot disagree with a run, and reloading it costs nothing. The only local state is what is
 * selected and what is being typed.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ExecutionCanvas } from './ExecutionCanvas';
import { label } from './execution';
import {
  toFlowEdges, toFlowNodes,
  type OurGraph, type OurRun, type OurRunDetail,
} from './model';

const POLL_MS = 3000;

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

export function App() {
  const [view, setView] = useState<'graph' | 'runs'>('runs');
  const [graphs, setGraphs] = useState<{ graph: string; running: string | null }[]>([]);
  const [runs, setRuns] = useState<OurRun[]>([]);
  const [graph, setGraph] = useState('');
  const [run, setRun] = useState('');
  const [detail, setDetail] = useState<OurRunDetail | null>(null);
  const [node, setNode] = useState('');
  const [pass, setPass] = useState('');
  const [draft, setDraft] = useState('');                 // the graph being edited, as text
  const [saved, setSaved] = useState('');                 // what is on disk, to know if it differs
  const [notice, setNotice] = useState<{ kind: 'ok' | 'bad'; text: string } | null>(null);
  const [problem, setProblem] = useState('');
  const [busy, setBusy] = useState(false);
  const graphRef = useRef(graph); graphRef.current = graph;
  const runRef = useRef(run); runRef.current = run;

  const refresh = useCallback(async () => {
    try {
      const [graphList, runList] = await Promise.all([
        api<{ graphs: { graph: string; running: string | null }[] }>('/graphs'),
        api<{ runs: OurRun[] }>('/runs'),
      ]);
      setGraphs(graphList.graphs);
      setRuns(runList.runs);
      setProblem('');
      if (!graphRef.current && graphList.graphs.length) setGraph(graphList.graphs[0].graph);
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

  const definition = useMemo<OurGraph | null>(() => {
    try {
      const parsed = JSON.parse(draft || '{}');
      return parsed?.nodes && parsed?.agents ? parsed : null;
    } catch { return null; }
  }, [draft]);

  // Reload the editor when the chosen graph changes, so switching away from unsaved text is a
  // deliberate act rather than something that happens by clicking.
  useEffect(() => {
    if (!graph) return;
    void api<{ definition: OurGraph }>(`/graphs/${encodeURIComponent(graph)}`)
      .then(value => {
        const text = JSON.stringify(value.definition, null, 2);
        setDraft(text); setSaved(text); setNotice(null);
      })
      .catch(error => setProblem(`读不到 ${graph}：${(error as Error).message}`));
  }, [graph]);

  useEffect(() => {
    if (!run) { setDetail(null); return; }
    void api<OurRunDetail>(`/runs/${encodeURIComponent(run)}`).then(setDetail)
      .catch(() => setDetail(null));
  }, [run, runs]);

  const nodes = useMemo(
    () => (definition ? toFlowNodes(definition, graph, view === 'runs' ? detail?.state ?? null : null) : []),
    [definition, graph, detail, view]);
  const edges = useMemo(
    () => (definition ? toFlowEdges(definition, view === 'runs' ? detail?.state ?? null : null) : []),
    [definition, detail, view]);

  const passes = useMemo(() => {
    const names = Object.keys(detail?.traces ?? {})
      .filter(name => name === node || name.startsWith(`${node}-`))
      .filter(name => name === node || /^\d+$/.test(name.slice(node.length + 1)));
    return names.sort((a, b) => (a === node ? 0 : Number(a.slice(node.length + 1)))
                             - (b === node ? 0 : Number(b.slice(node.length + 1))));
  }, [detail, node]);
  const shown = passes.includes(pass) ? pass : passes[0] ?? '';
  const messages = detail?.traces?.[shown] ?? [];
  const dirty = draft !== saved;

  // Both buttons go through the same call: validation is the save that was not written, and the
  // server is the only thing that knows whether a graph runs.
  const send = async (what: '校验' | '保存') => {
    setBusy(true); setNotice(null);
    try {
      const parsed = JSON.parse(draft);
      await api(`/graphs/${encodeURIComponent(graph)}`, 'PUT', { definition: parsed });
      setSaved(draft);
      setNotice({ kind: 'ok', text: `${what}通过：这个图能跑。` });
    } catch (error) {
      setNotice({ kind: 'bad', text: `${what}未通过：${(error as Error).message}` });
    } finally { setBusy(false); }
  };

  const create = async () => {
    const name = window.prompt('新图的名字（会成为工作区目录名）：');
    if (!name) return;
    setBusy(true);
    try {
      await api('/graphs', 'POST', { name });
      await refresh();
      setGraph(name); setView('graph');
      setNotice({ kind: 'ok', text: `已新建 ${name}。` });
    } catch (error) {
      setNotice({ kind: 'bad', text: `新建失败：${(error as Error).message}` });
    } finally { setBusy(false); }
  };

  const trigger = async () => {
    setBusy(true);
    try {
      const body = await api<{ run: string }>('/trigger', 'POST', { graph });
      setRun(body.run); setView('runs'); setProblem('');
    } catch (error) {
      setProblem((error as Error).message);
    } finally { setBusy(false); void refresh(); }
  };

  return (
    <div className="app-shell">
      <header className="topbar">
        <strong className="brand">Anchor</strong>
        <nav className="tabs">
          <button className={view === 'runs' ? 'chosen' : ''} onClick={() => setView('runs')}>运行</button>
          <button className={view === 'graph' ? 'chosen' : ''} onClick={() => setView('graph')}>图</button>
        </nav>
        <select value={graph} onChange={event => { setGraph(event.target.value); setRun(''); }}>
          {graphs.map(item => (
            <option key={item.graph} value={item.graph}>
              {item.graph}{item.running ? '（正在运行）' : ''}
            </option>
          ))}
        </select>
        <button onClick={trigger} disabled={busy || !graph}>触发一次运行</button>
        <button onClick={create} disabled={busy}>新建图</button>
        {problem && <span className="problem">{problem}</span>}
      </header>

      {view === 'graph' ? (
        <div className="app-body editing">
          <section className="library">
            <h3>图</h3>
            {graphs.map(item => (
              <button key={item.graph}
                      className={`library-row ${item.graph === graph ? 'chosen' : ''}`}
                      onClick={() => setGraph(item.graph)}>
                <span className="library-name">{item.graph}</span>
                {item.running && <span className="pill running">执行中</span>}
              </button>
            ))}
          </section>

          <section className="editor">
            <div className="editor-head">
              <span className="eyebrow">{graph || '（未选择）'} · graph.json</span>
              {dirty && <span className="dirty">有未保存的改动</span>}
              <span className="spacer" />
              <button onClick={() => void send('校验')} disabled={busy || !graph}>校验</button>
              <button onClick={() => void send('保存')} disabled={busy || !graph} className="primary">保存</button>
            </div>
            {notice && <p className={`notice ${notice.kind}`}>{notice.text}</p>}
            <textarea className="json-editor" spellCheck={false}
                      value={draft} onChange={event => { setDraft(event.target.value); setNotice(null); }} />
          </section>

          <section className="preview">
            <div className="editor-head"><span className="eyebrow">预览（保存后的样子）</span></div>
            {definition
              ? <ExecutionCanvas instanceKey={`edit-${graph}`} nodes={nodes} edges={edges}
                                 onSelectNode={setNode} />
              : <p className="hint canvas-empty">这段 JSON 还读不出来，先修语法。</p>}
          </section>
        </div>
      ) : (
        <div className="app-body running">
          <aside className="runs">
            <h3>运行历史</h3>
            {runs.filter(item => !graph || item.graph === graph).map(item => (
              <button key={`${item.graph}-${item.run}`}
                      className={`run-row ${item.run === run ? 'chosen' : ''}`}
                      onClick={() => { setRun(item.run); setGraph(item.graph); setNode(''); }}>
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
            <ExecutionCanvas instanceKey={`${graph}-${run}`} nodes={nodes} edges={edges}
                             onSelectNode={id => { setNode(id); setPass(''); }} />
          </main>

          <aside className="trace">
            <h3>{node ? `${node} 的对话` : '节点对话'}</h3>
            {passes.length > 1 && <div className="passes">
              {passes.map((name, index) => (
                <button key={name} className={name === shown ? 'chosen' : ''}
                        onClick={() => setPass(name)}>第 {index + 1} 轮</button>
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
      )}
    </div>
  );
}
