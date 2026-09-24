import { describe, expect, it } from 'vitest';
import research from '../../../examples/graphs/deep-academic-research.json';
import { asDefinition, toFlowEdges, type OurRunState } from './model';
import { feedbackEdges, layoutWorkflow, NODE_WIDTH, NODE_HEIGHT, type Definition, type Layout } from './graph';

function checkRoutes(layout: Layout, definition: Definition) {
  const segments: { edge: number; x1: number; y1: number; x2: number; y2: number }[] = [];
  layout.routes.forEach((route, index) => {
    expect(route.points.length).toBeGreaterThanOrEqual(2);
    const edge = definition.edges[index];
    for (const [node, portId, point] of [
      [edge.source, `e${index}-out`, route.points[0]],
      [edge.target, `e${index}-in`, route.points.at(-1)!],
    ] as const) {
      const port = layout.ports[node].find(p => p.id === portId)!;
      expect(point.x).toBeCloseTo(layout.positions[node].x + port.x);
      expect(point.y).toBeCloseTo(layout.positions[node].y + port.y);
    }
    for (let i = 1; i < route.points.length; i++) {
      const a = route.points[i - 1], b = route.points[i];
      expect(a.x === b.x || a.y === b.y).toBe(true);
      for (const point of [a, b]) {
        expect(point.x).toBeGreaterThanOrEqual(layout.bounds.x);
        expect(point.x).toBeLessThanOrEqual(layout.bounds.x + layout.bounds.width);
        expect(point.y).toBeGreaterThanOrEqual(layout.bounds.y);
        expect(point.y).toBeLessThanOrEqual(layout.bounds.y + layout.bounds.height);
      }
      const x1 = Math.min(a.x, b.x), x2 = Math.max(a.x, b.x), y1 = Math.min(a.y, b.y), y2 = Math.max(a.y, b.y);
      for (const p of Object.values(layout.positions)) {
        const crosses = a.x === b.x
          ? a.x > p.x + .01 && a.x < p.x + NODE_WIDTH - .01 && y2 > p.y + .01 && y1 < p.y + NODE_HEIGHT - .01
          : a.y > p.y + .01 && a.y < p.y + NODE_HEIGHT - .01 && x2 > p.x + .01 && x1 < p.x + NODE_WIDTH - .01;
        expect(crosses, `edge ${index} crosses a node`).toBe(false);
      }
      segments.push({ edge: index, x1, x2, y1, y2 });
    }
  });
  for (const a of segments) for (const b of segments) {
    if (a.edge >= b.edge) continue;
    const horizontal = a.y1 === a.y2 && b.y1 === b.y2 && Math.abs(a.y1 - b.y1) < .01;
    const vertical = a.x1 === a.x2 && b.x1 === b.x2 && Math.abs(a.x1 - b.x1) < .01;
    const overlap = horizontal ? Math.min(a.x2, b.x2) - Math.max(a.x1, b.x1)
      : vertical ? Math.min(a.y2, b.y2) - Math.max(a.y1, b.y1) : 0;
    expect(overlap, `edges ${a.edge} and ${b.edge} overlap`).toBeLessThanOrEqual(.01);
  }
}

describe('shared workflow geometry', () => {
  const definition = asDefinition(research, 'research');
  it('routes all 12 research edges without crossing nodes or sharing segments', async () => {
    expect([...feedbackEdges(definition)]).toEqual([3, 4, 8, 9, 10]);
    const layout = await layoutWorkflow(definition);
    expect(layout.routes).toHaveLength(12);
    for (const [index, edge] of definition.edges.entries()) {
      if (!layout.routes[index].feedback) expect(layout.positions[edge.target].y).toBeGreaterThan(layout.positions[edge.source].y);
    }
    checkRoutes(layout, definition);
    for (const ports of Object.values(layout.ports)) {
      expect(new Set(ports.map(p => `${p.x},${p.y}`)).size).toBe(ports.length);
    }
  });
  it('keeps saved coordinates and reroutes around dragged nodes', async () => {
    const auto = await layoutWorkflow(definition);
    const positions = { ...auto.positions, investigate: { ...auto.positions.investigate, x: auto.positions.investigate.x - 280 } };
    const manual = await layoutWorkflow(definition, positions);
    expect(manual.positions).toEqual(positions);
    checkRoutes(manual, definition);
    expect((await layoutWorkflow(definition)).positions).toEqual(auto.positions);
  });
  it('handles empty graphs, parallel edges and a self-loop', async () => {
    expect((await layoutWorkflow({ ...definition, nodes: [], edges: [] })).routes).toEqual([]);
    const small: Definition = { ...definition, nodes: definition.nodes.slice(0, 2), edges: [
      { source: 'frame', target: 'investigate' }, { source: 'frame', target: 'investigate' },
      { source: 'investigate', target: 'investigate' },
    ] };
    checkRoutes(await layoutWorkflow(small), small);
  });
  it('keeps a previously traversed feedback edge solid after a later decision', () => {
    const state = { executed: ['frame', 'investigate', 'challenge', 'feedback', 'frame'],
      decided: { 'feedback|frame': [false, 2] }, cursor: null } as unknown as OurRunState;
    expect(toFlowEdges(research, state)[3].style?.strokeDasharray).toBeUndefined();
    expect(toFlowEdges(research, state)[4].style?.strokeDasharray).toBeDefined();
  });
});
