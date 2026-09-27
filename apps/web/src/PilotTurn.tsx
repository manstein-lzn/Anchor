import { useEffect, useState } from 'react';
import { Anchor, LoaderCircle } from 'lucide-react';
import { Markdown } from './markdown';
import { bearerKey, setBearerKey } from './api';

export type Turn = { id: string; session: string; status: string; error: string; created_at: string; prompt: string | null };
type Tool = { id: string; name: string; input?: unknown; output?: unknown; status: string };
type Chunk = { type: string; id?: string; delta?: string; toolCallId?: string; toolName?: string; input?: unknown; output?: unknown; errorText?: string };
const outcomes: Record<string, string> = { completed: '回复完成', failed: '执行失败', stopped: '已停止', interrupted: '执行中断', waiting_approval: '等待你确认操作', waiting_user: '等待你的回答' };

/** Read-only delivery projection. Native EventSource reconnects with Last-Event-ID. */
export function PilotTurn({ turn, onComplete }: { turn: Turn; onComplete: (turn: Turn) => void }) {
  const [text, setText] = useState('');
  const [tools, setTools] = useState<Tool[]>([]);
  const [problem, setProblem] = useState('');
  const [status, setStatus] = useState(turn.status);
  const [connection, setConnection] = useState('连接执行记录…');
  useEffect(() => {
    let last = 0;
    let ended = false;
    let resultText = '';
    const consume = (chunk: Chunk) => {
      if (chunk.type === 'text-start' && resultText) resultText += '\n\n';
      if (chunk.type === 'text-delta') {
        resultText += chunk.delta ?? '';
        setText(resultText);
      }
      if (chunk.type === 'tool-input-available' || chunk.type === 'tool-input-start') {
        setTools(previous => {
          const old = previous.find(item => item.id === chunk.toolCallId);
          const value: Tool = { id: chunk.toolCallId!, name: chunk.toolName ?? old?.name ?? '工具',
            status: '执行中', input: chunk.input ?? old?.input };
          return old ? previous.map(item => item.id === value.id ? value : item) : [...previous, value];
        });
      }
      if (chunk.type.startsWith('tool-output-') || chunk.type === 'tool-input-error') {
        setTools(previous => previous.map(item => item.id === chunk.toolCallId
          ? { ...item, status: chunk.type === 'tool-output-available' ? '已返回' : '未完成', output: chunk.output ?? chunk.errorText } : item));
      }
      if (chunk.type === 'error') setProblem(chunk.errorText ?? '执行失败');
    };
    const complete = (final: Turn) => {
      ended = true; setStatus(final.status); setConnection('');
      if (final.error) setProblem(final.error);
      setTools(previous => previous.map(item => item.status === '执行中' ? { ...item, status: '执行已结束，请核查结果' } : item));
      onComplete(final);
    };
    const connect = async () => {
      while (!ended) {
        try {
          const headers: Record<string, string> = { Accept: 'text/event-stream' };
          const key = bearerKey();
          if (key) headers.Authorization = `Bearer ${key}`;
          const response = await fetch(`/sessions/${encodeURIComponent(turn.session)}/turns/${turn.id}/events?after=${last}`, { headers });
          if (response.status === 401) {
            const entered = window.prompt('请输入 Anchor API key') ?? '';
            if (!entered) { setConnection('需要 API key 才能读取执行记录。'); return; }
            setBearerKey(entered); continue;
          }
          if (!response.ok || !response.body) throw new Error(`HTTP ${response.status}`);
          setConnection('');
          const reader = response.body.getReader();
          const decoder = new TextDecoder();
          let buffer = '';
          while (!ended) {
            const { value, done } = await reader.read();
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
          if (!ended) { setConnection('连接断开，正在重新连接；任务仍由服务端处理。'); await new Promise(resolve => setTimeout(resolve, 1000)); }
        }
      }
    };
    const consumeBlock = (block: string) => {
      const event = block.split(/\r?\n/);
      const id = event.find(line => line.startsWith('id:'))?.slice(3).trim();
      const data = event.filter(line => line.startsWith('data:')).map(line => line.slice(5).trim()).join('\n');
      if (!data) return false;
      const type = event.find(line => line.startsWith('event:'))?.slice(6).trim();
      if (id && Number(id) > last) { last = Number(id); consume(JSON.parse(data)); }
      else if (type === 'turn') { complete(JSON.parse(data) as Turn); return true; }
      return false;
    };
    void connect();
    return () => { ended = true; };
  }, [turn.id, turn.session, onComplete]);
  return <section className="pilot-turn" aria-label="本次执行过程">
    {tools.length > 0 && <details className="pilot-tools" open={status === 'running'}>
      <summary>工具活动 · {tools.length} 项</summary>
      {tools.map(tool => <details className="pilot-tool" key={tool.id}>
        <summary><span>{tool.name}</span><small>{tool.status}</small></summary>
        <pre>{JSON.stringify({ input: tool.input, output: tool.output }, null, 2)}</pre>
      </details>)}
    </details>}
    {status !== 'completed' && text && <article className="pilot-message assistant">
      <div className="pilot-message-author"><Anchor size={16} />Anchor Pilot{status !== 'running' && <small> · 未完成的回复</small>}</div>
      <Markdown text={text} prefix={`turn-${turn.id}`} />
    </article>}
    {status === 'running' && <div className="pilot-thinking" role="status"><LoaderCircle size={16} />{text ? '正在继续回复…' : '正在处理你的请求…'}</div>}
    {connection && <p className="pilot-muted" role="status">{connection}</p>}
    {problem && <p className="pilot-error" role="alert">{problem}</p>}
    {status !== 'running' && <small className="pilot-turn-status">{outcomes[status] ?? status}</small>}
  </section>;
}
