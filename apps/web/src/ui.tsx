import { useEffect, useId, useRef, useState, type ButtonHTMLAttributes, type ReactNode } from 'react';
import { X, type LucideIcon } from 'lucide-react';

export function ToolButton({ icon: Icon, label, ...rest }:
  { icon: LucideIcon; label: string } & ButtonHTMLAttributes<HTMLButtonElement>) {
  return <button type="button" aria-label={label} title={label} {...rest}><Icon size={16} /></button>;
}

export function Modal({ title, close, children, className = '' }:
  { title: string; close: () => void; children: ReactNode; className?: string }) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => { ref.current?.showModal(); }, []);
  return <dialog ref={ref} className={`modal ${className}`} aria-labelledby={titleId}
    onCancel={event => { event.preventDefault(); close(); }} onClick={event => { if (event.target === event.currentTarget) close(); }}>
    <div className="modal-content">
      <header><h2 id={titleId}>{title}</h2><ToolButton icon={X} label="关闭" onClick={close} /></header>
      {children}
    </div>
  </dialog>;
}

export function JsonDialog({ title, value, apply, close }:
  { title: string; value: unknown; apply: (value: unknown) => void; close: () => void }) {
  const [text, setText] = useState(() => JSON.stringify(value, null, 2));
  const [error, setError] = useState('');
  return <Modal title={title} close={close}>
    <textarea className="json-editor" aria-label={title} spellCheck={false} value={text}
      onChange={event => { setText(event.target.value); setError(''); }} />
    {error && <p className="notice bad" role="alert">{error}</p>}
    <div className="modal-actions"><button className="primary" onClick={() => {
      try { apply(JSON.parse(text)); close(); }
      catch (problem) { setError(`JSON 语法错误：${(problem as Error).message}`); }
    }}>应用更改</button></div>
  </Modal>;
}

export function EmptyState({ icon: Icon, title, children }:
  { icon: LucideIcon; title: string; children: ReactNode }) {
  return <div className="empty-state"><Icon size={26} strokeWidth={1.4} />
    <strong>{title}</strong><p>{children}</p></div>;
}
