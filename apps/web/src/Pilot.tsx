import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { Anchor, ArrowDown, Check, Copy, LoaderCircle, MessageSquare, Plus, Search, Send, Square } from 'lucide-react';
import { api, ApiError } from './api';
import { Markdown } from './markdown';
import { PilotTurn, type Turn } from './PilotTurn';
import { SessionActions } from './SessionActions';
import { PilotQuestionHistory } from './PilotQuestionHistory';

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
type Submission = { request_id: string; message?: string; resume?: boolean };
function pendingSubmission(id: string): Submission | null {
  try {
    const value = JSON.parse(cached(`submission:${id}`) || 'null');
    if (value && typeof value.request_id === 'string' && (typeof value.message === 'string' || value.resume === true)) return value;
  } catch { /* Invalid browser cache must not prevent loading the server's history. */ }
  return null;
}

export function Pilot({ session = '', onSession }: { session?: string; onSession?: (id: string) => void }) {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selected, setSelected] = useState('');
  const selectedRef = useRef('');
  const [messages, setMessages] = useState<Message[]>([]);
  const [turn, setTurn] = useState<Turn | null>(null);
  const [turns, setTurns] = useState<Turn[]>([]);
  const turnRef = useRef<Turn | null>(null);
  const [waitingQuestion, setWaitingQuestion] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [syncedTurn, setSyncedTurn] = useState('');
  const [draft, setDraft] = useState(() => cached('draft:new'));
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [problem, setProblem] = useState('');
  const [query, setQuery] = useState('');
  const [copied, setCopied] = useState<number | null>(null);
  const [atBottom, setAtBottom] = useState(true);
  const follow = useRef(true);
  // Read through a ref so `activate` stays a plain function and the effect below can depend on the id.
  const onSessionRef = useRef(onSession);
  onSessionRef.current = onSession;
  const scroller = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const historyRequest = useRef<AbortController | null>(null);
  const submissions = useRef(new Set<string>());
  const navigation = useRef(0);
  const mounted = useRef(true);
  const listVersion = useRef(0);
  const setCurrentTurn = useCallback((value: Turn | null) => { turnRef.current = value; setTurn(value); }, []);
  const questionState = useCallback((id: string, turnId: string, pending: boolean) => {
    if (!mounted.current || selectedRef.current !== id || turnRef.current?.id !== turnId || turnRef.current.status !== 'running') return;
    setWaitingQuestion(pending);
    setSessions(previous => previous.map(item => item.id === id && item.status !== 'archived'
      ? { ...item, status: pending ? 'waiting_user' : 'active' } : item));
  }, []);

  const editDraft = (value: string) => {
    cached(`draft:${selectedRef.current || 'new'}`, value);
    setDraft(value);
  };
  const activate = (id: string) => {
    navigation.current += 1;
    historyRequest.current?.abort();
    selectedRef.current = id;
    setSelected(id); cached('selected', id); onSessionRef.current?.(id);
    setDraft(cached(`draft:${id || 'new'}`));
    setMessages([]); setCurrentTurn(null); setTurns([]); setWaitingQuestion(false); setStopping(false); setSyncedTurn(''); setCopied(null); setProblem('');
    setBusy(submissions.current.has(id));
    follow.current = true; setAtBottom(true);
  };
  const refresh = useCallback(async (id: string) => {
    const version = listVersion.current;
    historyRequest.current?.abort();
    const controller = new AbortController();
    historyRequest.current = controller;
    try {
      const [history, result, execution] = await Promise.all([
        api<{ messages: Message[] }>(`/sessions/${encodeURIComponent(id)}/messages`, 'GET', undefined, controller.signal),
        api<{ sessions: Session[] }>('/sessions', 'GET', undefined, controller.signal),
        api<{ turns: Turn[] }>(`/sessions/${encodeURIComponent(id)}/turns`, 'GET', undefined, controller.signal),
      ]);
      if (!mounted.current || controller.signal.aborted || selectedRef.current !== id) return;
      const latest = execution.turns[0];
      const pending = pendingSubmission(id);
      if (pending && execution.turns.some(item => item.request_id === pending.request_id)) {
        cached(`submission:${id}`, '');
        if (pending.message && cached(`draft:${id}`) === pending.message) {
          cached(`draft:${id}`, ''); setDraft('');
        }
      }
      // The turn is accepted before Harness saves its prompt. Keep that accepted message visible
      // when reopening a session during this window, without duplicating an already saved prompt.
      const last = history.messages.at(-1);
      const prompt = latest?.status === 'running' ? latest.prompt : pendingSubmission(id)?.message;
      setMessages(prompt && !(last?.role === 'user' && last.text === prompt)
        ? [...history.messages, { role: 'user', text: prompt }] : history.messages);
      if (version === listVersion.current) setSessions(result.sessions);
      setCurrentTurn(latest ?? null);
      setTurns(execution.turns);
      if (latest?.status !== 'running') { setWaitingQuestion(false); setStopping(false); }
      setSyncedTurn(latest && ['completed', 'waiting_user', 'waiting_approval'].includes(latest.status) ? latest.id : '');
      setBusy(submissions.current.has(id) || latest?.status === 'running');
    } catch (error) {
      if (!controller.signal.aborted) throw error;
    }
  }, [setCurrentTurn]);
  const complete = useCallback((final: Turn) => {
    if (!mounted.current || selectedRef.current !== final.session) return;
    setCurrentTurn(final); setBusy(false); setWaitingQuestion(false); setStopping(false);
    void refresh(final.session).catch(error => {
      if (mounted.current && selectedRef.current === final.session) setProblem((error as Error).message);
    });
  }, [refresh, setCurrentTurn]);
  const submit = async (id: string, message: string | null) => {
    const key = `submission:${id}`;
    const pending = pendingSubmission(id);
    if (pending && (pending.message ?? null) !== message) throw new Error('上次提交的结果尚未确认，请先使用原内容重试。');
    const body = pending ?? { request_id: crypto.randomUUID(), ...(message === null ? { resume: true } : { message }) };
    cached(key, JSON.stringify(body));
    let result: { turn: Turn };
    try { result = await api<{ turn: Turn }>(`/sessions/${encodeURIComponent(id)}/turns`, 'POST', body); }
    catch (error) {
      // Explicit rejection means no turn was accepted. Only ambiguous delivery keeps its retry ID.
      if (error instanceof ApiError && [400, 401, 403, 404, 409, 422].includes(error.status)) cached(key, '');
      throw error;
    }
    cached(key, '');
    if (mounted.current && selectedRef.current === id) {
      historyRequest.current?.abort();
      setLoading(false); setCurrentTurn(result.turn); setWaitingQuestion(false); setStopping(false); setSyncedTurn('');
      setTurns(previous => [result.turn, ...previous.filter(item => item.id !== result.turn.id)]);
      setBusy(result.turn.status === 'running');
    }
  };
  useEffect(() => {
    let live = true;
    mounted.current = true;
    const initialNavigation = navigation.current;
    let expectedNavigation = initialNavigation;
    const controller = new AbortController();
    void (async () => {
      try {
        const result = await api<{ sessions: Session[] }>('/sessions', 'GET', undefined, controller.signal);
        if (!live || navigation.current !== initialNavigation) return;
        setSessions(result.sessions);
        const saved = session || cached('selected');
        if (result.sessions.some(item => item.id === saved)) {
          activate(saved);
          expectedNavigation = navigation.current;
          await refresh(saved);
        }
      } catch (error) { if (live && navigation.current === expectedNavigation) setProblem((error as Error).message); }
      finally { if (live && navigation.current === expectedNavigation) setLoading(false); }
    })();
    return () => { live = false; mounted.current = false; controller.abort(); historyRequest.current?.abort(); };
  }, []);
  useEffect(() => {
    if (!input.current) return;
    input.current.style.height = 'auto';
    input.current.style.height = `${Math.min(input.current.scrollHeight, 200)}px`;
  }, [draft, selected]);
  useEffect(() => {
    if (follow.current && scroller.current) scroller.current.scrollTop = scroller.current.scrollHeight;
  }, [messages, busy, loading, turn, waitingQuestion]);
  useEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const observer = new ResizeObserver(() => {
      if (follow.current) element.scrollTop = element.scrollHeight;
    });
    observer.observe(element);
    const content = element.querySelector('.pilot-message-list');
    if (content) observer.observe(content);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (!busy && !loading) input.current?.focus();
  }, [busy, loading, selected]);

  const select = async (id: string) => {
    activate(id); setLoading(true);
    const version = navigation.current;
    try { await refresh(id); }
    catch (error) { if (mounted.current && version === navigation.current) setProblem((error as Error).message); }
    finally { if (mounted.current && version === navigation.current) setLoading(false); }
  };
  const create = async () => {
    let version = navigation.current;
    setLoading(true); setProblem('');
    try {
      const { session } = await api<{ session: Session }>('/sessions', 'POST');
      if (!mounted.current || version !== navigation.current) return;
      activate(session.id); version = navigation.current; await refresh(session.id);
    } catch (error) { if (mounted.current && version === navigation.current) setProblem((error as Error).message); }
    finally { if (mounted.current && version === navigation.current) setLoading(false); }
  };
  const send = async (event: FormEvent) => {
    event.preventDefault();
    if (!draft.trim() || busy || loading || !canSend) return;
    historyRequest.current?.abort();
    const prompt = draft.trim();
    let id = selected;
    const version = navigation.current;
    setBusy(true); setProblem(''); follow.current = true;
    setCurrentTurn(null); setWaitingQuestion(false); setSyncedTurn('');
    editDraft('');
    setMessages(previous => pendingSubmission(id)?.message === prompt && previous.at(-1)?.role === 'user'
      && previous.at(-1)?.text === prompt ? previous : [...previous, { role: 'user', text: prompt }]);
    submissions.current.add(id);
    try {
      if (!id) {
        const { session } = await api<{ session: Session }>('/sessions', 'POST');
        submissions.current.delete('');
        id = session.id; submissions.current.add(id);
        if (mounted.current) setSessions(previous => [session, ...previous]);
        if (mounted.current && version === navigation.current) {
          activate(id);
          setMessages([{ role: 'user', text: prompt }]);
        }
        cached('draft:new', '');
      }
      if (mounted.current) setSessions(previous => previous.map(item => item.id === id && !item.title
        ? { ...item, title: prompt.slice(0, 60) } : item));
      await submit(id, prompt);
    } catch (error) {
      // Keep unsent text available if transport failed before the backend saved it.
      cached(`draft:${id || 'new'}`, prompt);
      if (mounted.current && selectedRef.current === id) {
        setProblem((error as Error).message); setDraft(prompt); setBusy(false);
      }
    } finally {
      submissions.current.delete(id);
    }
  };
  const resume = async () => {
    if (!selected || busy) return;
    historyRequest.current?.abort();
    setBusy(true); setProblem(''); follow.current = true;
    const id = selected;
    submissions.current.add(id);
    try {
      await submit(id, null);
    } catch (error) {
      if (mounted.current && selectedRef.current === id) { setProblem((error as Error).message); setBusy(false); }
    } finally { submissions.current.delete(id); }
  };
  const stop = async () => {
    if (!selected || stopping) return;
    const id = selected;
    const version = navigation.current;
    setStopping(true);
    try { await api(`/sessions/${encodeURIComponent(id)}/stop`, 'POST'); }
    catch (error) {
      if (mounted.current && version === navigation.current) { setProblem((error as Error).message); setStopping(false); }
    }
  };
  const decide = async (accept: boolean) => {
    const approval = current?.approval;
    if (!selected || !approval || busy) return;
    historyRequest.current?.abort();
    setBusy(true); setProblem('');
    const id = selected;
    submissions.current.add(id);
    try {
      await api(`/sessions/${encodeURIComponent(selected)}/${accept ? 'confirm' : 'reject'}`, 'POST', {
        action: approval.action, approval_key: approval.key,
      });
      // A decision does not run anything by itself: it resumes the paused run with the original call.
      await submit(id, null);
    } catch (error) {
      if (mounted.current && selectedRef.current === id) { setProblem((error as Error).message); setBusy(false); }
    } finally { submissions.current.delete(id); }
  };
  const copy = async (message: Message, index: number) => {
    try { await navigator.clipboard.writeText(message.text); setCopied(index); }
    catch { setProblem('无法访问剪贴板，请选中文字复制。'); }
  };
  const renameSession = async (id: string, title: string) => {
    const result = await api<{ session: Session }>(`/sessions/${encodeURIComponent(id)}`, 'PUT', { title });
    listVersion.current += 1;
    if (mounted.current) setSessions(items => items.map(item => item.id === id ? { ...item, title: result.session.title } : item));
  };
  const deleteSession = async (id: string) => {
    await api(`/sessions/${encodeURIComponent(id)}`, 'DELETE');
    listVersion.current += 1;
    cached(`draft:${id}`, ''); cached(`submission:${id}`, '');
    if (!mounted.current) return;
    setSessions(items => items.filter(item => item.id !== id));
    if (selectedRef.current === id) { activate(''); setLoading(false); }
  };
  const current = sessions.find(item => item.id === selected);
  const pendingApproval = current?.approval?.status === 'requested';
  // An interrupted session accepts a new message: the next turn carries what the dead run recorded.
  const canSend = !selected || (current &&
    ['active', 'interrupted'].includes(current.status) && !pendingApproval && !waitingQuestion);
  const heading = current?.title || messages.find(item => item.role === 'user')?.text.slice(0, 60) || '新对话';
  const filtered = sessions.filter(item => `${title(item)} ${item.id}`.toLowerCase().includes(query.toLowerCase()));
  return <main className="pilot-page">
    <aside className="pilot-sessions" aria-label="对话历史">
      <div className="pilot-session-heading"><h2><Anchor size={18} />Pilot</h2>
        <button onClick={() => void create()} disabled={loading} title="新建对话"><Plus size={16} />新对话</button></div>
      <label className="pilot-search"><Search size={15} /><input aria-label="搜索对话" placeholder="搜索对话…" value={query}
        onChange={event => setQuery(event.target.value)} /></label>
      <p className="pilot-sidebar-label">对话历史 <span>{sessions.length}</span></p>
      {!filtered.length && <p className="pilot-muted">{query ? '没有找到匹配的对话' : '你的对话会保存在这里'}</p>}
      {filtered.map(session => <div key={session.id} className={`pilot-session-row ${session.id === selected ? 'chosen' : ''}`}><button aria-current={session.id === selected ? 'page' : undefined}
        title={session.id} className={`pilot-session ${session.id === selected ? 'chosen' : ''}`}
        onClick={() => void select(session.id)}>
        <MessageSquare size={16} /><span><strong>{title(session)}</strong>
          <small>{date(session.updated_at)} · {labels[session.status] ?? session.status}</small></span>
      </button><SessionActions title={title(session)} busy={(session.id === selected && busy) || submissions.current.has(session.id)}
        onRename={name => renameSession(session.id, name)} onDelete={() => deleteSession(session.id)} /></div>)}
    </aside>
    <section className="pilot-chat" aria-label="Pilot 对话">
      <header className="pilot-chat-heading">
        <div><strong title={selected || undefined}>{heading}</strong><span>{stopping ? '正在停止执行' : waitingQuestion ? '等待你的答复' : busy ? '正在处理你的请求' : loading ? '正在加载对话' : current ? labels[current.status] : '和 Anchor 一起开展研究'}</span></div>
        {current?.status === 'interrupted' && <button onClick={() => void resume()} disabled={busy || loading}>继续上次回复</button>}
      </header>
      <div className="pilot-messages" ref={scroller} role="log" aria-label="对话消息" aria-live="polite" onScroll={() => {
        const el = scroller.current;
        if (!el) return;
        const near = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
        follow.current = near; setAtBottom(near);
      }}>
        <div className="pilot-message-list">
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
        {turns.some(item => item.id !== turn?.id) && <PilotQuestionHistory key={selected} session={selected}
          turns={turns.filter(item => item.id !== turn?.id).map(item => item.id).reverse()} />}
        {turn && <PilotTurn key={`${turn.session}-${turn.id}`} turn={turn} saved={syncedTurn === turn.id} stopping={stopping} onComplete={complete} onQuestions={questionState} />}
        {busy && !turn && <div className="pilot-thinking" role="status"><LoaderCircle size={16} />正在提交…</div>}
        </div>
      </div>
      {!atBottom && <button className="pilot-latest" onClick={() => {
        if (scroller.current) scroller.current.scrollTop = scroller.current.scrollHeight;
        follow.current = true; setAtBottom(true);
      }}><ArrowDown size={14} />回到最新消息</button>}
      {current?.status === 'waiting_user' && !pendingApproval && <p className="pilot-waiting" role="status">{current.waiting_reason || 'Pilot 需要你的补充信息，请回答本次执行中的问题。'}</p>}
      {pendingApproval && current.approval && <div className="pilot-approval" role="region" aria-label="待确认操作">
        <span><Check size={16} />需要你的确认 · {current.approval.target || current.approval.action}</span>
        <details><summary>查看操作内容 · {current.approval.action}</summary><pre>{JSON.stringify(current.approval.proposal, null, 2)}</pre></details>
        <div><button onClick={() => void decide(false)} disabled={busy}>拒绝</button><button onClick={() => void decide(true)} disabled={busy}>确认并继续</button></div>
      </div>}
      {problem && <p className="pilot-error" role="alert">{problem}
        {selected && <button onClick={() => void select(selected)}>重新加载对话</button>}
      </p>}
      <form className="pilot-composer" onSubmit={send}>
        <textarea ref={input} rows={2} aria-label="发送给 Anchor Pilot" placeholder={pendingApproval ? '请先确认或拒绝上方操作' : waitingQuestion || current?.status === 'waiting_user' ? '请先回答、拒绝或取消上方问题' : '描述你的问题，或告诉 Pilot 你想完成什么…'} value={draft}
          maxLength={100000} onChange={event => editDraft(event.target.value)} onKeyDown={event => {
            if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing && event.keyCode !== 229) {
              event.preventDefault(); event.currentTarget.form?.requestSubmit();
            }
          }} disabled={busy || loading || !canSend} />
        <div className="pilot-composer-footer"><small>Enter 发送 · Shift + Enter 换行</small>
          {busy ? <button className="pilot-stop" type="button" disabled={stopping} onClick={() => void stop()}><Square size={14} fill="currentColor" />{stopping ? '正在停止…' : '停止'}</button> :
            <button className="primary" type="submit" disabled={!draft.trim() || loading || !canSend}><Send size={16} />发送</button>}
        </div>
      </form>
      <small className="pilot-disclaimer">删除工作流会再次确认；研究结论请结合来源核验。</small>
    </section>
  </main>;
}
