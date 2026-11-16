import { useEffect, useState } from 'react';
import { api, reason } from './api';
import { callModeLabel, type GraphRelationsData } from './model';
import { Modal } from './ui';
import './GraphRelations.css';

export function GraphRelations({ graph, close, onOpen }: { graph: string; close: () => void;
  onOpen: (graph: string, node?: string) => void }) {
  const [data, setData] = useState<GraphRelationsData | null>(null);
  const [focus, setFocus] = useState(graph);
  const [all, setAll] = useState(false);
  const [search, setSearch] = useState('');
  const [error, setError] = useState('');
  const [attempt, setAttempt] = useState(0);
  useEffect(() => setFocus(graph), [graph]);
  useEffect(() => {
    let active = true;
    setError('');
    void api<GraphRelationsData>('/graph-relations').then(value => { if (active) setData(value); })
      .catch(cause => { if (active) setError(reason(cause)); });
    return () => { active = false; };
  }, [attempt]);
  const items = new Map(data?.graphs.map(item => [item.graph, item]));
  const matches = (source: string, target: string, detail = '') =>
    (all || source === focus || target === focus) &&
    [source, target, items.get(source)?.objective ?? '', items.get(target)?.objective ?? '', detail].join(' ').toLowerCase().includes(search.trim().toLowerCase());
  const calls = (data?.calls ?? []).filter(call => matches(call.graph, call.target, call.node));
  const created = (data?.graphs ?? []).filter(item => item.task_source && matches(item.task_source, item.graph));
  const subscriptions = (data?.subscriptions ?? []).filter(item => matches(item.graph, item.target, item.schedule));
  const visible = new Set([focus, ...calls.flatMap(call => [call.graph, call.target]),
    ...created.flatMap(item => [item.task_source!, item.graph]),
    ...subscriptions.flatMap(item => [item.graph, item.target])]);
  const reference = (id: string, choose: () => void) => <button className="relation-reference" title={id}
    disabled={!items.has(id)} onClick={choose}>
    <strong>{id}</strong>
  </button>;
  return <Modal title="工作流关系" close={close} className="relations-dialog">
    <p className="inspector-note">创建归属、工作流调用和结果订阅分别展示。订阅只消费结果，不代表拥有或重新执行生产工作流。</p>
    <div className="relations-filters">
      <label>中心工作流<select value={focus} onChange={event => setFocus(event.target.value)}>{data?.graphs.map(item => <option key={item.graph} value={item.graph}>{item.graph}</option>)}</select></label>
      <label>查找关系<input value={search} placeholder="业务描述、ID 或来源节点" onChange={event => setSearch(event.target.value)} /></label>
      <label className="check-label"><input type="checkbox" checked={all} onChange={event => setAll(event.target.checked)} />显示全部关系</label>
    </div>
    {error && <p className="problem" role="alert">{error} <button onClick={() => setAttempt(value => value + 1)}>重试</button></p>}
    {!data && !error && <p role="status">正在读取关系…</p>}
    <div className="relation-graphs" aria-label="相关工作流">{data?.graphs.filter(item => visible.has(item.graph)).map(item => <button key={item.graph}
      title={item.graph} className={item.graph === focus ? 'chosen' : ''} onClick={() => setFocus(item.graph)}>
      <strong>{item.graph}</strong>{item.schedules > 0 && <span className="pill">定时计划 {item.schedules}</span>}
    </button>)}</div>
    {created.length > 0 && <section className="relationship-section" aria-label="创建归属">
      <h3>创建的任务 <span className="count">{created.length}</span></h3>
      <p className="inspector-note">按已有创建来源整理，不改变任务的独立运行和权限边界。</p>
      <ul className="relation-calls">{created.map(item => <li key={item.graph}>
        {reference(item.task_source!, () => onOpen(item.task_source!))}
        <span className="relation-edge"><span>创建 →</span><small>任务来源</small></span>
        {reference(item.graph, () => onOpen(item.graph))}
      </li>)}</ul>
    </section>}
    {calls.length > 0 && <section className="relationship-section" aria-label="工作流调用">
      <h3>工作流调用 <span className="count">{calls.length}</span></h3>
      <p className="inspector-note">来自已保存的调用节点；同一目标可被多个工作流复用。</p>
      <ul className="relation-calls" aria-label="调用关系列表">{calls.map(call => <li key={`${call.graph}/${call.node}`}>
        {reference(call.graph, () => onOpen(call.graph))}
        <button className="relation-edge" onClick={() => onOpen(call.graph, call.node)} aria-label={`定位 ${call.graph} 的调用节点 ${call.node}`}>
          <span>{callModeLabel(call.mode)} →</span><small>{call.node}</small>
        </button>
        {reference(call.target, () => onOpen(call.target))}
      </li>)}</ul>
    </section>}
    {subscriptions.length > 0 && <section className="relationship-section" aria-label="结果订阅">
      <h3>结果订阅 <span className="count">{subscriptions.length}</span></h3>
      <p className="inspector-note">左侧消费右侧计划的结果；不产生父子所有权。</p>
      <ul className="relation-calls">{subscriptions.map((item, index) => <li key={`${item.graph}/${item.schedule}/${index}`}>
        {reference(item.graph, () => onOpen(item.graph))}
        <span className="relation-edge" title={`计划 ${item.schedule}`}><span>订阅结果 →</span>
          <small className={!item.active || !item.authorized ? 'relation-inactive' : ''}>{!item.active ? '已取消' : !item.authorized ? '授权失效' : '订阅中'}{item.not_before ? ` · ${item.not_before} 后` : ''}</small>
        </span>
        {reference(item.target, () => onOpen(item.target))}
      </li>)}</ul>
    </section>}
    {data && !calls.length && !created.length && !subscriptions.length && <p className="inspector-note">当前范围没有匹配关系，可切换中心工作流或显示全部关系。</p>}
  </Modal>;
}
