import { useEffect, useState } from 'react';
import { api, reason } from './api';
import { callModeLabel, type GraphRelationsData } from './model';
import { Modal } from './ui';

export function GraphRelations({ graph, close, onOpen }: { graph: string; close: () => void;
  onOpen: (graph: string, node?: string) => void }) {
  const [data, setData] = useState<GraphRelationsData | null>(null);
  const [focus, setFocus] = useState(graph);
  const [all, setAll] = useState(false);
  const [search, setSearch] = useState('');
  const [error, setError] = useState('');
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let active = true;
    setError('');
    void api<GraphRelationsData>('/graph-relations').then(value => { if (active) setData(value); })
      .catch(cause => { if (active) setError(reason(cause)); });
    return () => { active = false; };
  }, [attempt]);
  const calls = (data?.calls ?? []).filter(call => (all || call.graph === focus || call.target === focus)
    && `${call.graph} ${call.target} ${call.node}`.toLowerCase().includes(search.toLowerCase()));
  const visible = new Set([focus, ...calls.flatMap(call => [call.graph, call.target])]);
  return <Modal title="工作流关系" close={close} className="relations-dialog">
    <p className="inspector-note">来自已保存工作流的调用定义。默认显示直接调用者与被调用者，定时计划仅标在它直接触发的工作流上。</p>
    <div className="relations-filters">
      <label>中心工作流<select value={focus} onChange={event => setFocus(event.target.value)}>{data?.graphs.map(item => <option key={item.graph}>{item.graph}</option>)}</select></label>
      <label>查找调用<input value={search} placeholder="工作流或来源节点" onChange={event => setSearch(event.target.value)} /></label>
      <label className="check-label"><input type="checkbox" checked={all} onChange={event => setAll(event.target.checked)} />显示全部层级</label>
    </div>
    {error && <p className="problem" role="alert">{error} <button onClick={() => setAttempt(value => value + 1)}>重试</button></p>}
    {!data && !error && <p role="status">正在读取关系…</p>}
    <div className="relation-graphs">{data?.graphs.filter(item => all || visible.has(item.graph)).map(item => <button key={item.graph}
      className={item.graph === focus ? 'chosen' : ''} onClick={() => setFocus(item.graph)}>
      <strong>{item.graph}</strong>{item.schedules > 0 && <span className="pill">定时计划 {item.schedules}</span>}
    </button>)}</div>
    <ul className="relation-calls" aria-label="调用关系列表">{calls.map(call => <li key={`${call.graph}/${call.node}`}>
      <button onClick={() => onOpen(call.graph)}>{call.graph}</button>
      <button className="relation-edge" onClick={() => onOpen(call.graph, call.node)} aria-label={`定位 ${call.graph} 的调用节点 ${call.node}`}>
        <span>{callModeLabel(call.mode)} →</span><small>{call.node}</small>
      </button>
      <button onClick={() => onOpen(call.target)}>{call.target} ↗</button>
    </li>)}</ul>
    {data && !calls.length && <p className="inspector-note">当前范围没有调用关系。</p>}
  </Modal>;
}
