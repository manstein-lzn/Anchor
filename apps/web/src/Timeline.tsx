import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type CSSProperties } from 'react';
import { Activity, ArrowUpRight, CalendarDays, ChevronLeft, ChevronRight, Clock3, Plus, Trash2 } from 'lucide-react';
import type { OurRun, OurRunDetail, Schedule, TimelineData, TimelineItem } from './model';
import { api } from './api';
import { Modal } from './ui';
import { assignGraphColors } from './graphColors';

const day = (value: Date) => `${value.getFullYear()}-${String(value.getMonth() + 1).padStart(2, '0')}-${String(value.getDate()).padStart(2, '0')}`;
const addDays = (date: Date, amount: number) => { const result = new Date(date); result.setDate(result.getDate() + amount); return result; };
const clockText = (value: string | number) => new Date(value).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
const dateText = (value: string | number) => new Date(value).toLocaleString('zh-CN', { month: 'long', day: 'numeric', hour: '2-digit', minute: '2-digit' });
const statusText = (status: string) => ({
  running: '执行中', finished: '已完成', completed: '已完成', failed: '失败', aborted: '已终止', stopped: '已停止',
  interrupted: '已中断', waiting_resume: '等待接续', paused: '已暂停', uncertain: '待核查', planned: '已计划',
  missed_busy: '忙碌错过', missed_downtime: '停机错过',
}[status] ?? status);
const sourceText = (value: string) => ({ manual: '手动', schedule: '定时', webhook: 'Webhook', graph_call: '工作流调用', channel: '通道' }[value] ?? value);
const duration = (ms: number) => {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  return minutes < 60 ? `${minutes} 分钟` : `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分`;
};
const hasRunEnd = (run: OurRun) => Boolean(run.updated?.trim()) && Number.isFinite(Date.parse(run.updated));
const runStatus = (run: OurRun) => run.running ? 'running' : run.status === 'running' ? 'waiting_resume' : run.status;
const needsAttention = (status: string) => ['failed', 'aborted', 'interrupted', 'waiting_resume', 'uncertain'].includes(status) || status.startsWith('missed_');
const ruleText = (rule: Record<string, unknown>) => {
  if (rule.type === 'once') return `一次性 · ${dateText(String(rule.at))}`;
  if (rule.type === 'interval') return `每 ${rule.seconds} 秒`;
  if (rule.type === 'weekly') return `每周 ${(rule.weekdays as number[]).map(index => '一二三四五六日'[index]).join('、')} · ${rule.time}`;
  if (rule.type === 'monthly') return `每月 ${rule.day} 日 · ${rule.time}`;
  return `每天 · ${rule.time}`;
};
type Entry = { id: string; graph: string; start: number; end: number; status: string; source: string;
  /** The Run has not finished this interval, so its end grows with the clock rather than a record. */
  open?: boolean;
  /** The Run reports its own execution windows, so this bar is one of them (or its start marker). */
  resident?: boolean; run?: OurRun; plan?: TimelineItem };
type PlacedEntry = Entry & { lane: number };
type Filter = 'all' | 'running' | 'finished' | 'attention' | 'planned' | 'stopped';

/** One Run's bars. A resident instance reports the intervals it actually worked; drawing its whole
 *  lifetime instead would paint every idle wait as execution and join the days into one line. An
 *  instance that reports no window yet is only marked where it started, never stretched to now. */
export function timelineEntries(run: OurRun, now: number): Entry[] {
  const base = { graph: run.graph, status: runStatus(run), source: run.trigger?.source ?? 'manual', run };
  const started = +new Date(run.started);
  if (run.activity) {
    const windows = run.activity
      .map(window => ({ start: +new Date(window.start), end: +new Date(window.end), open: Boolean(window.running) }))
      .filter(window => Number.isFinite(window.start) && Number.isFinite(window.end));
    if (!windows.length) return [{ ...base, resident: true, id: run.run, start: started, end: started }];
    return windows.map(window => ({ ...base, resident: true, id: `${run.run}#${window.start}`, start: window.start, open: window.open,
      end: window.open ? Math.max(now, window.start) : Math.max(window.end, window.start) }));
  }
  return [{ ...base, id: run.run, start: started, open: run.running,
    end: run.running ? now : hasRunEnd(run) ? +new Date(run.updated) : started }];
}

/** What one bar can honestly say about its own interval. A segmented resident bar reports the
 *  segment, so a closed window of a still-running instance must not claim it is executing now. */
export function intervalFacts(run: OurRun, entry: Entry, now: number): { label: string; value: string; end?: string } {
  if (run.running && entry.open) return { label: '已运行', value: duration(now - entry.start), end: '仍在执行' };
  if (entry.resident) return entry.end > entry.start
    ? { label: '这段时长', value: duration(entry.end - entry.start), end: dateText(entry.end) }
    : run.running
      ? { label: '这段时长', value: '暂无记录', end: '等待下一轮输入' }
      : { label: '这段时长', value: '暂无记录', end: '暂无记录' };
  if (run.status === 'running') return { label: '运行状态', value: '宿主重启后等待接续' };
  return entry.end > entry.start
    ? { label: '运行时长', value: duration(entry.end - entry.start), end: dateText(entry.end) }
    : { label: '运行时长', value: '暂无记录', end: '暂无记录' };
}

const placeEntries = (items: Entry[], date: Date): PlacedEntry[] => {
  const dayStart = +date;
  const dayEnd = +addDays(date, 1);
  const laneEnds: number[] = [];
  return [...items].sort((a, b) => a.start - b.start).map(entry => {
    const start = Math.max(entry.start, dayStart);
    const end = Math.min(Math.max(entry.end, entry.start), dayEnd);
    let lane = laneEnds.findIndex(lastEnd => lastEnd <= start);
    if (lane < 0) { lane = laneEnds.length; laneEnds.push(end); }
    else laneEnds[lane] = end;
    return { ...entry, start, end, lane };
  });
};

export function Timeline({ data, graphs, graphColors: sharedGraphColors, page, onPage, onSelect, onOpenRun, onRefresh, problem = '' }: {
  data: TimelineData | null; graphs: string[]; graphColors?: Record<string, string>; page: number; onPage: (page: number) => void;
  onSelect: (run: OurRun) => void; onOpenRun?: (run: string, graph: string) => void; onRefresh: () => void; problem?: string;
}) {
  const [graphFilter, setGraphFilter] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const [sourceFilter, setSourceFilter] = useState('');
  const [chainFilter, setChainFilter] = useState('');
  const [expanded, setExpanded] = useState(false);
  const [selected, setSelected] = useState<Entry | null>(null);
  const [hint, setHint] = useState<{ entry: Entry; anchor: HTMLButtonElement } | null>(null);
  const hintId = useId();
  const tooltip = useRef<HTMLDivElement>(null);
  const [scheduleOpen, setScheduleOpen] = useState(false);
  const [graph, setGraph] = useState(graphs[0] ?? '');
  const [rule, setRule] = useState('once');
  const [at, setAt] = useState('');
  const [time, setTime] = useState('09:00');
  const [interval, setInterval] = useState(3600);
  const [scheduleInput, setScheduleInput] = useState('');
  const [weekdays, setWeekdays] = useState([0, 1, 2, 3, 4]);
  const [monthDay, setMonthDay] = useState(1);
  const [error, setError] = useState('');
  const [pending, setPending] = useState(false);
  const [now, setNow] = useState(Date.now());
  const scheduling = data?.capabilities?.scheduling !== false;
  const graphColors = useMemo(() => sharedGraphColors ?? assignGraphColors([
    ...graphs,
    ...(data?.runs ?? []).map(run => run.graph),
    ...(data?.scheduled ?? []).map(plan => plan.graph),
    ...(data?.schedules ?? []).map(schedule => schedule.graph),
  ]), [sharedGraphColors, graphs, data?.runs, data?.scheduled, data?.schedules]);
  const surface = useRef<HTMLDivElement>(null);
  const positioned = useRef('');
  const requestedFocus = useRef<'today' | 'latest'>('today');
  useLayoutEffect(() => {
    const tip = tooltip.current;
    if (!hint || !tip) return;
    tip.showPopover();
    const anchor = hint.anchor.getBoundingClientRect();
    const { width, height } = tip.getBoundingClientRect();
    tip.style.left = `${Math.max(8, Math.min(anchor.left, window.innerWidth - width - 8))}px`;
    const top = anchor.top >= height + 8 ? anchor.top - height : anchor.bottom;
    tip.style.top = `${Math.max(8, Math.min(top, window.innerHeight - height - 8))}px`;
    const dismiss = () => setHint(null);
    const escape = (event: KeyboardEvent) => { if (event.key === 'Escape') dismiss(); };
    window.addEventListener('resize', dismiss);
    document.addEventListener('scroll', dismiss, true);
    document.addEventListener('keydown', escape);
    return () => {
      tip.hidePopover();
      window.removeEventListener('resize', dismiss);
      document.removeEventListener('scroll', dismiss, true);
      document.removeEventListener('keydown', escape);
    };
  }, [hint]);
  useEffect(() => { if (!graphs.includes(graph)) setGraph(graphs[0] ?? ''); }, [graphs, graph]);
  useEffect(() => { const timer = window.setInterval(() => setNow(Date.now()), 15000); return () => clearInterval(timer); }, []);
  const today = new Date(now); today.setHours(0, 0, 0, 0);
  const todayKey = day(today);
  const first = addDays(today, -29 - page * 30);
  const last = addDays(today, -page * 30);
  const rangeEnd = page === 0 ? addDays(today, 7) : last;
  const rows = [...Array.from({ length: 30 }, (_, index) => addDays(first, index)),
    ...(page === 0 ? Array.from({ length: 7 }, (_, index) => addDays(today, index + 1)) : [])];

  const runs = (data?.runs ?? []).filter(run => !graphFilter || run.graph === graphFilter);
  // A scheduled occurrence with a Run is already represented by that Run's actual duration.
  const plans = (data?.scheduled ?? []).filter(plan => !plan.run && (!graphFilter || plan.graph === graphFilter));
  const entries: Entry[] = [
    ...runs.flatMap(run => timelineEntries(run, now)),
    ...plans.map(plan => ({ id: `${plan.schedule}-${plan.scheduled_at}`, graph: plan.graph,
      start: +new Date(plan.scheduled_at), end: +new Date(plan.scheduled_at),
      status: plan.status, source: 'schedule', plan })),
  ].filter(entry => Number.isFinite(entry.start) && Number.isFinite(entry.end));
  const matching = entries.filter(entry => {
    if (sourceFilter && entry.source !== sourceFilter) return false;
    if (chainFilter && (!entry.run || (entry.run.trigger?.root_run ?? entry.run.run) !== chainFilter)) return false;
    if (filter === 'attention') return needsAttention(entry.status);
    if (filter === 'finished') return ['finished', 'completed'].includes(entry.status);
    return filter === 'all' || entry.status === filter;
  }).sort((a, b) => a.start - b.start);
  const rowData = rows.map(date => {
    const end = +addDays(date, 1);
    return { date, items: placeEntries(matching.filter(entry => entry.start < end && Math.max(entry.start, entry.end) >= +date), date) };
  });
  const nextPlan = (data?.schedules ?? []).filter(item => item.enabled && (!graphFilter || item.graph === graphFilter) && +new Date(item.next_at) > now)
    .sort((a, b) => +new Date(a.next_at) - +new Date(b.next_at))[0];
  const running = runs.filter(run => run.running);
  const todayRuns = runs.filter(run => day(new Date(run.started)) === todayKey);
  const issues = entries.filter(entry => needsAttention(entry.status) && entry.start >= +first && entry.start < +addDays(last, 1));
  const focusToday = () => {
    const area = surface.current;
    const target = area?.querySelector<HTMLElement>('.timeline-day.today');
    if (area && target) area.scrollTop += target.getBoundingClientRect().top - area.getBoundingClientRect().top - 120;
  };
  const focusLatest = () => {
    const area = surface.current;
    const days = [...(area?.querySelectorAll<HTMLElement>('.timeline-day.has-events') ?? [])]
      .filter(item => item.dataset.date && item.dataset.date <= todayKey);
    const target = days.at(-1);
    if (area && target) area.scrollTop += target.getBoundingClientRect().top - area.getBoundingClientRect().top - 120;
  };
  useEffect(() => {
    if (!data || positioned.current === `${page}:${todayKey}`) return;
    positioned.current = `${page}:${todayKey}`;
    if (page === 0 && requestedFocus.current === 'latest') {
      requestedFocus.current = 'today';
      requestAnimationFrame(focusLatest);
    } else if (page === 0) focusToday();
    else if (surface.current) surface.current.scrollTop = 0;
  }, [data, page, todayKey]);
  const reset = () => { setFilter('all'); setGraphFilter(''); setSourceFilter(''); setChainFilter(''); };

  const create = async () => {
    setPending(true); setError('');
    try {
      const scheduleRule = rule === 'once' ? { type: rule, at } : rule === 'interval' ? { type: rule, seconds: interval }
        : rule === 'daily' ? { type: rule, time } : rule === 'weekly' ? { type: rule, time, weekdays } : { type: rule, time, day: monthDay };
      const input = scheduleInput.trim() ? JSON.parse(scheduleInput) : {};
      if (!input || Array.isArray(input) || typeof input !== 'object') throw new Error('计划输入必须是 JSON object');
      await api('/schedules', 'POST', { graph, rule: scheduleRule, input });
      setAt(''); onRefresh(); setScheduleOpen(false);
    } catch (cause) { setError((cause as Error).message); }
    finally { setPending(false); }
  };
  const remove = async (id: string) => {
    setPending(true); setError('');
    try { await api(`/schedules/${encodeURIComponent(id)}`, 'DELETE'); onRefresh(); }
    catch (cause) { setError((cause as Error).message); }
    finally { setPending(false); }
  };
  // Keep empty dates accessible without filling the initial view with weeks of empty rows.
  let emptyStart = 0;

  return <main className="timeline-board">
    <header className="timeline-header">
      <div className="timeline-title"><h2>运行看板</h2><p>运行进度与定时计划</p></div>
      <div className="timeline-actions"><button disabled={!scheduling} title={!scheduling ? '定时计划尚未由此运行时托管' : undefined} onClick={() => { setError(''); setScheduleOpen(true); }}><CalendarDays size={16} />管理计划 <span className="count">{data?.schedules.length ?? 0}</span></button>
        <button className="primary" disabled={!graphs.length || !scheduling} title={!scheduling ? '定时计划尚未由此运行时托管' : undefined} onClick={() => { setError(''); setScheduleOpen(true); }}><Plus size={16} />添加计划</button></div>
    </header>
    <section className="timeline-summary" aria-label="运行概览">
      <button className="summary-card summary-running" onClick={() => { reset(); setFilter('running'); onPage(0); }}><span><Activity size={15} />正在运行</span><strong>{data ? running.length : '—'}<small> 个</small></strong><small>{running[0]?.graph ?? '当前没有执行中的工作流'}</small></button>
      <button className="summary-card" onClick={() => { reset(); setFilter('planned'); onPage(0); }}><span><Clock3 size={15} />下一次计划</span><strong>{nextPlan ? clockText(nextPlan.next_at) : '—'}</strong><small>{nextPlan ? `${dateText(nextPlan.next_at)} · ${nextPlan.graph}` : '尚未安排未来运行'}</small></button>
      <button className="summary-card" onClick={() => { reset(); onPage(0); focusToday(); }}><span><CalendarDays size={15} />今天启动</span><strong>{data ? todayRuns.length : '—'}<small> 次</small></strong><small>{todayRuns.filter(run => ['finished', 'completed'].includes(run.status)).length} 次已完成</small></button>
      <button className={`summary-card ${issues.length ? 'summary-alert' : ''}`} onClick={() => setFilter('attention')}><span>需要注意</span><strong>{data ? issues.length : '—'}<small> 项</small></strong><small>本页历史 · 失败、中断或错过</small></button>
    </section>

    <section className="timeline-panel" aria-label="运行时间线">
      <div className="timeline-toolbar">
        <div className="timeline-navigation"><button aria-label="查看更早记录" onClick={() => onPage(page + 1)}><ChevronLeft size={16} /></button>
          <button disabled={!page} aria-label="查看更新记录" onClick={() => onPage(page - 1)}><ChevronRight size={16} /></button>
          <strong>{day(first)} — {day(rangeEnd)}</strong><button onClick={() => { onPage(0); focusToday(); }}>今天</button>
          <button onClick={() => { requestedFocus.current = 'latest'; onPage(0); if (page === 0) requestAnimationFrame(focusLatest); }}>最近活动</button></div>
        <span className="timeline-range-note">{scheduling ? '历史每页 30 天 · 未来 7 天' : '运行历史 · 定时计划由其他宿主提供'}</span>
      </div>
      <div className="timeline-controls">
        <div className="timeline-filters">
          <select aria-label="筛选 Graph" value={graphFilter} onChange={event => setGraphFilter(event.target.value)}><option value="">全部 Graph</option>{graphs.map(name => <option key={name}>{name}</option>)}</select>
          <select aria-label="筛选状态" value={filter} onChange={event => setFilter(event.target.value as Filter)}>
            <option value="all">全部状态</option><option value="running">执行中</option><option value="finished">已完成</option><option value="attention">仅异常 / 错过</option><option value="planned">仅未来计划</option><option value="stopped">已停止</option>
          </select>
          <select aria-label="筛选触发方式" value={sourceFilter} onChange={event => setSourceFilter(event.target.value)}><option value="">全部触发方式</option><option value="manual">手动</option><option value="schedule">定时</option><option value="webhook">Webhook</option><option value="graph_call">工作流调用</option><option value="channel">通道</option></select>
          <select aria-label="筛选调用链" value={chainFilter} onChange={event => setChainFilter(event.target.value)}><option value="">全部调用链</option>
            {[...new Set((data?.runs ?? []).filter(run => run.trigger?.source === 'graph_call').map(run => run.trigger?.root_run ?? run.trigger?.run).filter((run): run is string => Boolean(run)))].map(run => <option key={run} value={run}>{run}</option>)}
          </select>
          {(graphFilter || filter !== 'all' || sourceFilter || chainFilter) && <button className="timeline-clear" onClick={reset}>清除筛选</button>}
        </div>
        <label className="timeline-expand"><input type="checkbox" checked={expanded} onChange={event => setExpanded(event.target.checked)} />展开空白日期</label>
      </div>
      {problem && <p className="timeline-error" role="status">数据暂未更新。<button onClick={onRefresh}>重新连接</button></p>}
      {!data ? <p className="timeline-loading">{problem ? '无法加载运行记录' : '正在读取运行记录…'}</p> : <>
        <div className="timeline-surface" ref={surface} tabIndex={0} aria-label="可滚动的每日时间线">
          <div className="timeline-scale"><span>日期 / 工作流</span><div>{[0, 6, 12, 18, 24].map(hour => <b key={hour} style={{ left: `${hour / 24 * 100}%` }}>{String(hour).padStart(2, '0')}:00</b>)}</div></div>
          {rowData.map((row, index) => {
            const isToday = day(row.date) === todayKey;
            const isEmptyPast = +row.date < +today && !row.items.length && !expanded;
            if (isEmptyPast) {
              if (index === 0 || rowData[index - 1].items.length) emptyStart = index;
              const next = rowData[index + 1];
              if (next && +next.date < +today && !next.items.length) return null;
              return <button className="timeline-gap" key={day(row.date)} onClick={() => setExpanded(true)}>{day(rowData[emptyStart].date)}{emptyStart !== index && ` — ${day(row.date)}`} · {index - emptyStart + 1} 天无匹配记录 <span>展开</span></button>;
            }
            const dayLength = +addDays(row.date, 1) - +row.date;
            const lanes = row.items.length ? Math.max(...row.items.map(item => item.lane)) + 1 : 0;
            return <section className={`timeline-day ${isToday ? 'today' : ''} ${row.items.length ? 'has-events' : ''}`} data-date={day(row.date)} key={day(row.date)} style={{ minHeight: Math.max(62, lanes * 57 + 12) }}>
              <div className="timeline-day-meta"><time dateTime={day(row.date)}>{row.date.toLocaleDateString('zh-CN', { month: '2-digit', day: '2-digit' })}</time><span>{row.date.toLocaleDateString('zh-CN', { weekday: 'short' })}</span>{isToday && <em>今天</em>}</div>
              <div className="timeline-track">
                {[0, 25, 50, 75, 100].map(mark => <i className="timeline-gridline" key={mark} style={{ left: `${mark}%` }} />)}
                {isToday && <i className="timeline-now" style={{ left: `${(now - +row.date) / dayLength * 100}%` }}><span>现在 {clockText(now)}</span></i>}
                {row.items.map(entry => {
                  const left = (entry.start - +row.date) / dayLength * 100;
                  const width = (entry.end - entry.start) / dayLength * 100;
                  return <button key={entry.id} className={`timeline-entry ${entry.run ? 'timeline-run' : 'timeline-plan'} ${entry.status}`}
                    data-run-id={entry.run?.run}
                    style={{ top: entry.lane * 57 + (entry.run ? 37 : 36), left: `${left}%`, width: entry.run ? `${width}%` : undefined, '--graph-color': graphColors[entry.graph] } as CSSProperties}
                    aria-label={`${entry.graph}，${statusText(entry.status)}，${clockText(entry.start)}`}
                    aria-describedby={hint?.anchor.dataset.hintKey === `${day(row.date)}:${entry.id}` ? hintId : undefined}
                    data-hint-key={`${day(row.date)}:${entry.id}`}
                    onMouseEnter={event => setHint({ entry, anchor: event.currentTarget })}
                    onMouseLeave={event => { if (!tooltip.current?.contains(event.relatedTarget as Node | null)) setHint(null); }}
                    onFocus={event => setHint({ entry, anchor: event.currentTarget })}
                    onBlur={() => setHint(null)}
                    onClick={() => { setHint(null); setSelected(entry); }}>
                    <i className={entry.run ? 'timeline-duration' : 'timeline-point'} />
                  </button>;
                })}
                {!row.items.length && <span className="timeline-no-events">{+row.date > +today ? '暂无计划' : '没有匹配的运行'}</span>}
              </div>
            </section>;
          })}
        </div>
        <footer className="timeline-footer"><div className="timeline-legend"><span><i className="finished" />实际运行</span><span><i className="planned" />计划时点</span><span><i className="missed_busy" />已错过</span></div><span>悬停查看状态 · 点击查看详情</span></footer>
      </>}
    </section>

    <div ref={tooltip} id={hintId} popover="manual" role="tooltip" className="timeline-entry-label"
      style={{ '--graph-color': hint ? graphColors[hint.entry.graph] : undefined } as CSSProperties}
      onMouseLeave={event => { if (!hint?.anchor.contains(event.relatedTarget as Node | null)) setHint(null); }}>
      {hint && <><strong>{hint.entry.graph}</strong><small>{sourceText(hint.entry.source)} · {clockText(hint.entry.start)} · {statusText(hint.entry.status)}{hint.entry.run && (hint.entry.run.running || hasRunEnd(hint.entry.run)) ? ` · ${duration(hint.entry.end - hint.entry.start)}` : ''}</small></>}
    </div>
    {selected && <RunPreview key={selected.id} entry={selected} data={data} onClose={() => setSelected(null)} onOpen={onSelect} onOpenRun={onOpenRun} />}
    {scheduleOpen && <Modal title="定时计划" close={() => { if (!pending) setScheduleOpen(false); }} className="timeline-schedule-dialog">
      <p className="timeline-muted">按本机时间执行。停机或 Graph 忙碌时跳过，不补跑。</p>
      <form onSubmit={event => { event.preventDefault(); void create(); }}>
        <label>工作流<select required value={graph} onChange={event => setGraph(event.target.value)}>{graphs.map(name => <option key={name}>{name}</option>)}</select></label>
        <label>规则<select value={rule} onChange={event => setRule(event.target.value)}><option value="once">未来执行一次</option><option value="interval">固定间隔</option><option value="daily">每天</option><option value="weekly">每周</option><option value="monthly">每月</option></select></label>
        {rule === 'once' ? <label>执行时间<input required type="datetime-local" value={at} onChange={event => setAt(event.target.value)} /></label> : rule === 'interval' ? <label>间隔秒数<input required type="number" min="1" step="1" value={interval} onChange={event => setInterval(Number(event.target.value))} /></label> : <label>本机时间<input required type="time" value={time} onChange={event => setTime(event.target.value)} /></label>}
        {rule === 'weekly' && <fieldset className="weekdays"><legend>星期</legend>{['一', '二', '三', '四', '五', '六', '日'].map((label, index) => <label key={index}><input type="checkbox" checked={weekdays.includes(index)} onChange={event => setWeekdays(old => event.target.checked ? [...old, index].sort() : old.filter(value => value !== index))} />{label}</label>)}</fieldset>}
        {rule === 'monthly' && <label>每月日期<input required type="number" min="1" max="31" value={monthDay} onChange={event => setMonthDay(Number(event.target.value))} /></label>}
        <label>运行输入（可选 JSON）<textarea rows={3} value={scheduleInput} onChange={event => setScheduleInput(event.target.value)} placeholder="{}" /></label>
        {error && <p className="timeline-error" role="alert">{error}</p>}
        <button className="primary" disabled={pending || !graph} type="submit"><Plus size={15} />{pending ? '正在保存…' : '保存计划'}</button>
      </form>
      <section className="schedule-list" aria-label="已配置的计划"><h3>已配置 · {data?.schedules.length ?? 0}</h3>
        {!data?.schedules.length && <p className="timeline-muted">还没有计划。保存后即可在时间线上查看。</p>}
        {data?.schedules.map(item => <div className="schedule-item" key={item.id}><div><strong>{item.graph}</strong><span>{ruleText(item.rule)}</span><small>{item.enabled ? `下次 ${dateText(item.next_at)}` : '已结束'}</small></div><button disabled={pending} aria-label={`删除 ${item.graph} 计划`} onClick={() => void remove(item.id)}><Trash2 size={15} /></button></div>)}
      </section>
    </Modal>}
  </main>;
}

function RunPreview({ entry, data, onClose, onOpen, onOpenRun }: { entry: Entry; data: TimelineData | null; onClose: () => void;
  onOpen: (run: OurRun) => void; onOpenRun?: (run: string, graph: string) => void }) {
  const [detail, setDetail] = useState<OurRunDetail | null>(null);
  const [error, setError] = useState('');
  const [attempt, setAttempt] = useState(0);
  const run = entry.run && (data?.runs.find(run => run.run === entry.run!.run) ?? entry.run);
  const schedule: Schedule | undefined = data?.schedules.find(schedule => schedule.id === entry.plan?.schedule);
  useEffect(() => {
    if (!entry.run) return;
    let active = true;
    setError('');
    void api<OurRunDetail>(`/runs/${encodeURIComponent(entry.run.run)}`).then(result => { if (active) setDetail(result); })
      .catch(cause => { if (active) setError((cause as Error).message); });
    return () => { active = false; };
  }, [entry.id, attempt]);
  const status = run ? runStatus(run) : entry.status;
  const interval = run ? intervalFacts(run, entry, Date.now()) : null;
  return <Modal title={run ? '运行详情' : '计划详情'} close={onClose} className="timeline-drawer">
    <span className={`board-badge ${status}`}>{statusText(status)}</span><h3 className="preview-graph">{entry.graph}</h3>
    {run?.objective && <p className="preview-objective">{run.objective}</p>}
    <dl className="preview-facts">
      <dt>触发方式</dt><dd>{sourceText(entry.source)}</dd>
      {run?.trigger?.source === 'graph_call' && <>
        <dt>调用方式</dt><dd>{run.trigger.mode ? (run.trigger.mode === 'wait' ? '等待目标完成' : '启动后独立运行') : '工作流调用'}</dd>
        <dt>调用来源</dt><dd>{run.trigger.graph} / {run.trigger.node} · 第 {run.trigger.invocation} 轮<br />{run.trigger.run}</dd>
        <dt>调用链</dt><dd>{run.trigger.root_run ?? run.trigger.run}</dd>
      </>}
      <dt>{run ? '开始时间' : '计划时间'}</dt><dd>{dateText(entry.start)}</dd>
      {run && interval && <>
        <dt>{interval.label}</dt><dd>{interval.value}</dd>
        {interval.end && <><dt>结束时间</dt><dd>{interval.end}</dd></>}
        <dt>已执行步骤</dt><dd>{run.executed.length}</dd>
        {entry.resident && <><dt>实例启动</dt><dd>{dateText(run.started)}</dd><dt>活动时段</dt><dd>{run.activity?.length ?? 0} 段，等待期间不算执行</dd></>}
      </>}
      {schedule && <><dt>定时规则</dt><dd>{ruleText(schedule.rule)}</dd></>}
    </dl>
    {run?.trigger?.source === 'graph_call' && run.trigger.run && run.trigger.graph && <button className="full-button"
      onClick={() => onOpenRun?.(run.trigger!.run!, run.trigger!.graph!)}>查看来源运行 ↖</button>}
    {entry.plan && <p className="preview-note">{entry.status === 'planned' ? '这是计划开始的时点，实际时长将在执行后显示。' : entry.status === 'missed_busy' ? 'Graph 在该时点忙碌，本次未执行，也不会补跑。' : '该计划未执行，也不会补跑；错过原因以服务端记录为准。'}</p>}
    {detail?.state.error && <p className="timeline-error" role="alert">{detail.state.error}</p>}
    <section className="preview-input"><h3>运行输入</h3>{error ? <p role="alert">{error} <button onClick={() => setAttempt(value => value + 1)}>重试</button></p> : run && !detail ? <p>正在读取…</p> : <pre>{JSON.stringify(run ? detail?.state.input ?? {} : schedule?.input ?? {}, null, 2)}</pre>}</section>
    {run && <><p className="timeline-muted">Run · {run.run}</p><button className="primary preview-open" onClick={() => onOpen(run)}>查看节点与产物<ArrowUpRight size={16} /></button></>}
  </Modal>;
}
