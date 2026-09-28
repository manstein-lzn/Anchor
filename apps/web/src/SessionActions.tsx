import { useEffect, useId, useRef, useState } from 'react';
import { MoreHorizontal, Pencil, Trash2 } from 'lucide-react';
import { Modal } from './ui';

export function SessionActions({ title, busy, onRename, onDelete }: {
  title: string; busy: boolean; onRename: (title: string) => Promise<void>; onDelete: () => Promise<void>;
}) {
  const id = useId();
  const menu = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const field = useRef<HTMLInputElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [action, setAction] = useState<'rename' | 'delete' | null>(null);
  const [name, setName] = useState(title);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    if (action === 'rename') { field.current?.focus(); field.current?.select(); }
    else if (action === 'delete') cancel.current?.focus();
  }, [action]);
  useEffect(() => {
    if (!open) return;
    const close = () => menu.current?.hidePopover();
    window.addEventListener('resize', close);
    document.addEventListener('scroll', close, true);
    return () => {
      window.removeEventListener('resize', close);
      document.removeEventListener('scroll', close, true);
    };
  }, [open]);
  const close = () => {
    if (pending) return;
    setAction(null);
    requestAnimationFrame(() => trigger.current?.focus());
  };
  const choose = (value: 'rename' | 'delete') => {
    menu.current?.hidePopover();
    setName(title); setError(''); setAction(value);
  };
  return <>
    <button ref={trigger} className="session-actions-trigger" aria-label={`管理对话 ${title}`}
      aria-haspopup="menu" aria-expanded={open} popoverTarget={id} onClick={event => {
        const rect = event.currentTarget.getBoundingClientRect();
        if (menu.current) {
          menu.current.style.left = `${Math.max(8, Math.min(rect.right - 180, innerWidth - 188))}px`;
          menu.current.style.top = `${rect.bottom + 100 > innerHeight ? rect.top - 100 : rect.bottom + 4}px`;
        }
      }}><MoreHorizontal size={17} /></button>
    <div ref={menu} id={id} popover="auto" role="menu" aria-label={`${title} 的操作`}
      className="session-actions-menu" onToggle={event => {
        setOpen(event.newState === 'open');
        if (event.newState === 'open') menu.current?.querySelector('button')?.focus();
      }} onKeyDown={event => {
        const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')];
        const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
        const next = event.key === 'ArrowDown' ? (index + 1) % buttons.length : event.key === 'ArrowUp'
          ? (index - 1 + buttons.length) % buttons.length : event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : -1;
        if (next >= 0) { event.preventDefault(); buttons[next]?.focus(); }
      }}>
      <button role="menuitem" onClick={() => choose('rename')}><Pencil size={15} />改名</button>
      <button role="menuitem" className="session-delete" disabled={busy} onClick={() => choose('delete')}><Trash2 size={15} />删除</button>
      {busy && <small>请先停止此对话的回复</small>}
    </div>
    {action && <Modal title={action === 'rename' ? '修改对话名称' : '删除对话？'} close={close} className="session-action-dialog">
      <form onSubmit={async event => {
        event.preventDefault();
        if (pending || (action === 'delete' && busy)) return;
        setPending(true); setError('');
        try {
          if (action === 'rename') await onRename(name.trim());
          else await onDelete();
          setAction(null);
          requestAnimationFrame(() => trigger.current?.focus());
        } catch (cause) { setError((cause as Error).message); }
        finally { setPending(false); }
      }}>
        {action === 'rename' ? <label>对话名称<input ref={field} required maxLength={120} value={name}
          disabled={pending} onChange={event => setName(event.target.value)} /></label> : <>
          <p>删除“{title}”及其对话历史？此操作无法撤销。</p>
          <p className="pilot-muted">关联的工作流、运行记录和产物会保留。</p>
        </>}
        {error && <p role="alert" className="pilot-error">{error}</p>}
        <div className="modal-actions"><button ref={cancel} type="button" className="cancel-delete" disabled={pending} onClick={close}>取消</button>
          <button type="submit" className={action === 'delete' ? 'confirm-delete' : 'primary'}
            disabled={pending || (action === 'rename' ? !name.trim() : busy)}>{pending ? (action === 'rename' ? '正在保存…' : '正在删除…') : action === 'rename' ? '保存' : '删除对话'}</button></div>
      </form>
    </Modal>}
  </>;
}
