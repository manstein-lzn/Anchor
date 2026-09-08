import { describe, expect, it } from 'vitest';
import { fingerprint, parseDocument, project, removeSelection } from './graph';

const fixture = {
  definition: {
    graph_id: 'roundtrip', name: 'Round trip', metadata: { owner: 'test' }, entry_node_id: 'work',
    nodes: [
      { id: 'work', type: 'agent', name: 'Work', agent_ref: 'agent.research', metadata: { skill: 'research' }, input_schema: 'task-v1', retry_policy: 'durable', timeout_seconds: null },
      { id: 'verify', type: 'verifier', name: 'Verify', verifier_ref: 'evidence' },
    ],
    edges: [{ source: 'work', target: 'verify', condition: 'evidence_ready', input_mapping: { evidence: 'work.artifact' } }],
  },
  layout: { positions: { work: { x: 10, y: 20 }, verify: { x: 300, y: 20 } }, viewport: { x: 1, y: 2, zoom: 1 } },
};

describe('Graph IR document', () => {
  it('keeps every semantic and layout field through import and projection', () => {
    const doc = parseDocument(fixture);
    const original = fingerprint(doc);
    const canvas = project(doc, 'node:work', false);
    expect(doc).toEqual(fixture);
    expect(canvas.nodes[0].data.spec.metadata).toEqual({ skill: 'research' });
    expect(canvas.edges[0].label).toBe('条件');
    expect(canvas.edges[0].ariaLabel).toContain('evidence_ready');
    expect(canvas.nodes[0].position).toEqual({ x: 10, y: 20 });
    expect(fingerprint(doc)).toBe(original);
    expect('position' in doc.definition.nodes[0]).toBe(false);
  });
  it('imports bare IR with incomplete agent references without pretending it is valid', () => {
    const value = { graph_id: 'draft', name: '', nodes: [{ id: 'work', name: '', type: 'agent' }] };
    expect(parseDocument(value).definition.edges).toEqual([]);
  });
  it('keeps extensions for server-side schema validation, without executing them', () => {
    const doc = parseDocument({ ...fixture, definition: { ...fixture.definition, future_field: '<script>alert(1)</script>' } });
    expect(doc.definition.future_field).toBe('<script>alert(1)</script>');
  });
  it('removes a node, incident edges, entry and its position only', () => {
    const result = removeSelection(parseDocument(fixture), 'node:work');
    expect(result.definition.nodes.map(node => node.id)).toEqual(['verify']);
    expect(result.definition.edges).toEqual([]);
    expect(result.definition.entry_node_id).toBeNull();
    expect(result.layout.positions).toEqual({ verify: { x: 300, y: 20 } });
    expect(result.layout.viewport).toEqual(fixture.layout.viewport);
    expect(result.definition.metadata).toEqual({ owner: 'test' });
    expect(fixture.definition.nodes).toHaveLength(2);
  });
  it('removes only the selected edge, leaving node configuration untouched', () => {
    const result = removeSelection(parseDocument(fixture), 'edge:0');
    expect(result.definition.edges).toEqual([]);
    expect(result.definition.nodes).toEqual(fixture.definition.nodes);
  });
  it('uses deterministic fallback positions and read-only projection', () => {
    const doc = parseDocument(fixture.definition);
    expect(project(doc, null, true)).toEqual(project(doc, null, true));
    expect(project(doc, null, true).nodes.every(node => !node.draggable)).toBe(true);
  });
  it.each([
    null, [], {}, { ...fixture, layout: [] },
    { ...fixture, layout: { positions: { work: { x: Infinity, y: 0 } } } },
    { ...fixture, definition: { ...fixture.definition, nodes: [fixture.definition.nodes[0], fixture.definition.nodes[0]] } },
    { ...fixture, definition: { ...fixture.definition, nodes: [{ id: 'x', name: 'x', type: 'constructor' }] } },
    { ...fixture, definition: { ...fixture.definition, edges: [null] } },
  ])('rejects non-representable imports: %j', value => {
    expect(() => parseDocument(value)).toThrow();
  });
});

describe('layered auto layout', () => {
  const layered = {
    graph_id: 'layered', name: 'Layered',
    nodes: [
      { id: 'a', type: 'agent' as const, name: 'A' },
      { id: 'b', type: 'agent' as const, name: 'B' },
      { id: 'c', type: 'agent' as const, name: 'C' },
      { id: 'd', type: 'agent' as const, name: 'D' },
    ],
    edges: [{ source: 'a', target: 'b' }, { source: 'a', target: 'c' },
            { source: 'b', target: 'd' }, { source: 'c', target: 'd' }],
  };
  it('places graph nodes without saved positions on distinct layered coordinates', () => {
    const canvas = project(parseDocument(layered), null, true);
    const seen = new Set(canvas.nodes.map(node => `${node.position.x},${node.position.y}`));
    expect(seen.size).toBe(4);
    // a precedes b/c on the x axis; b/c precede d (left-to-right ranks)
    const at = (id: string) => canvas.nodes.find(node => node.id === id)!.position;
    expect(at('a').x).toBeLessThan(at('b').x);
    expect(at('b').x).toBeLessThan(at('d').x);
  });
  it('keeps saved drag positions authoritative and is stable across polls', () => {
    const doc = parseDocument({ definition: layered, layout: { positions: { a: { x: 5, y: 7 } } } });
    expect(project(doc, null, true).nodes.find(node => node.id === 'a')!.position).toEqual({ x: 5, y: 7 });
    expect(project(doc, null, true)).toEqual(project(doc, null, true));
  });
  it('handles a cyclic graph without throwing', () => {
    const cyclic = { ...layered, edges: [...layered.edges, { source: 'd', target: 'a' }] };
    expect(() => project(parseDocument(cyclic), null, true)).not.toThrow();
  });
});
