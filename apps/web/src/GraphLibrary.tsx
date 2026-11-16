import { ChevronDown, FolderOpen, GitBranch, Plus, Repeat2, Search } from 'lucide-react';
import { useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { GraphActions } from './GraphActions';
import { EmptyState } from './ui';
import { listenerLabel, type GraphSummary, type OurRun } from './model';
import { groupGraphTasks } from './graphPresentation';
import './GraphCatalog.css';

type Props = {
  graphs: GraphSummary[]; runs: OurRun[]; name: string; busy: boolean;
  selectGraph: (graph: string) => boolean; create: () => Promise<void>;
  deleteGraph: (graph: string, precondition: string) => Promise<void>;
  sidebarControl: ReactNode; children: ReactNode;
};

function TaskGroup({ source, tasks, search, selected, row, matches }: {
  source: GraphSummary; tasks: GraphSummary[]; search: string; selected: string;
  row: (item: GraphSummary) => ReactNode; matches: (item: GraphSummary) => boolean;
}) {
  const selectedChild = tasks.some(item => item.graph === selected);
  const matchingChild = Boolean(search) && tasks.some(matches);
  const [expanded, setExpanded] = useState(selectedChild || matchingChild);
  const contentId = useId();
  useEffect(() => { setExpanded(selectedChild || matchingChild); }, [selected, search, selectedChild, matchingChild]);
  const visibleTasks = search && !matches(source) ? tasks.filter(matches) : tasks;
  return <div className="graph-catalog-group">
    {row(source)}
    {!!tasks.length && <section className="graph-created-tasks">
      <button className="graph-tasks-toggle" aria-expanded={expanded} aria-controls={contentId}
        aria-label={`${source.graph} 创建的任务 ${tasks.length}`} onClick={() => setExpanded(!expanded)}>
        <ChevronDown size={14} aria-hidden="true" /><span>创建的任务</span><span className="count">{tasks.length}</span>
      </button>
      <div id={contentId} hidden={!expanded}>
        <div className="library-list graph-task-list">{visibleTasks.map(row)}</div>
      </div>
    </section>}
  </div>;
}

/** Keep task history reachable without treating finished one-off work as the active catalog. */
export function GraphLibrary({ graphs, runs, name, busy, selectGraph, create, deleteGraph, sidebarControl, children }: Props) {
  const [search, setSearch] = useState('');
  const [expanded, setExpanded] = useState(false);
  const completedId = useId();
  const newGraphButton = useRef<HTMLButtonElement>(null);
  const main = graphs.filter(item => !item.completed_task);
  const completed = graphs.filter(item => item.completed_task);
  const query = search.trim().toLowerCase();
  const matches = (item: GraphSummary) => [item.graph, item.objective ?? '']
    .some(value => value.toLowerCase().includes(query));
  const grouped = groupGraphTasks(main);
  const roots = grouped.roots.filter(item => matches(item) || grouped.children.get(item.graph)?.some(matches));
  const matchingCompleted = completed.filter(matches);
  const showCompleted = expanded;
  const selectedCompleted = completed.some(item => item.graph === name);
  useEffect(() => { if (selectedCompleted) setExpanded(true); }, [name, selectedCompleted]);

  const row = (item: GraphSummary) => {
    const running = Boolean(item.active_runs?.length || item.running);
    const recurring = (item.schedule_summary?.recurring ?? 0) > 0;
    return <div key={item.graph} data-graph={item.graph} className={`library-item ${item.graph === name ? 'chosen' : ''} ${item.listener ? running ? 'listener-working' : item.listener.status === 'authenticated' ? 'listener-listening' : 'listener-unavailable' : ''}`}>
    <button className="library-row" aria-pressed={item.graph === name}
      title={item.listener ? `${item.listener.platform} · ${listenerLabel(item.listener.status)}` : undefined}
      onClick={() => selectGraph(item.graph)}>
      <GitBranch size={16} aria-hidden="true" /><span className="library-label" title={item.graph}>
        <span className="library-name">{item.graph}</span>
        {(running || item.listener || recurring) && <span className="graph-row-badges">
          {running && <span className="pill running">执行中{(item.active_runs?.length ?? 0) > 1 ? ` ${item.active_runs!.length}` : ''}</span>}
          {item.listener && !running && <span className={`pill listener-${item.listener.status}`}>{listenerLabel(item.listener.status)}</span>}
          {recurring && <span className="pill graph-recurring" title={`共 ${item.schedule_summary!.total} 个计划，${item.schedule_summary!.enabled} 个启用，${item.schedule_summary!.recurring} 个周期计划`}>
            <Repeat2 size={10} aria-hidden="true" />周期任务
          </span>}
        </span>}
      </span>
    </button>
    <GraphActions graph={item.graph} running={running} busy={busy}
      runCount={runs.filter(run => run.graph === item.graph).length} onDelete={async (target, precondition) => {
        await deleteGraph(target, precondition); requestAnimationFrame(() => newGraphButton.current?.focus());
      }} />
    </div>;
  };

  return <aside className="library graph-catalog">
    <div className="section-heading"><h3><FolderOpen size={15} />工作流</h3><div className="panel-heading-actions"><span className="count">{main.length}</span>{sidebarControl}</div></div>
    <label className="search-field"><Search size={15} /><input aria-label="搜索工作流" placeholder="搜索工作流…" value={search} onChange={event => { setSearch(event.target.value); setExpanded(Boolean(event.target.value.trim())); }} /></label>
    <button ref={newGraphButton} className="new-graph" onClick={() => void create()} disabled={busy}><Plus size={16} />新建图</button>
    <div className="library-list" aria-label="主工作流列表">{roots.map(item => <TaskGroup key={item.graph}
      source={item} tasks={grouped.children.get(item.graph) ?? []} search={query} selected={name} row={row} matches={matches} />)}</div>
    {!!completed.length && <section className="completed-tasks">
      <button className="completed-tasks-toggle" aria-expanded={showCompleted} aria-controls={completedId} onClick={() => setExpanded(!showCompleted)}>
        <ChevronDown size={14} aria-hidden="true" /><span>已完成的一次性任务</span><span className="count">{completed.length}</span>
      </button>
      <div id={completedId} hidden={!showCompleted}>
        <p className="hint">执行记录和回执保留；再次安排任务后会回到主列表。</p>
        <div className="library-list" aria-label="已完成工作流列表">{matchingCompleted.map(row)}</div>
        {!!query && !matchingCompleted.length && <p className="hint">没有匹配的已完成任务。</p>}
      </div>
    </section>}
    {!graphs.length && <EmptyState icon={GitBranch} title="从一个想法开始">新建工作流，连接你的第一个节点。</EmptyState>}
    {graphs.length > 0 && !graphs.some(matches) && <p className="hint">没有匹配的工作流。</p>}
    <div className="library-footer">{children}</div>
  </aside>;
}
