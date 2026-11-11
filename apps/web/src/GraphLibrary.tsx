import { ChevronDown, FolderOpen, GitBranch, Plus, Search } from 'lucide-react';
import { useId, useRef, useState, type ReactNode } from 'react';
import { GraphActions } from './GraphActions';
import { EmptyState } from './ui';
import { listenerLabel, type GraphSummary, type OurRun } from './model';

type Props = {
  graphs: GraphSummary[]; runs: OurRun[]; name: string; busy: boolean;
  selectGraph: (graph: string) => boolean; create: () => Promise<void>;
  deleteGraph: (graph: string, precondition: string) => Promise<void>;
  sidebarControl: ReactNode; children: ReactNode;
};

/** Keep task history reachable without treating finished one-off work as the active catalog. */
export function GraphLibrary({ graphs, runs, name, busy, selectGraph, create, deleteGraph, sidebarControl, children }: Props) {
  const [search, setSearch] = useState('');
  const [expanded, setExpanded] = useState(false);
  const completedId = useId();
  const newGraphButton = useRef<HTMLButtonElement>(null);
  const main = graphs.filter(item => !item.completed_task);
  const completed = graphs.filter(item => item.completed_task);
  const matches = (item: GraphSummary) => item.graph.toLowerCase().includes(search.trim().toLowerCase());
  const matchingCompleted = completed.filter(matches);
  const showCompleted = expanded;

  const row = (item: GraphSummary) => <div key={item.graph} className={`library-item ${item.graph === name ? 'chosen' : ''} ${item.listener ? item.active_runs?.length || item.running ? 'listener-working' : item.listener.status === 'authenticated' ? 'listener-listening' : 'listener-unavailable' : ''}`}>
    <button className="library-row" aria-pressed={item.graph === name}
      title={item.listener ? `${item.listener.platform} · ${listenerLabel(item.listener.status)}` : undefined}
      onClick={() => selectGraph(item.graph)}>
      <GitBranch size={16} /><span className="library-name" title={item.graph}>{item.graph}</span>
      {(item.active_runs?.length || item.running) && <span className="pill running">执行中{(item.active_runs?.length ?? 0) > 1 ? ` ${item.active_runs!.length}` : ''}</span>}
      {item.listener && !item.active_runs?.length && !item.running && <span className={`pill listener-${item.listener.status}`}>{listenerLabel(item.listener.status)}</span>}
    </button>
    <GraphActions graph={item.graph} running={Boolean(item.active_runs?.length || item.running)} busy={busy}
      runCount={runs.filter(run => run.graph === item.graph).length} onDelete={async (target, precondition) => {
        await deleteGraph(target, precondition); requestAnimationFrame(() => newGraphButton.current?.focus());
      }} />
  </div>;

  return <aside className="library">
    <div className="section-heading"><h3><FolderOpen size={15} />工作流</h3><div className="panel-heading-actions"><span className="count">{main.length}</span>{sidebarControl}</div></div>
    <label className="search-field"><Search size={15} /><input aria-label="搜索工作流" placeholder="搜索工作流…" value={search} onChange={event => { setSearch(event.target.value); setExpanded(Boolean(event.target.value.trim())); }} /></label>
    <button ref={newGraphButton} className="new-graph" onClick={() => void create()} disabled={busy}><Plus size={16} />新建图</button>
    <div className="library-list" aria-label="主工作流列表">{main.filter(matches).map(row)}</div>
    {!!completed.length && <section className="completed-tasks">
      <button className="completed-tasks-toggle" aria-expanded={showCompleted} aria-controls={completedId} onClick={() => setExpanded(!showCompleted)}>
        <ChevronDown size={14} aria-hidden="true" /><span>已完成的一次性任务</span><span className="count">{completed.length}</span>
      </button>
      <div id={completedId} hidden={!showCompleted}>
        <p className="hint">执行记录和回执保留；再次安排任务后会回到主列表。</p>
        <div className="library-list">{matchingCompleted.map(row)}</div>
      </div>
    </section>}
    {!graphs.length && <EmptyState icon={GitBranch} title="从一个想法开始">新建工作流，连接你的第一个节点。</EmptyState>}
    {graphs.length > 0 && !graphs.some(matches) && <p className="hint">没有匹配的工作流。</p>}
    <div className="library-footer">{children}</div>
  </aside>;
}
