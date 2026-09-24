import { describe, expect, it } from 'vitest';
import { layeredLayout } from './graph';

describe('workflow layout', () => {
  it('lays out feedback and parallel edges without throwing', () => {
    const positions = layeredLayout({
      graph_id: 'test-layout', name: 'test-layout',
      nodes: [
        { id: 'plan', type: 'agent', name: 'plan' },
        { id: 'review', type: 'agent', name: 'review' },
      ],
      edges: [
        { source: 'plan', target: 'review' },
        { source: 'review', target: 'plan' },
        { source: 'plan', target: 'review' },
      ],
    });
    expect(positions.size).toBe(2);
  });
});
