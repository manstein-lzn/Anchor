/** Application navigation and cross-page references. Page owners retain drafts and subscribe to Host projections. */
import { Anchor, Activity, GitBranch, MessageSquare, Plug, Settings2 } from 'lucide-react';
import { lazy, Suspense, useEffect, useLayoutEffect, useMemo, useRef, useState, type MouseEvent } from 'react';
import type { OurRun, OurRunDetail } from './model';
import { GraphRelations } from './GraphRelations';
import { Workspace } from './Workspace';
import { GraphEditorPage } from './GraphEditorPage';
import { PageActivity } from './PageActivity';
import { graphRunAction } from './GraphRunButton';
import { ChannelSessions } from './ChannelSessions';
import { CreateGraphDialog, RunInputDialog } from './WorkbenchDialogs';
import { anchorRef, anchorTarget, type AnchorRef } from './links';
import { api } from './api';
import { Timeline } from './Timeline';
import { assignGraphColors } from './graphColors';
import { graphSaveNotice, useGraphEditor, type WorkbenchNotice } from './useGraphEditor';
import { useWorkbenchData, type WorkbenchView } from './useWorkbenchData';
import { useRunDetail } from './useRunDetail';

const Pilot = lazy(() => import('./Pilot').then(module => ({ default: module.Pilot })));
const RunDetailPage = lazy(() => import('./RunDetailPage').then(module => ({ default: module.RunDetailPage })));
const PluginManagement = lazy(() => import('./PluginManagement').then(module => ({ default: module.PluginManagement })));
const SettingsPage = lazy(() => import('./SettingsPage').then(module => ({ default: module.SettingsPage })));
const openingPage = <main className="main"><p role="status">正在打开页面…</p></main>;

const recentRuns = (runs: OurRun[], graph: string) => runs
  .filter(item => item.graph === graph)
  .sort((a, b) => Date.parse(b.updated || b.started) - Date.parse(a.updated || a.started));

export function App() {
  const [view, setView] = useState<WorkbenchView>(() => {
    try {
      const saved = localStorage.getItem('anchor:view');
      return saved === 'pilot' || saved === 'runs' || saved === 'plugins' || saved === 'settings' ? saved : 'graph';
    } catch { return 'graph'; }
  });
  useEffect(() => {
    try { localStorage.setItem('anchor:view', view === 'runDetail' ? 'runs' : view); }
    catch { /* Navigation still works without browser storage. */ }
  }, [view]);
  const browser = useWorkbenchData(view);
  const { graphs, runs, timeline, timelineProblem, timelinePage, setTimelinePage, problem, refresh } = browser;
  const [name, setName] = useState('');
  const nameRef = useRef(name); nameRef.current = name;
  const [run, setRun] = useState('');
  const [node, setNode] = useState('');
  const [targetPath, setTargetPath] = useState('');
  const [notice, setNotice] = useState<WorkbenchNotice>(null);
  const [busy, setBusy] = useState(false);
  const [dialog, setDialog] = useState<'create' | 'run' | null>(null);
  const [assistance, setAssistance] = useState('');
  const [relations, setRelations] = useState(false);
  const [sessions, setSessions] = useState(false);
  const [graphReturn, setGraphReturn] = useState<{ graph: string; node?: string } | null>(null);
  // The return session after opening a Graph, Run or artifact from Pilot.
  const [chat, setChat] = useState('');
  const navigation = useRef(0);
  useEffect(() => { navigation.current += 1; }, [view, name, run]);
  const editor = useGraphEditor(name, graphs, setNotice);
  const { doc, dirty, selectedNode } = editor;
  const detail = useRunDetail(run, view === 'runDetail', runs);
  const graphColors = useMemo(() => assignGraphColors([
    ...graphs.map(item => item.graph), ...runs.map(item => item.graph),
    ...(timeline?.runs ?? []).map(item => item.graph),
    ...(timeline?.scheduled ?? []).map(item => item.graph),
    ...(timeline?.schedules ?? []).map(item => item.graph),
  ]), [graphs, runs, timeline]);

  useLayoutEffect(() => {
    if (view === 'pilot' || nameRef.current) return;
    const initial = runs.find(item => item.running)?.graph ?? graphs[0]?.graph;
    if (initial) {
      nameRef.current = initial;
      setName(initial); setRun(runs.find(item => item.graph === initial)?.run ?? '');
    }
  }, [graphs, runs, view]);
  const selectGraph = (next: string): boolean => {
    if (next === name) return true;
    if (dirty && !window.confirm('当前工作流有未保存的修改，确定切换并放弃修改吗？')) return false;
    nameRef.current = next;
    setName(next);
    setRun(recentRuns(runs, next)[0]?.run ?? '');
    setNode('');
    setTargetPath('');
    return true;
  };

  const perform = async (what: string, action: () => Promise<void>) => {
    setBusy(true); setNotice(null);
    try { await action(); }
    catch (error) { setNotice({ kind: 'bad', text: `${what}失败：${(error as Error).message}` }); }
    finally { setBusy(false); }
  };

  const save = () => perform('保存', async () => {
    const activation = await editor.save(); setNotice({ kind: 'ok', text: graphSaveNotice(activation) });
  });
  const check = () => perform('校验', async () => {
    const activation = await editor.save(); setNotice({ kind: 'ok', text: graphSaveNotice(activation, true) });
  });
  const create = async () => {
    if (dirty && !window.confirm('当前工作流有未保存的修改，确定新建并放弃这些修改吗？')) return;
    setDialog('create');
  };

  const openReference = async (ref: AnchorRef) => {
    const version = navigation.current;
    let graph = ref.kind === 'graph' ? ref.graph : runs.find(item => item.run === ref.run)?.graph;
    // Pilot no longer polls Runs. Resolve a newly linked Run from its own Host resource.
    if (ref.kind !== 'graph' && !graph) {
      try { graph = (await api<OurRunDetail>(`/runs/${encodeURIComponent(ref.run)}`)).graph; }
      catch (error) { setNotice({ kind: 'bad', text: `打开引用失败：${(error as Error).message}` }); return; }
      if (version !== navigation.current) return;
    }
    const target = anchorTarget(ref, graph ?? nameRef.current);
    if (target.graph && !selectGraph(target.graph)) return;
    setRun(target.run); setNode(target.node); setTargetPath('path' in target ? target.path ?? '' : '');
    setView(target.view === 'runs' && target.run ? 'runDetail' : target.view);
  };

  // A Pilot reply links its objects with `#anchor/…`; the click opens the page that already shows
  // them, and the way back is the session it was opened from. Captured on the shell so every
  // rendered reply is covered without teaching the Markdown renderer about Anchor.
  const followReference = (event: MouseEvent<HTMLElement>) => {
    const element = event.target as HTMLElement | null;
    const link = element?.closest?.('a');
    const ref = anchorRef(link?.getAttribute('href'));
    if (!ref) return;
    event.preventDefault();
    void openReference(ref);
  };

  const trigger = async () => {
    if (!name) return;
    const action = graphRunAction(graphs.find(item => item.graph === name));
    if (action.kind === 'sessions') { setSessions(true); return; }
    if (action.kind === 'view') { setRun(action.run); setNode(''); setTargetPath(''); setView('runDetail'); return; }
    if (!dirty) setDialog('run');
  };
  const askPilot = (reference: 'graph' | 'run') => {
    const target = reference === 'run' ? `#anchor/run/${encodeURIComponent(run)}` : `#anchor/graph/${encodeURIComponent(name)}`;
    setAssistance(reference === 'run'
      ? `请诊断这次运行，先检查状态、错误和相关产物，再说明原因及建议。\n[运行 ${run}](${target})`
      : `请检查这个工作流并协助完善，先读取当前定义，再说明建议。\n[工作流 ${name}](${target})`);
    setChat(''); setView('pilot');
  };

  const openRun = (item: OurRun) => {
    if (item.graph !== name && !selectGraph(item.graph)) return;
    setRun(item.run); setNode(''); setTargetPath(''); setView('runDetail');
  };

  const navigateGraph = (target: string, targetNode?: string, remember = false, sourceNode?: string) => {
    const previous = { graph: name, node: sourceNode ?? selectedNode?.id };
    if (!selectGraph(target)) return;
    if (remember) setGraphReturn(previous);
    editor.focusNode(target, targetNode);
    setView('graph'); setRelations(false);
  };
  const navigateRun = async (targetRun: string, targetGraph: string, targetNode = '') => {
    if (!selectGraph(targetGraph)) return;
    setRun(targetRun); setNode(targetNode); setTargetPath(''); setView('runDetail');
  };

  const controlRun = (what: 'pause' | 'stop' | 'resume' | 'abandon') => perform(what, async () => {
    await api(`/runs/${run}/${what}`, 'POST');
    await refresh();
  });

  const deleteRun = () => perform('删除运行记录', async () => {
    if (!run) return;
    const version = navigation.current;
    // A channel turn or resident-assistant instance is one link of a conversation lineage: later
    // Runs read the Run they were handed over from, so the Host only deletes a link once nothing
    // newer still reads it and refuses out-of-order deletion with a concrete reason.
    const conversation = Boolean(detail?.state.trigger?.session);
    const question = conversation
      ? `确定删除渠道会话中的这次运行 ${run} 及其文件吗？更早和更新的轮次都会保留；如果更新的轮次仍要从它继承现场，Host 会拒绝并说明是哪一条。此操作不可恢复。`
      : `确定彻底删除运行记录 ${run} 及其所有文件吗？此操作不可恢复。`;
    if (!window.confirm(question)) return;
    await api(`/runs/${encodeURIComponent(run)}`, 'DELETE');
    if (version === navigation.current) {
      setRun(''); setNode(''); setTargetPath('');
      setNotice({ kind: 'ok', text: conversation ? '这次会话运行及其文件已删除，更新的轮次保持可用。' : '运行记录及其文件已删除。' });
    }
    await refresh();
  });

  const deleteGraph = async (target: string, precondition: string) => {
    setBusy(true);
    try {
      await api(`/graphs/${encodeURIComponent(target)}`, 'DELETE', undefined, undefined, { 'If-Match': `"${precondition}"` });
      browser.forgetGraph(target);
      if (nameRef.current === target) {
        const next = graphs.find(item => item.graph !== target)?.graph ?? '';
        nameRef.current = next;
        setName(next); setRun(runs.find(item => item.graph === next)?.run ?? '');
        setNode(''); setTargetPath('');
      }
      setNotice({ kind: 'ok', text: `工作流“${target}”及其运行历史已删除。` });
      await refresh();
    } finally { setBusy(false); }
  };

  return (
    <div className="app-shell" onClickCapture={followReference}>
      <header className="topbar">
        <div className="brand"><span className="brand-mark"><Anchor size={20} /></span>Anchor</div>
        <nav className="product-switch" aria-label="工作台">
          <button aria-pressed={view === 'graph'} className={view === 'graph' ? 'chosen' : ''} onClick={() => setView('graph')}><GitBranch size={15} />图编排</button>
          <button aria-pressed={view === 'runs' || view === 'runDetail'} className={view === 'runs' || view === 'runDetail' ? 'chosen' : ''} onClick={() => setView('runs')}><Activity size={15} />运行看板</button>
          <button aria-pressed={view === 'pilot'} className={view === 'pilot' ? 'chosen' : ''} onClick={() => setView('pilot')}><MessageSquare size={15} />Pilot</button>
          <button aria-pressed={view === 'plugins'} className={view === 'plugins' ? 'chosen' : ''} onClick={() => setView('plugins')}><Plug size={15} />插件库</button>
          <button aria-pressed={view === 'settings'} className={view === 'settings' ? 'chosen' : ''} onClick={() => setView('settings')}><Settings2 size={15} />设置</button>
        </nav>
        <div className="connection" title={problem || '服务已连接'}><span className={`status-dot ${problem ? 'bad' : ''}`} />
          {problem ? '连接中断' : '服务在线'}
        </div>
      </header>
      {problem && <div className="connection-error" role="alert">{problem}</div>}
      {view === 'runs' && notice && <p className={`notice ${notice.kind}`} role="status">{notice.text}</p>}

      <PageActivity active={view === 'pilot'}><Suspense fallback={openingPage}><Pilot session={chat} onSession={setChat} initialDraft={assistance} onDraftConsumed={() => setAssistance('')} onSettings={() => setView('settings')} /></Suspense></PageActivity>
      <Suspense fallback={openingPage}>{view === 'plugins' ? <PluginManagement /> : view === 'settings' ? <SettingsPage /> : view === 'runs' ? (
        <Workspace running inspector={false}>
          {timelineProblem ? <main className="main"><p className="timeline-error" role="alert">
            运行时间线无法加载：{timelineProblem} <button onClick={() => void refresh()}>重新连接</button>
          </p></main> : <Timeline data={timeline} graphs={graphs.map(item => item.graph)} graphColors={graphColors}
            page={timelinePage} onPage={setTimelinePage} onOpenRun={(id, graph) => void navigateRun(id, graph)}
            onRefresh={() => void refresh()} onSelect={openRun} onSessions={() => setSessions(true)} />}
        </Workspace>
      ) : view === 'runDetail' ? <RunDetailPage name={name} graphs={graphs} dirty={dirty} busy={busy}
        notice={notice} run={run} node={node} targetPath={targetPath} detail={detail} selectGraph={selectGraph}
        onPilot={() => askPilot('run')} trigger={trigger} onTimeline={() => setView('runs')} navigateRun={navigateRun} controlRun={controlRun}
        deleteRun={deleteRun} onNode={setNode} />
        : null}</Suspense>
      <PageActivity active={view === 'graph'}><GraphEditorPage name={name} editor={editor} graphs={graphs} runs={runs} graphColors={graphColors}
          busy={busy} notice={notice} setNotice={setNotice} selectGraph={selectGraph} create={create}
          onPilot={() => askPilot('graph')} deleteGraph={deleteGraph} save={save} check={check} trigger={trigger} openRun={openRun} navigateGraph={navigateGraph}
          onRelations={() => setRelations(true)} graphReturn={graphReturn}
          onReturn={() => { if (graphReturn) navigateGraph(graphReturn.graph, graphReturn.node); setGraphReturn(null); }} /></PageActivity>
      {sessions && <ChannelSessions onOpenRun={(id, graph) => void navigateRun(id, graph)} close={() => setSessions(false)} />}
      {relations && <GraphRelations graph={name} close={() => setRelations(false)} onOpen={navigateGraph} />}
      {dialog === 'create' && <CreateGraphDialog close={() => setDialog(null)} create={async wanted => {
        await api('/graphs', 'POST', { name: wanted });
        nameRef.current = wanted; setName(wanted); setRun(''); setNode(''); setTargetPath(''); setView('graph');
        await refresh();
      }} />}
      {dialog === 'run' && <RunInputDialog graph={name} initial={doc?.input ?? {}} close={() => setDialog(null)} run={async input => {
        if (graphRunAction(graphs.find(item => item.graph === name)).kind !== 'start') { setDialog(null); await trigger(); return; }
        const result = await api<{ run: string }>('/trigger', 'POST', { graph: name, input });
        setRun(result.run); setNode(''); setTargetPath(''); setView('runDetail'); await refresh();
      }} />}
    </div>
  );
}
