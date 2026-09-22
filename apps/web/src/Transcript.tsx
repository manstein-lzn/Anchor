/** One node's conversation, as something a person can read.
 *
 * A trace holds three kinds of thing and they want to be read differently. Showing them the same way
 * is what made the previous view a wall of text:
 *
 *   what it said     the point of the view — shown whole, as Markdown
 *   what it ran      one line per command, collapsed — the command is what you scan for
 *   what came back   collapsed to a few lines — output is long and rarely all needed at once
 *
 * Collapsed means `<details>`, which is the browser's own disclosure element: no state, no library, and
 * it opens before anything has hydrated.
 */

import { useMemo } from 'react';
import { CheckCircle2, ChevronRight, Terminal, XCircle } from 'lucide-react';
import { Markdown } from './markdown';
import type { TraceMessage } from './model';

/** How much of one output is shown before it is folded away. */
const LINES_SHOWN = 12;

type Entry =
  | { kind: 'said'; text: string }
  | { kind: 'call'; command: string; output: string; truncated: boolean; exit?: string }
  | { kind: 'note'; role: string; text: string; truncated: boolean };

/** Pair each command with what came back from it.
 *
 * The trace stores them as separate messages — an assistant turn carrying the calls, then one result
 * per call in order — so the pairing is positional. A result with no call in front of it is kept as a
 * note rather than dropped: a slice of the tail can start in the middle of a turn.
 */
export function toEntries(messages: TraceMessage[]): Entry[] {
  const entries: Entry[] = [];
  let pending: string[] = [];
  let at = 0;

  for (const message of messages) {
    if (message.role === 'assistant') {
      if (message.text?.trim()) entries.push({ kind: 'said', text: message.text });
      pending = message.commands?.length ? [...message.commands] : [];
      at = 0;
      continue;
    }
    if (message.role === 'tool') {
      const command = pending[at];
      at += 1;
      if (command !== undefined) {
        entries.push({ kind: 'call', command, output: message.text ?? '',
                       truncated: Boolean(message.truncated), exit: message.exit_status });
      } else {
        entries.push({ kind: 'note', role: '结果', text: message.text ?? '',
                       truncated: Boolean(message.truncated) });
      }
      continue;
    }
    if (message.text?.trim()) {
      entries.push({
        kind: 'note',
        role: message.role === 'system' ? '系统' : message.role === 'user' ? '任务' : message.role,
        text: message.text,
        truncated: Boolean(message.truncated),
      });
    }
  }
  return entries;
}

/** The first line that says something, short enough for a summary row. */
function oneLine(text: string, width = 110): string {
  const line = text.split('\n').find(item => item.trim()) ?? '';
  const flat = line.replace(/\s+/g, ' ').trim();
  return flat.length > width ? `${flat.slice(0, width)}…` : flat;
}

function fold(text: string): { shown: string; hidden: number } {
  const lines = text.split('\n');
  if (lines.length <= LINES_SHOWN) return { shown: text, hidden: 0 };
  return { shown: lines.slice(0, LINES_SHOWN).join('\n'), hidden: lines.length - LINES_SHOWN };
}

function Call({ entry }: { entry: Extract<Entry, { kind: 'call' }> }) {
  const { shown, hidden } = fold(entry.output);
  const failed = Boolean(entry.exit) && !['Submitted', 'succeeded'].includes(entry.exit ?? '');
  return <details className="call">
    <summary>
      <ChevronRight size={13} className="call-caret" />
      <Terminal size={13} />
      {/* The command is the summary, because a command is what a person is scanning for. A summary
          that said only "tool" would make every row identical. */}
      <code>{oneLine(entry.command)}</code>
      {entry.exit && <span className={`call-exit ${failed ? 'bad' : ''}`}>
        {failed ? <XCircle size={12} /> : <CheckCircle2 size={12} />}{entry.exit}
      </span>}
    </summary>
    <pre className="call-command">{entry.command}</pre>
    {entry.output.trim() && <pre className="call-output">{shown}</pre>}
    {hidden > 0 && <p className="folded">
      还有 {hidden} 行{entry.truncated ? '，更长的部分没有传给界面' : ''}
    </p>}
  </details>;
}

export function Transcript({ messages }: { messages: TraceMessage[] }) {
  const entries = useMemo(() => toEntries(messages), [messages]);

  return <div className="transcript">
    {entries.map((entry, index) => {
      if (entry.kind === 'said') {
        return <div className="said" key={index}><Markdown text={entry.text} prefix={`s${index}`} /></div>;
      }
      if (entry.kind === 'call') {
        return <Call entry={entry} key={index} />;
      }
      // The task and the system prompt are long and are read once. Collapsed, not hidden.
      return <details className="note" key={index}>
        <summary><ChevronRight size={13} className="call-caret" />{entry.role}
          <small>{oneLine(entry.text, 60)}</small></summary>
        <Markdown text={entry.text} prefix={`n${index}`} />
      </details>;
    })}
  </div>;
}
