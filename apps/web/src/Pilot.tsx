import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { Anchor, ArrowDown, Check, Copy, LoaderCircle, MessageSquare, Plus, Search, Send, Square } from 'lucide-react';
import { api } from './api';
import { Markdown } from './markdown';
import { PilotTurn, type Turn } from './PilotTurn';

type Session = {
  id: string; title?: string; status: string; waiting_reason: string; updated_at: string;
  approval: { action: string; key: string; status: string; target?: string; proposal?: unknown } | null;
};
type Message = { role: 'user' | 'assistant'; text: string };
const labels: Record<string, string> = { active: '可以继续对话', interrupted: '回复已中断', waiting_user: '等待你的答复', archived: '已归档' };
const starters = ['查看现有的研究工作流', '帮我梳理一个研究问题', '查看最近运行的结果'];
const title = (session: Session) => session.title || '新对话';
const date = (value: string) => new Date(value).toLocaleDateString('zh-CN', { month: 'short', day: 'numeric' });
// Browser storage holds navigation and unsent drafts only; saved messages come from the server.
function cached(key: string, value?: string) {
  try {
    if (value !== undefined) localStorage.setItem(`pilot:${key}`, value);
    return localStorage.getItem(`pilot:${key}`) ?? '';
  } catch { return ''; }
}

export function Pilot() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selected, setSelected] = useState('');
  const selectedRef = useRef('');
  const [messages, setMessages] = useState<Message[]>([]);
  const [turn, setTurn] = useState<Turn | null>(null);
  const [draft, setDraft] = useState(() => cached('draft:new'));
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [problem, setProblem] = useState('');
  const [query, setQuery] = useState('');
  const [copied, setCopied] = useState<number | null>(null);
  const [atBottom, setAtBottom] = useState(true);
  const follow = useRef(true);
  const scroller = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);

  const editDraft = (value: string) => {
    cached(`draft:${selectedRef.current || 'new'}`, value);
    setDraft(value);
  };
  const activate = (id: string) => {
    selectedRef.current = id;
    setSelected(id); cached('selected', id);
    setDraft(cached(`draft:${id || 'new'}`));
    setMessages([]); setTurn(null); setCopied(null); setProblem('');
    follow.current = true; setAtBottom(true);
  };
  const refresh = useCallback(async (id: string) => {
    const history = await api<{ messages: Message[] }>(`/sessions/${encodeURIComponent(id)}/messages`);
    const result = await api<{ sessions: Session[] }>('/sessions');
    if (selectedRef.current !== id) return;
    setMessages(history.messages); setSessions(result.sessions);
  }, []);
  const complete = useCallback((final: Turn) => {
    if (selectedRef.current !== final.session) return;
    setBusy(false);
    void refresh(final.session).catch(error => setProblem((error as Error).message));
  }, [refresh]);
  const reconnect = async (id: string) => {
    const result = await api<{ turns: Turn[] }>(`/sessions/${encodeURIComponent(id)}/turns`);
    if (selectedRef.current !== id) return;
    const latest = result.turns[0];
    setTurn(latest ?? null);
    setBusy(latest?.status === 'running');
  };
  const submit = async (id: string, message: string | null) => {
    const key = `submission:${id}`;
    const saved = cached(key);
    const pending = saved ? JSON.parse(saved) as { request_id: string; message?: string; resume?: boolean } : null;
    if (pending && (pending.message ?? null) !== message) throw new Error('上次提交的结果尚未确认，请先使用原内容重试。');
    const body = pending ?? { request_id: crypto.randomUUID(), ...(message === null ? { resume: true } : { message }) };
    cached(key, JSON.stringify(body));
    const result = await api<{ turn: Turn }>(`/sessions/${encodeURIComponent(id)}/turns`, 'POST', body);
    cached(key, '');
    setTurn(result.turn); setBusy(result.turn.status === 'running');
  };
  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const result = await api<{ sessions: Session[] }>('/sessions');
        if (!live) return;
        setSessions(result.sessions);
        const saved = cached('selected');
        if (result.sessions.some(item => item.id === saved)) {
          activate(saved);
          await refresh(saved);
          await reconnect(saved);
        }
      } catch (error) { if (live) setProblem((error as Error).message); }
      finally { if (live) setLoading(false); }
    })();
    return () => { live = false; };
  }, []);
  useEffect(() => {
    if (!input.current) return;
    input.current.style.height = 'auto';
    input.current.style.height = `${Math.min(input.current.scrollHeight, 200)}px`;
  }, [draft, selected]);
  useEffect(() => {
    if (follow.current && scroller.current) scroller.current.scrollTop = scroller.current.scrollHeight;
  }, [messages, busy, loading]);
  useEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const observer = new ResizeObserver(() => {
      if (follow.current) element.scrollTop = element.scrollHeight;
    });
    observer.observe(element);
    const content = element.firstElementChild;
    if (content) observer.observe(content);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (!busy && !loading) input.current?.focus();
  }, [busy, loading, selected]);

  const select = async (id: string) => {
    if (busy || loading) return;
    activate(id); setLoading(true);
    try { await refresh(id); await reconnect(id); } catch (error) { setProblem((error as Error).message); }
    finally { setLoading(false); }
  };
  const create = async () => {
    setLoading(true); setProblem('');
    try {
      const { session } = await api<{ session: Session }>('/sessions', 'POST');
      activate(session.id); await refresh(session.id);
    } catch (error) { setProblem((error as Error).message); }
    finally { setLoading(false); }
  };
  const send = async (event: FormEvent) => {
    event.preventDefault();
    if (!draft.trim() || busy || loading) return;
    const prompt = draft.trim();
    let id = selected;
    setBusy(true); setProblem(''); follow.current = true;
    try {
      if (!id) {
        const { session } = await api<{ session: Session }>('/sessions', 'POST');
        id = session.id; activate(id);
        setSessions(previous => [session, ...previous]);
        cached('draft:new', '');
      }
      editDraft('');
      setMessages(previous => [...previous, { role: 'user', text: prompt }]);
      await submit(id, prompt);
      await refresh(id);
    } catch (error) {
      setProblem((error as Error).message);
      // Keep unsent text available if transport failed before the backend saved it.
      editDraft(prompt);
      if (id) await refresh(id).catch(() => undefined);
      setBusy(false);
    }
  };
  const resume = async () => {
    if (!selected || busy) return;
    setBusy(true); setProblem(''); follow.current = true;
    try {
      await submit(selected, null);
      await refresh(selected);
    } catch (error) {
      setProblem((error as Error).message);
      await refresh(selected).catch(() => undefined);
      setBusy(false);
    }
  };
  const stop = async () => {
    if (!selected) return;
    try { await api(`/sessions/${encodeURIComponent(selected)}/stop`, 'POST'); }
    catch (error) { setProblem((error as Error).message); }
  };
  const decide = async (accept: boolean) => {
    const approval = current?.approval;
    if (!selected || !approval || busy) return;
    setBusy(true); setProblem('');
    try {
      await api(`/sessions/${encodeURIComponent(selected)}/${accept ? 'confirm' : 'reject'}`, 'POST', {
        action: approval.action, approval_key: approval.key,
      });
      // A decision does not run anything by itself: it resumes the paused run with the original call.
      await submit(selected, null);
      await refresh(selected);
    } catch (error) {
      setProblem((error as Error).message);
      await refresh(selected).catch(() => undefined);
      setBusy(false);
    }
  };
  const copy = async (message: Message, index: number) => {
    try { await navigator.clipboard.writeText(message.text); setCopied(index); }
    catch { setProblem('无法访问剪贴板，请选中文字复制。'); }
  };
  const current = sessions.find(item => item.id === selected);
  const pendingApproval = current?.approval?.status === 'requested';
  const canSend = !selected || (current && ['active', 'waiting_user'].includes(current.status) && !pendingApproval);
  const heading = current?.title || messages.find(item => item.role === 'user')?.text.slice(0, 60) || '新对话';
  const filtered = sessions.filter(item => `${title(item)} ${item.id}`.toLowerCase().includes(query.toLowerCase()));
  return <main className="pilot-page">
    <aside className="pilot-sessions" aria-label="对话历史">
      <div className="pilot-session-heading"><h2><Anchor size={18} />Pilot</h2>
        <button onClick={() => void create()} disabled={busy || loading} title="新建对话"><Plus size={16} />新对话</button></div>
      <label className="pilot-search"><Search size={15} /><input aria-label="搜索对话" placeholder="搜索对话…" value={query}
        onChange={event => setQuery(event.target.value)} /></label>
      <p className="pilot-sidebar-label">对话历史 <span>{sessions.length}</span></p>
      {!filtered.length && <p className="pilot-muted">{query ? '没有找到匹配的对话' : '你的对话会保存在这里'}</p>}
      {filtered.map(session => <button key={session.id} aria-current={session.id === selected ? 'page' : undefined}
        title={session.id} className={`pilot-session ${session.id === selected ? 'chosen' : ''}`}
        disabled={busy || loading} onClick={() => void select(session.id)}>
        <MessageSquare size={16} /><span><strong>{title(session)}</strong>
          <small>{date(session.updated_at)} · {labels[session.status] ?? session.status}</small></span>
      </button>)}
    </aside>
    <section className="pilot-chat" aria-label="Pilot 对话">
      <header className="pilot-chat-heading">
        <div><strong title={selected || undefined}>{heading}</strong><span>{busy ? '正在处理你的请求' : loading ? '正在加载对话' : current ? labels[current.status] : '和 Anchor 一起开展研究'}</span></div>
        {current?.status === 'interrupted' && <button onClick={() => void resume()} disabled={busy || loading}>继续上次回复</button>}
      </header>
      <div className="pilot-messages" ref={scroller} role="log" aria-label="对话消息" aria-live="polite" onScroll={() => {
        const el = scroller.current;
        if (!el) return;
        const near = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
        follow.current = near; setAtBottom(near);
      }}>
        {!messages.length && !loading && <div className="pilot-welcome">
          <span className="pilot-emblem"><Anchor size={30} /></span>
          <p className="pilot-eyebrow">ANCHOR PILOT</p><h2>{selected ? '我们从哪里开始？' : '从一个问题，开始探索'}</h2>
          <p>梳理研究问题、组织工作流，或一起看看已有的成果。</p>
          <div className="pilot-starters">{starters.map(text => <button key={text} disabled={busy}
            onClick={() => { editDraft(text); input.current?.focus(); }}>{text}<span aria-hidden="true">↗</span></button>)}</div>
          {!selected && <button className="pilot-text-button" disabled={busy} onClick={() => void create()}>新建对话</button>}
        </div>}
        {loading && <div className="pilot-thinking" role="status"><LoaderCircle size={16} />正在加载对话…</div>}
        {messages.map((message, index) => <article key={`${selected}-${index}`} className={`pilot-message ${message.role}`}>
          <div className="pilot-message-author">{message.role === 'assistant' && <Anchor size={16} />}<span>{message.role === 'user' ? '你' : 'Anchor Pilot'}</span></div>
          {message.role === 'assistant' ? <Markdown text={message.text} prefix={`pilot-${selected}-${index}`} /> : <p className="pilot-user-text">{message.text}</p>}
          <button className="pilot-copy" aria-label={`复制${message.role === 'assistant' ? '回复' : '消息'} ${index + 1}`}
            onClick={() => void copy(message, index)}>{copied === index ? <Check size={14} /> : <Copy size={14} />}{copied === index ? '已复制' : '复制'}</button>
        </article>)}
        {turn && <PilotTurn key={turn.id} turn={turn} onComplete={complete} />}
        {busy && !turn && <div className="pilot-thinking" role="status"><LoaderCircle size={16} />正在提交…</div>}
      </div>
      {!atBottom && <button className="pilot-latest" onClick={() => {
        if (scroller.current) scroller.current.scrollTop = scroller.current.scrollHeight;
        follow.current = true; setAtBottom(true);
      }}><ArrowDown size={14} />回到最新消息</button>}
      {current?.status === 'waiting_user' && !pendingApproval && <p className="pilot-waiting" role="status">{current.waiting_reason || 'Pilot 需要你的补充信息，请在下方回答。'}</p>}
      {pendingApproval && current.approval && <div className="pilot-approval" role="region" aria-label="待确认操作">
        <span><Check size={16} />需要你的确认 · {current.approval.target || current.approval.action}</span>
        <details><summary>查看操作内容 · {current.approval.action}</summary><pre>{JSON.stringify(current.approval.proposal, null, 2)}</pre></details>
        <div><button onClick={() => void decide(false)} disabled={busy}>拒绝</button><button onClick={() => void decide(true)} disabled={busy}>确认并继续</button></div>
      </div>}
      {problem && <p className="pilot-error" role="alert">{problem}</p>}
      <form className="pilot-composer" onSubmit={send}>
        <textarea ref={input} rows={2} aria-label="发送给 Anchor Pilot" placeholder={pendingApproval ? '请先确认或拒绝上方操作' : '描述你的问题，或告诉 Pilot 你想完成什么…'} value={draft}
          maxLength={100000} onChange={event => editDraft(event.target.value)} onKeyDown={event => {
            if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing && event.keyCode !== 229) {
              event.preventDefault(); event.currentTarget.form?.requestSubmit();
            }
          }} disabled={busy || loading || !canSend} />
        <div className="pilot-composer-footer"><small>Enter 发送 · Shift + Enter 换行</small>
          {busy ? <button className="pilot-stop" type="button" onClick={() => void stop()}><Square size={14} fill="currentColor" />停止</button> :
            <button className="primary" type="submit" disabled={!draft.trim() || loading || !canSend}><Send size={16} />发送</button>}
        </div>
      </form>
      <small className="pilot-disclaimer">修改工作流、启动或控制运行前会请求确认。研究结论请结合来源核验。</small>
    </section>
  </main>;
}
