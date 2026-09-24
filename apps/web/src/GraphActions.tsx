import { useEffect, useId, useRef, useState } from 'react';
import { GitBranch, MoreHorizontal, Trash2 } from 'lucide-react';
import { Modal } from './ui';

/** Actions belong to this row, independently of the graph open in the editor. */
export function GraphActions({ graph, running, runCount, busy, onDelete }:
  { graph: string; running: boolean; runCount: number; busy: boolean;
    onDelete: (graph: string) => Promise<void> }) {
  const id = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => { if (confirming) cancel.current?.focus(); }, [confirming]);
  useEffect(() => {
    if (!open) return;
    const dismiss = () => menu.current?.hidePopover();
    window.addEventListener('resize', dismiss);
    document.addEventListener('scroll', dismiss, true);
    return () => {
      window.removeEventListener('resize', dismiss);
      document.removeEventListener('scroll', dismiss, true);
    };
  }, [open]);
  const close = () => {
    if (deleting) return;
    setConfirming(false);
    requestAnimationFrame(() => trigger.current?.focus());
  };

  return <>
    <button ref={trigger} className="graph-actions-trigger" aria-label={`管理工作流 ${graph}`}
            aria-haspopup="menu" aria-expanded={open} popoverTarget={id}
            onClick={event => {
              const rect = event.currentTarget.getBoundingClientRect();
              if (menu.current) {
                menu.current.style.left = `${Math.max(8, Math.min(rect.right - 192, window.innerWidth - 200))}px`;
                menu.current.style.top = `${rect.bottom + 130 > window.innerHeight ? rect.top - 130 : rect.bottom + 6}px`;
              }
            }}>
      <MoreHorizontal size={16} />
    </button>
    <div ref={menu} id={id} popover="auto" role="menu" aria-label={`${graph} 的操作`}
         className="graph-actions-menu" onToggle={event => {
           setOpen(event.newState === 'open');
           if (event.newState === 'open') menu.current?.querySelector<HTMLButtonElement>('button')?.focus();
         }}
         onKeyDown={event => {
           if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
             event.preventDefault();
             menu.current?.querySelector<HTMLButtonElement>('button')?.focus();
           }
         }}>
      <button role="menuitem" autoFocus aria-disabled={busy || running} onClick={() => {
        if (busy || running) return;
        menu.current?.hidePopover();
        setError(''); setConfirming(true);
      }}><Trash2 size={15} />删除工作流</button>
      {running && <p>请先停止运行，再删除此工作流。</p>}
    </div>
    {confirming && <Modal title="删除工作流？" close={close} className="delete-graph-dialog">
      <div className="delete-graph-target"><GitBranch size={18} /><strong>{graph}</strong></div>
      <p className="delete-graph-description">以下内容将被永久删除，无法恢复：</p>
      <ul className="delete-graph-scope">
        <li>工作流定义</li>
        <li>全部运行记录（当前 {runCount} 条）</li>
        <li>所有运行的工作区文件、产物和 Git 历史</li>
      </ul>
      {running && <p className="problem" role="alert">此工作流正在运行，请先停止运行。</p>}
      {error && <p className="problem" role="alert">{error}</p>}
      <div className="modal-actions">
        <button ref={cancel} className="cancel-delete" disabled={deleting} onClick={close}>取消</button>
        <button className="confirm-delete" disabled={busy || running || deleting} onClick={async () => {
          setDeleting(true); setError('');
          try { await onDelete(graph); }
          catch (cause) { setError(`删除失败：${(cause as Error).message}`); }
          finally { setDeleting(false); }
        }}>{deleting ? '正在删除…' : '永久删除'}</button>
      </div>
    </Modal>}
  </>;
}
