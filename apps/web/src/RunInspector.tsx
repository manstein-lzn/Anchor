import { useMemo, useState } from 'react';
import { FolderOpen, MessageSquare, SlidersHorizontal } from 'lucide-react';
import { Files } from './Files';
import { Transcript } from './Transcript';
import { EmptyState } from './ui';
import type { OurRunDetail } from './model';

/** Inspection state is local to the selected run/node, independent of graph editing. */
export function RunInspector({ run, node, detail }:
  { run: string; node: string; detail: OurRunDetail | null }) {
  const [tab, setTab] = useState<'talk' | 'files'>('talk');
  const [pass, setPass] = useState('');
  const passes = useMemo(() => Object.keys(detail?.traces ?? {})
    .filter(item => item === node || (item.startsWith(`${node}-`) && /^\d+$/.test(item.slice(node.length + 1))))
    .sort((a, b) => (a === node ? 0 : Number(a.slice(node.length + 1)))
      - (b === node ? 0 : Number(b.slice(node.length + 1)))), [detail, node]);
  const shown = passes.includes(pass) ? pass : passes[0] ?? '';
  const messages = detail?.traces?.[shown] ?? [];

  return <aside className="inspector">
    <div className="section-heading">
      <h3><SlidersHorizontal size={15} />{node || '执行详情'}</h3>
      {node && <div className="product-switch inspector-tabs" aria-label="节点详情">
        <button aria-pressed={tab === 'talk'} className={tab === 'talk' ? 'chosen' : ''}
          onClick={() => setTab('talk')}><MessageSquare size={14} />对话</button>
        <button aria-pressed={tab === 'files'} className={tab === 'files' ? 'chosen' : ''}
          onClick={() => setTab('files')}><FolderOpen size={14} />文件</button>
      </div>}
    </div>
    {!node ? <EmptyState icon={MessageSquare} title="探索一次执行">
      在画布中选择节点，查看它的思考、执行过程和生成的文件。
    </EmptyState> : tab === 'files' ? <Files run={run} node={node} /> : <>
      {passes.length > 1 && <div className="passes">
        {passes.map((item, index) => <button key={item} className={item === shown ? 'chosen' : ''}
          aria-pressed={item === shown} onClick={() => setPass(item)}>第 {index + 1} 轮</button>)}
      </div>}
      {!messages.length && <EmptyState icon={MessageSquare} title="等待执行记录">
        节点开始执行后，对话会自动出现在这里。
      </EmptyState>}
      <Transcript messages={messages} />
    </>}
  </aside>;
}
