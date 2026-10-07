import { useCallback, useEffect, useRef, useState } from 'react';
import { Anchor, LoaderCircle } from 'lucide-react';
import { Markdown } from './markdown';
import { api, bearerKey, requestApiKey, setBearerKey } from './api';
import { PilotQuestion } from './PilotQuestion';
import { mergeQuestions, questionsPath, type Question } from './pilotQuestions';

export type Turn = { id: string; session: string; request_id?: string; status: string; error: string; created_at: string; prompt: string | null };
type Tool = { id: string; name: string; input?: unknown; output?: unknown; status: string; inputSize?: number };
type Chunk = { type: string; id?: string; delta?: string; inputTextDelta?: string; toolCallId?: string; toolName?: string; input?: unknown; output?: unknown; errorText?: string; question?: Question };
const outcomes: Record<string, string> = { completed: '回复完成', failed: '执行失败', stopped: '已停止', interrupted: '执行中断', waiting_approval: '等待你确认操作', waiting_user: '等待你的回答' };

/** Read-only SSE projection. Leaving a page cancels the subscription, never the server's task. */
export function PilotTurn({ turn, saved = false, stopping = false, onComplete, onQuestions }: {
  turn: Turn; saved?: boolean; stopping?: boolean; onComplete: (turn: Turn) => void;
  onQuestions: (session: string, turn: string, pending: boolean) => void;
}) {
  const [text, setText] = useState('');
  const [tools, setTools] = useState<Tool[]>([]);
  const [problem, setProblem] = useState('');
  const [status, setStatus] = useState(turn.status);
  const [connection, setConnection] = useState('连接执行记录…');
  const [phase, setPhase] = useState('等待模型响应');
  const [questions, setQuestions] = useState<Question[]>([]);
  const [questionProblem, setQuestionProblem] = useState('');
  const [questionReload, setQuestionReload] = useState(0);
  const questionsRef = useRef<Question[]>([]);
  const [now, setNow] = useState(Date.now);
  const lastEvent = useRef(Date.now());
  const updateQuestions = useCallback((incoming: Question[]) => {
    const next = mergeQuestions(questionsRef.current, incoming, turn.session, turn.id);
    questionsRef.current = next;
    setQuestions(next);
    onQuestions(turn.session, turn.id, next.some(question => question.status === 'pending'));
  }, [turn.session, turn.id, onQuestions]);
  useEffect(() => { setStatus(turn.status); }, [turn.status]);
  useEffect(() => {
    const controller = new AbortController();
    setQuestionProblem('');
    void api<{ questions: Question[] }>(questionsPath(turn.session, turn.id), 'GET', undefined, controller.signal).then(result => {
      if (!controller.signal.aborted) updateQuestions(result.questions);
    }).catch(error => {
      if (!controller.signal.aborted) setQuestionProblem((error as Error).message);
    });
    return () => controller.abort();
  }, [turn.session, turn.id, questionReload, updateQuestions]);
  useEffect(() => {
    if (status !== 'running') return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [status]);
  useEffect(() => {
    const controller = new AbortController();
    let last = 0;
    let ended = false;
    let resultText = '';
    setText(''); setTools([]); setProblem('');
    setStatus(turn.status); setConnection('连接执行记录…'); setPhase('等待模型响应');
    const consume = (chunk: Chunk) => {
      lastEvent.current = Date.now();
      if (['question', 'question-answered', 'question-interrupted'].includes(chunk.type) && chunk.question) updateQuestions([chunk.question]);
      if (chunk.type === 'start-step') setPhase('等待模型响应');
      if (chunk.type === 'reasoning-start' || chunk.type === 'reasoning-delta') setPhase('模型正在思考');
      if (chunk.type === 'reasoning-end') setPhase('等待模型继续输出');
      if (chunk.type === 'text-start' && resultText) resultText += '\n\n';
      if (chunk.type === 'text-delta') {
        setPhase('正在生成回复');
        resultText += chunk.delta ?? '';
        setText(resultText);
      }
      if (chunk.type === 'tool-input-available' || chunk.type === 'tool-input-start') {
        setTools(previous => {
          const old = previous.find(item => item.id === chunk.toolCallId);
          const value: Tool = { id: chunk.toolCallId!, name: chunk.toolName ?? old?.name ?? '工具',
            status: chunk.type === 'tool-input-start' ? '准备参数' : '执行中', input: chunk.input ?? old?.input,
            inputSize: old?.inputSize };
          return old ? previous.map(item => item.id === value.id ? value : item) : [...previous, value];
        });
      }
      if (chunk.type === 'tool-input-delta') {
        setTools(previous => previous.map(item => item.id === chunk.toolCallId
          ? { ...item, inputSize: (item.inputSize ?? 0) + (chunk.inputTextDelta?.length ?? 0) } : item));
      }
      if (chunk.type.startsWith('tool-output-') || chunk.type === 'tool-input-error') {
        setTools(previous => previous.map(item => item.id === chunk.toolCallId
          ? { ...item, status: chunk.type === 'tool-output-available' ? '已返回' : '未完成', output: chunk.output ?? chunk.errorText } : item));
        setPhase('等待模型继续回复');
      }
      if (chunk.type === 'error') setProblem(chunk.errorText ?? '执行失败');
    };
    const complete = (final: Turn) => {
      ended = true; setStatus(final.status); setConnection('');
      onQuestions(turn.session, turn.id, false);
      if (final.error) setProblem(final.error);
      setTools(previous => previous.map(item => ['执行中', '准备参数'].includes(item.status) ? { ...item, status: '执行已结束，请核查结果' } : item));
      onComplete(final);
    };
    const connect = async () => {
      while (!ended) {
        const attempt = new AbortController();
        let watchdog = window.setTimeout(() => attempt.abort(), 30000);
        let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
        try {
          const headers: Record<string, string> = { Accept: 'text/event-stream' };
          const key = bearerKey();
          if (key) headers.Authorization = `Bearer ${key}`;
          const response = await fetch(`/sessions/${encodeURIComponent(turn.session)}/turns/${turn.id}/events?after=${last}`, {
            headers, signal: AbortSignal.any([controller.signal, attempt.signal]),
          });
          if (ended) return;
          if (response.status === 401) {
            const entered = await requestApiKey();
            if (!entered) { setConnection('需要 API key 才能读取执行记录。'); return; }
            setBearerKey(entered); continue;
          }
          if (!response.ok || !response.body) throw new Error(`HTTP ${response.status}`);
          setConnection('');
          reader = response.body.getReader();
          const decoder = new TextDecoder();
          let buffer = '';
          while (!ended) {
            const { value, done } = await reader.read();
            if (ended) break;
            window.clearTimeout(watchdog);
            watchdog = window.setTimeout(() => attempt.abort(), 30000);
            if (done) {
              buffer += decoder.decode();
              const final = buffer.trim();
              if (final) consumeBlock(final);
              break;
            }
            buffer += decoder.decode(value, { stream: true });
            const blocks = buffer.replace(/\r\n/g, '\n').split('\n\n'); buffer = blocks.pop() ?? '';
            for (const block of blocks) if (consumeBlock(block)) break;
          }
        } catch {
          if (!ended) setConnection('连接断开，正在重新连接；任务仍由服务端处理。');
        } finally {
          window.clearTimeout(watchdog);
          await reader?.cancel().catch(() => undefined);
          attempt.abort();
        }
        // EOF without a terminal event is also a disconnect; avoid a tight retry loop.
        if (!ended) {
          setConnection('连接断开，正在重新连接；任务仍由服务端处理。');
          await new Promise(resolve => setTimeout(resolve, 1000));
        }
      }
    };
    const consumeBlock = (block: string) => {
      const event = block.split(/\r?\n/);
      const id = event.find(line => line.startsWith('id:'))?.slice(3).trim();
      const data = event.filter(line => line.startsWith('data:')).map(line => line.slice(5).trim()).join('\n');
      if (!data) return false;
      const type = event.find(line => line.startsWith('event:'))?.slice(6).trim();
      if (id && Number(id) > last) { const chunk = JSON.parse(data); consume(chunk); last = Number(id); }
      else if (type === 'turn') { complete(JSON.parse(data) as Turn); return true; }
      return false;
    };
    void connect();
    return () => { ended = true; controller.abort(); };
  }, [turn.id, turn.session, onComplete, onQuestions, updateQuestions]);
  const activeTool = tools.find(item => item.status === '执行中') ?? tools.find(item => item.status === '准备参数');
  const activity = activeTool ? `${activeTool.status === '准备参数' ? '正在准备工具参数' : '正在执行工具'}：${activeTool.name}` : phase;
  const elapsed = Math.max(0, Math.floor((now - Date.parse(turn.created_at)) / 1000));
  const quiet = Math.max(0, Math.floor((now - lastEvent.current) / 1000));
  return <section className="pilot-turn" aria-label="本次执行过程">
    {tools.length > 0 && <details className="pilot-tools" open={status === 'running'}>
      <summary>工具活动 · {tools.length} 项</summary>
      {tools.map(tool => <details className="pilot-tool" key={tool.id}>
        <summary><span>{tool.name}</span><small>{tool.status}{tool.status === '准备参数' && !!tool.inputSize && ` · 已生成 ${tool.inputSize} 字符`}</small></summary>
        <pre>{JSON.stringify({ input: tool.input, output: tool.output }, null, 2)}</pre>
      </details>)}
    </details>}
    {!saved && text && <article className="pilot-message assistant">
      <div className="pilot-message-author"><Anchor size={16} />Anchor Pilot{['failed', 'stopped', 'interrupted'].includes(status) && <small> · 未完成的回复</small>}</div>
      <Markdown text={text} prefix={`turn-${turn.id}`} />
    </article>}
    {questions.map(question => <PilotQuestion key={question.id} question={question} enabled={status === 'running' && !stopping} stopping={stopping}
      onAnswered={answered => updateQuestions([answered])} />)}
    {questionProblem && <p className="pilot-error" role="alert">无法加载提问：{questionProblem}
      <button type="button" onClick={() => setQuestionReload(previous => previous + 1)}>重新加载问题</button>
    </p>}
    {status === 'running' && !questions.some(question => question.status === 'pending') && <div className="pilot-thinking" role="status"><LoaderCircle size={16} />
      <span>{activity} · 已用时 {Number.isFinite(elapsed) ? `${Math.floor(elapsed / 60)}分${elapsed % 60}秒` : '未知'}
        {quiet >= 15 && ` · ${quiet} 秒未收到新进展`}</span>
    </div>}
    {connection && <p className="pilot-muted" role="status">{connection}</p>}
    {problem && <p className="pilot-error" role="alert">{problem}</p>}
    {status !== 'running' && <small className="pilot-turn-status">{outcomes[status] ?? status}</small>}
  </section>;
}
