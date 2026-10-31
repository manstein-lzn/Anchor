import { Activity, Trash2 } from 'lucide-react';
import { GraphRunButton } from './GraphRunButton';
import { Workspace } from './Workspace';
import { WorkflowCanvas } from './WorkflowCanvas';
import { RunInspector } from './RunInspector';
import { RunControls } from './RunControls';
import { runPhase, runPhaseLabel, type RunControl } from './runPresentation';
import { activeNodes } from './parallel';
import type { GraphSummary, OurRunDetail } from './model';
import type { WorkbenchNotice } from './useGraphEditor';

type Props = {
  name: string; graphs: GraphSummary[]; dirty: boolean; busy: boolean; notice: WorkbenchNotice;
  run: string; node: string; targetPath: string; detail: OurRunDetail | null;
  onPilot: () => void; selectGraph: (graph: string) => boolean; trigger: () => Promise<void>; onTimeline: () => void;
  navigateRun: (run: string, graph: string, node?: string) => Promise<void>;
  controlRun: (what: RunControl) => Promise<void>; deleteRun: () => Promise<void>; onNode: (node: string) => void;
};
export function RunDetailPage({ name, graphs, dirty, busy, notice, run, node, targetPath, detail,
  selectGraph, trigger, onTimeline, navigateRun, controlRun, deleteRun, onNode, onPilot }: Props) {
  const phase = detail ? runPhase(detail) : 'unavailable';
  const cursors = activeNodes(detail?.state);
  const cursorLabel = runPhaseLabel(detail?.projection);
  return (
        <Workspace running selection={`${run}/${node}/${targetPath}`}>{({ openInspector, inspectorControls, restoreControls }) => <>
          <main className="main timeline-inspector">
            <div className="document-header"><div className="document-title"><h2>{name || '运行概览'}</h2></div>
              <div className="document-context-actions">
                {restoreControls}
                <select aria-label="当前工作流" value={name} onChange={event => selectGraph(event.target.value)}>
                  {graphs.map(item => <option key={item.graph} value={item.graph}>{item.graph}{item.running ? '（执行中）' : ''}</option>)}
                </select>
                <GraphRunButton graph={graphs.find(item => item.graph === name)} name={name} dirty={dirty} busy={busy} onClick={trigger} />
                <button disabled={!run} onClick={onPilot}>在 Pilot 中诊断</button>
                <button onClick={() => onTimeline()}><Activity size={14} />返回时间线</button>
              </div>
            </div>
            {detail?.state.trigger?.source === 'graph_call' && detail.state.trigger.run && <div className="run-origin">
              由 {detail.state.trigger.graph} / {detail.state.trigger.node} · 第 {detail.state.trigger.invocation} 轮发起
              <button onClick={() => void navigateRun(detail.state.trigger!.run!, detail.state.trigger!.graph!, detail.state.trigger!.node)}>返回来源运行 ←</button>
            </div>}
            {notice && <p className={`notice ${notice.kind}`} role="status">{notice.text}</p>}
            <div className="canvas-head">
              {detail ? <>
                <span className={`pill ${phase}`}>{runPhaseLabel(detail.projection)}</span>
                <span className="objective">{detail.state.objective}</span>
                {cursors.length > 0 && <span className="hint" aria-label="活动节点">
                  {cursorLabel} {cursors.map(item => `${item.node}（第 ${item.pass} 轮）`).join('、')}</span>}
                <RunControls projection={detail.projection} busy={busy} onControl={operation => void controlRun(operation)} />
                {detail.projection?.active && phase === 'waiting' && <span className="hint" role="status">等待当前执行者退出后继续。</span>}
                {detail.projection?.reason && phase === 'settled' && <span className="problem">{detail.projection.reason}</span>}
                {detail.projection && !detail.projection.active ?
                  <button className="danger-link" onClick={() => void deleteRun()} disabled={busy}>
                    <Trash2 size={14} />删除运行记录
                  </button> : null}
              </> : <span className="hint">选一次运行，或触发一次。</span>}
            </div>
            {detail?.definition && <WorkflowCanvas graph={detail.definition} name={detail.graph} mode="run" state={detail.state} projection={detail.projection}
              onPick={value => { if (value?.kind === 'node') { onNode(value.id); openInspector(); } }} />}
            {detail && !detail.definition && <p className="problem" role="alert">该运行缺少冻结拓扑，无法显示运行画布。请更新 Host 后重试。</p>}
            <div className="canvas-bottom"><span className="legend"><i className="dot running" />执行中<i className="dot finished" />已完成<i className="dot failed" />失败</span><span className="canvas-caption">点击节点查看对话与产物</span></div>
          </main>
          <RunInspector key={`${run}/${node}/${targetPath}`} run={run} node={node} detail={detail} targetPath={targetPath} onOpenRun={navigateRun} controls={inspectorControls} />
        </>}</Workspace>
  );
}
