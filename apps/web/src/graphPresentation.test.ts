import { describe, expect, it } from 'vitest';
import { groupGraphTasks } from './graphPresentation';
import type { GraphSummary } from './model';

describe('Graph presentation facts', () => {
  it('collects nested tasks in one level and leaves independent workflows at the root', () => {
    const graphs: GraphSummary[] = [
      { graph: 'grandchild', task_source: 'child', running: null },
      { graph: 'source', running: null },
      { graph: 'shared', running: null },
      { graph: 'child', task_source: 'source', running: null },
    ];
    const result = groupGraphTasks(graphs);
    expect(result.roots.map(item => item.graph)).toEqual(['source', 'shared']);
    expect(result.children.get('source')?.map(item => item.graph)).toEqual(['grandchild', 'child']);
    expect(result.children.size).toBe(1);
    expect(graphs[0].task_source).toBe('child');
  });

  it('keeps dangling, self-referencing and cyclic lineages at root without dropping their descendants', () => {
    const graphs: GraphSummary[] = [
      { graph: 'missing', task_source: 'unknown', running: null },
      { graph: 'missing-child', task_source: 'missing', running: null },
      { graph: 'self', task_source: 'self', running: null },
      { graph: 'cycle-a', task_source: 'cycle-b', running: null },
      { graph: 'cycle-b', task_source: 'cycle-a', running: null },
      { graph: 'cycle-child', task_source: 'cycle-a', running: null },
      { graph: 'empty', task_source: '', running: null },
    ];
    const result = groupGraphTasks(graphs);
    expect(result.roots).toEqual(graphs);
    expect(result.children.size).toBe(0);
  });
});
