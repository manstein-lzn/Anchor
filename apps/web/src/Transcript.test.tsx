/** Pairing a command with what came back from it.
 *
 * The trace stores them as separate messages — an assistant turn carrying the calls, then one result
 * per call — so the pairing is positional, and a view that got it wrong would attribute output to the
 * wrong command. That is the kind of wrongness nobody notices by looking.
 */

import { describe, expect, it } from 'vitest';
import { toEntries } from './Transcript';
import type { TraceMessage } from './model';

const said = (text: string, commands: string[] = []): TraceMessage =>
  ({ role: 'assistant', text, commands });
const result = (text: string, exit_status?: string): TraceMessage =>
  ({ role: 'tool', text, exit_status });

describe('a command and its result', () => {
  it('pairs them in order, one result per call', () => {
    const entries = toEntries([
      said('looking around', ['ls -la', 'cat notes.md']),
      result('total 0'),
      result('# notes\n'),
    ]);

    expect(entries).toEqual([
      { kind: 'said', text: 'looking around' },
      { kind: 'call', command: 'ls -la', output: 'total 0', truncated: false, exit: undefined },
      { kind: 'call', command: 'cat notes.md', output: '# notes\n', truncated: false, exit: undefined },
    ]);
  });

  it('carries the exit status with the command it belongs to', () => {
    // A turn with no words produces no entry of its own, so the call is first.
    const [call] = toEntries([said('', ['anchor-done --summary ok']), result('ok', 'Submitted')]);

    expect(call).toMatchObject({ command: 'anchor-done --summary ok', exit: 'Submitted' });
  });

  it('keeps a result that has no call in front of it', () => {
    // A tail can start in the middle of a turn, and dropping the output would lose the only thing
    // there was to read.
    const entries = toEntries([result('orphaned output')]);

    expect(entries).toEqual([{ kind: 'note', role: '结果', text: 'orphaned output', truncated: false }]);
  });

  it('does not attribute a second result to the first command', () => {
    // One call, two results: the second is not the first command's output a second time.
    const entries = toEntries([said('', ['first']), result('a'), result('b')]);

    expect(entries[0]).toMatchObject({ kind: 'call', command: 'first', output: 'a' });
    expect(entries[1]).toMatchObject({ kind: 'note', text: 'b' });
  });
});

describe('what it said and what it was told', () => {
  it('shows a turn with words as words, and a turn without them as nothing', () => {
    const entries = toEntries([said('here is what I found'), result('x')]);

    expect(entries[0]).toEqual({ kind: 'said', text: 'here is what I found' });
    expect(toEntries([said('', ['ls']), result('x')])[0]).toMatchObject({ kind: 'call' });
  });

  it('keeps the task as a note, because it is read once and is long', () => {
    const entries = toEntries([{ role: 'user', text: '# Task\n\nanswer a question' }]);

    expect(entries).toEqual([
      { kind: 'note', role: '任务', text: '# Task\n\nanswer a question', truncated: false },
    ]);
  });
});
