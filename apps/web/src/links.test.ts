import { describe, expect, it } from 'vitest';
import { anchorHref, anchorRef, anchorTarget } from './links';

describe('anchor references', () => {
  it('reads the three kinds the Pilot writes', () => {
    expect(anchorRef('#anchor/graph/alpha')).toEqual({ kind: 'graph', graph: 'alpha' });
    expect(anchorRef('#anchor/run/20260926T104613')).toEqual({ kind: 'run', run: '20260926T104613' });
    expect(anchorRef('#anchor/artifact/20260926T104613/write/out/result.txt')).toEqual(
      { kind: 'artifact', run: '20260926T104613', node: 'write', path: 'out/result.txt' });
  });

  it('leaves ordinary links and malformed references alone', () => {
    for (const href of [undefined, null, '', 'https://example.com', '#user-content-x', '#anchor',
                        '#anchor/graph', '#anchor/graph/a/b', '#anchor/artifact/run/node', '#anchor/other/x']) {
      expect(anchorRef(href)).toBeNull();
    }
  });

  it('round-trips names that need escaping', () => {
    for (const ref of [{ kind: 'graph' as const, graph: '研究 demo' },
                       { kind: 'run' as const, run: 'a b/c' },
                       { kind: 'artifact' as const, run: 'r 1', node: '写 作', path: 'dir/结 果.txt' }] as const) {
      expect(anchorRef(anchorHref(ref))).toEqual(ref);
    }
  });

  it('opens the page that already shows the object', () => {
    expect(anchorTarget({ kind: 'graph', graph: 'alpha' }, 'other')).toEqual(
      { view: 'graph', graph: 'alpha', run: '', node: '' });
    expect(anchorTarget({ kind: 'run', run: 'r1' }, 'alpha')).toEqual(
      { view: 'runs', graph: 'alpha', run: 'r1', node: '', path: '' });
    expect(anchorTarget({ kind: 'artifact', run: 'r1', node: 'write', path: 'a.txt' }, 'alpha')).toEqual(
      { view: 'runs', graph: 'alpha', run: 'r1', node: 'write', path: 'a.txt' });
  });
});
