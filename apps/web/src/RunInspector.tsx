import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { FolderOpen, MessageSquare, SlidersHorizontal } from 'lucide-react';
import { Files } from './Files';
import { Transcript } from './Transcript';
import { Plugins } from './Plugins';
import { EmptyState } from './ui';
import { callModeLabel, type OurRunDetail } from './model';
import { tracePassKeys } from './parallel';
import { runNodePresentation, runPhase, runPhaseLabel } from './runPresentation';

/** Inspection state is local to the selected run/node, independent of graph editing. */
export function RunInspector({ run, node, detail, targetPath = '', onOpenRun, controls }:
  { run: string; node: string; detail: OurRunDetail | null; targetPath?: string; onOpenRun?: (run: string, graph: string, node?: string) => void; controls?: ReactNode }) {
  const [tab, setTab] = useState<'talk' | 'files' | 'plugins'>(targetPath ? 'files' : 'talk');
  const [pass, setPass] = useState('');
  const passes = useMemo(() => tracePassKeys(detail?.traces, node), [detail, node]);
  const latest = passes[passes.length - 1] ?? '';
  const shown = passes.includes(pass) ? pass : latest;
  const messages = detail?.traces?.[shown] ?? [];
  const result = detail?.state.nodes[node];
  const phase = detail ? runPhase(detail) : 'unavailable';
  const presentation = runNodePresentation(detail?.state, node, detail?.projection);
  const { atCursor, status: nodeStatus, summaryLabel: nodeLabel } = presentation;
  useEffect(() => { setPass(''); }, [node, detail?.run]);

  return <aside className="inspector">
    <div className="section-heading">
      <h3><SlidersHorizontal size={15} />{node || '执行详情'}</h3>
      {controls}
      {node && <div className="product-switch inspector-tabs" aria-label="节点详情">
        <button aria-pressed={tab === 'talk'} className={tab === 'talk' ? 'chosen' : ''}
          onClick={() => setTab('talk')}><MessageSquare size={14} />对话</button>
        <button aria-pressed={tab === 'files'} className={tab === 'files' ? 'chosen' : ''}
          onClick={() => setTab('files')}><FolderOpen size={14} />文件</button>
        <button aria-pressed={tab === 'plugins'} className={tab === 'plugins' ? 'chosen' : ''}
          onClick={() => setTab('plugins')}>插件</button>
      </div>}
    </div>
    {!!detail?.calls?.filter(call => !node || call.node === node).length && <section className="call-records" aria-label="工作流调用记录">
      <h3>独立运行</h3>
      {node && <p>本节点：{presentation.label}</p>}
      {detail.calls.filter(call => !node || call.node === node).map(call => <article className="call-record" key={`${call.node}/${call.invocation}`}>
        <header><strong>{call.graph}</strong><span>第 {call.invocation} 轮 · {call.node}</span></header>
        <p>{callModeLabel(call.mode)}</p>
        <p>目标运行：<span className={`pill ${runPhase(call)}`}>{runPhaseLabel(call.projection)}</span></p>
        <button className="full-button" onClick={() => onOpenRun?.(call.run, call.graph)}>查看目标运行 ↗</button>
        <small className="call-run-id">{call.run}</small>
        {call.summary && <p>{call.summary}</p>}
        {call.input && <details><summary>本次传入参数</summary><pre className="call-json">{JSON.stringify(call.input, null, 2)}</pre></details>}
        {call.result != null && <details><summary>返回结果</summary><pre className="call-json">{JSON.stringify(call.result, null, 2)}</pre></details>}
        <p className="inspector-note">{call.mode === 'detach' ? '目标独立继续；其后续状态不会改写已完成的调用节点。' : '停止来源运行会停止本次调用创建的目标运行。'}</p>
      </article>)}
    </section>}
    {!node ? <EmptyState icon={MessageSquare} title="探索一次执行">
      在画布中选择节点，查看它的思考、执行过程和生成的文件。
    </EmptyState> : tab === 'plugins' ? <Plugins key={`${run}/${node}`} recorded={detail?.plugins?.[node] ?? []} />
    : tab === 'files' ? <><p className="inspector-note">显示 {node}{result?.artifact ? ` 第 ${result.artifact.invocation} 轮` : ''}交付的固定版本文件。</p>
      <Files key={result?.artifact?.id ?? `${run}/${node}`} run={run} node={node} artifact={result?.artifact}
        availability={result?.artifact_availability} targetPath={targetPath} /></> : <>
      <div className="node-summary">
        <span className={`pill ${nodeStatus}`}>{nodeLabel}</span>
        <span>已执行 {detail?.state.passes[node] ?? 0} 轮</span>
        {result?.submission && <span className="node-summary-text" title={result.submission}>{result.submission.split('\n').find(line => line.trim())}</span>}
        {result?.route && <span>下一步：{result.route}</span>}
      </div>
      {passes.length > 1 && <div className="passes">
        {passes.map((item, index) => <button key={item} className={item === shown ? 'chosen' : ''}
          aria-pressed={item === shown} onClick={() => setPass(item)}>第 {index + 1} 轮</button>)}
      </div>}
      {!messages.length && <EmptyState icon={MessageSquare}
        title={result ? '该节点没有对话记录' : atCursor ? `${nodeLabel}，暂无对话记录` : phase === 'unavailable' ? '运行状态不可用' : '节点尚未执行'}>
        {result ? '它可能是确定性操作节点，或只留下了文件结果。' : phase === 'unavailable' ? '请重读 Host 的运行状态。' : atCursor ? '这里会保留节点产生的对话与工具记录。' : '选择已执行的节点查看过程。'}
      </EmptyState>}
      <Transcript messages={messages} />
    </>}
  </aside>;
}
