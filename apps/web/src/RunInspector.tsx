import { useEffect, useMemo, useState } from 'react';
import { FolderOpen, MessageSquare, SlidersHorizontal } from 'lucide-react';
import { Files } from './Files';
import { Transcript } from './Transcript';
import { Plugins } from './Plugins';
import { EmptyState } from './ui';
import { callModeLabel, type OurRunDetail } from './model';
import { label } from './execution';

/** Inspection state is local to the selected run/node, independent of graph editing. */
export function RunInspector({ run, node, detail, targetPath = '', onOpenRun }:
  { run: string; node: string; detail: OurRunDetail | null; targetPath?: string; onOpenRun?: (run: string, graph: string, node?: string) => void }) {
  const [tab, setTab] = useState<'talk' | 'files' | 'plugins'>(targetPath ? 'files' : 'talk');
  const [pass, setPass] = useState('');
  const passes = useMemo(() => Object.keys(detail?.traces ?? {})
    .filter(item => item === node || (item.startsWith(`${node}-`) && /^\d+$/.test(item.slice(node.length + 1))))
    .sort((a, b) => (a === node ? 0 : Number(a.slice(node.length + 1)))
      - (b === node ? 0 : Number(b.slice(node.length + 1)))), [detail, node]);
  const latest = passes[passes.length - 1] ?? '';
  const shown = passes.includes(pass) ? pass : latest;
  const messages = detail?.traces?.[shown] ?? [];
  const result = detail?.state.nodes[node];
  useEffect(() => { setPass(''); }, [node, detail?.run]);

  return <aside className="inspector">
    <div className="section-heading">
      <h3><SlidersHorizontal size={15} />{node || '执行详情'}</h3>
      {node && <div className="product-switch inspector-tabs" aria-label="节点详情">
        <button aria-pressed={tab === 'talk'} className={tab === 'talk' ? 'chosen' : ''}
          onClick={() => setTab('talk')}><MessageSquare size={14} />对话</button>
        <button aria-pressed={tab === 'files'} className={tab === 'files' ? 'chosen' : ''}
          onClick={() => setTab('files')}><FolderOpen size={14} />文件</button>
        <button aria-pressed={tab === 'plugins'} className={tab === 'plugins' ? 'chosen' : ''}
          onClick={() => setTab('plugins')}>Plugin</button>
      </div>}
    </div>
    {!!detail?.calls?.filter(call => !node || call.node === node).length && <section className="call-records" aria-label="工作流调用记录">
      <h3>独立运行</h3>
      {node && <p>本节点：{detail.state.cursor?.node === node ? '执行中' : result?.submitted ? '已完成' : result ? '失败' : '尚未完成'}</p>}
      {detail.calls.filter(call => !node || call.node === node).map(call => <article className="call-record" key={`${call.node}/${call.invocation}`}>
        <header><strong>{call.graph}</strong><span>第 {call.invocation} 轮 · {call.node}</span></header>
        <p>{callModeLabel(call.mode)} · {call.mode === 'detach' ? '已接纳独立运行' : ['running', 'queued', 'created', 'pending'].includes(call.status) ? '等待目标结果' : '目标已结束'}</p>
        <p>目标运行：<span className={`pill ${call.status}`}>{label(call.status)}</span></p>
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
    </EmptyState> : tab === 'plugins' ? <Plugins recorded={detail?.plugins?.[node] ?? []} />
    : tab === 'files' ? <><p className="inspector-note">显示 {node} 当前工作区的文件；历史轮次的对话不会改变这里的最新文件。</p><Files run={run} node={node} targetPath={targetPath} /></> : <>
      <div className="node-summary">
        <span className={`pill ${detail?.state.cursor?.node === node ? 'running' : result?.submitted ? 'finished' : result ? 'failed' : 'pending'}`}>
          {detail?.state.cursor?.node === node ? '执行中' : result ? (result.submitted ? '已提交' : '失败') : '未执行'}
        </span>
        <span>已执行 {detail?.state.passes[node] ?? 0} 轮</span>
        {result?.submission && <span className="node-summary-text" title={result.submission}>{result.submission.split('\n').find(line => line.trim())}</span>}
        {result?.route && <span>下一步：{result.route}</span>}
      </div>
      {passes.length > 1 && <div className="passes">
        {passes.map((item, index) => <button key={item} className={item === shown ? 'chosen' : ''}
          aria-pressed={item === shown} onClick={() => setPass(item)}>第 {index + 1} 轮</button>)}
      </div>}
      {!messages.length && <EmptyState icon={MessageSquare}
        title={result ? '该节点没有对话记录' : detail?.state.status === 'running' ? '等待执行记录' : '节点尚未执行'}>
        {result ? '它可能是确定性操作节点，或只留下了文件结果。' : detail?.state.status === 'running' ? '节点开始执行后，对话会自动出现在这里。' : '选择已执行的节点查看过程。'}
      </EmptyState>}
      <Transcript messages={messages} />
    </>}
  </aside>;
}
