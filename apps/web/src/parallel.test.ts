import { describe, expect, it } from 'vitest';
import { activeNodes, addParallelRegion, pairedNodes, renameGraphNode, tracePassKeys } from './parallel';
import { toFlowEdges, toFlowNodes, type OurGraph, type OurRunState } from './model';

const empty: OurGraph = { entry: '', nodes: [], edges: [] };
const region = addParallelRegion(empty).graph;
const state: OurRunState = { objective: '', started: '', updated: '', status: 'running', cursor: null,
  active: { 'branch-a': { node: 'branch-a', pass: 2, run: 2, dir: 'a' }, 'branch-b': { node: 'branch-b', pass: 2, run: 2, dir: 'b' } },
  parallel: { fanout: 'fanout', join: 'join', invocation: 2 }, passes: { 'branch-a': 2, 'branch-b': 2 },
  decided: { 'fanout|branch-a': [true, 2], 'fanout|branch-b': [true, 2] }, nodes: {}, executed: ['fanout'], skipped: [], error: '' };

describe('same-run parallel authoring and observation', () => {
  it('separates full node identities and invocation numbers in persisted trace keys', () => {
    const traces = { '["a",2]': [], '["a-2",1]': [], '["a",1]': [], '["a/b",1]': [], '["a__b",1]': [] };
    expect(tracePassKeys(traces, 'a')).toEqual(['["a",1]', '["a",2]']);
    expect(tracePassKeys(traces, 'a-2')).toEqual(['["a-2",1]']);
    expect(tracePassKeys(traces, 'a/b')).toEqual(['["a/b",1]']);
    expect(tracePassKeys({ a: [], 'a-2': [] }, 'a')).toEqual(['a', 'a-2']);
  });
  it('creates complete regions with unique identities while retaining the old graph and role', () => {
    const existing: OurGraph = { entry: 'start', nodes: [{ id: 'start', agent: 'writer' }, { id: 'fanout', op: 'reserved' }],
      edges: [{ from: 'start', to: 'fanout' }], agents: { writer: { model: 'configured-model' } }, ops: { reserved: { run: 'true' }, join: { run: 'true' } } };
    const first = addParallelRegion(existing);
    const second = addParallelRegion(first.graph);
    expect(first.fanout).toBe('fanout2');
    expect(first.graph.ops?.fanout2.fanout?.join).toBe('join2');
    expect(first.graph.edges).toContainEqual({ from: 'join2', to: 'start' });
    expect(first.graph.agents).toEqual(existing.agents);
    expect(first.graph.nodes.filter(node => node.id.startsWith('branch-')).every(node => node.agent === 'writer')).toBe(true);
    expect(new Set(second.graph.nodes.map(node => node.id)).size).toBe(second.graph.nodes.length);
    expect(second.graph.edges).toContainEqual({ from: second.graph.ops![second.fanout].fanout!.join, to: first.fanout });
    expect(existing.nodes).toHaveLength(2);
  });

  it('creates a downstream agent for an empty graph and keeps pairing intact when renamed', () => {
    expect(region.edges).toContainEqual({ from: 'join', to: 'synthesize' });
    const renamed = renameGraphNode(renameGraphNode(region, 'join', 'collect'), 'fanout', 'split');
    expect(renamed.entry).toBe('split');
    expect(pairedNodes(renamed, 'split')).toEqual(['collect']);
    expect(pairedNodes(renamed, 'collect')).toEqual(['split']);
    expect(renamed.edges).toContainEqual({ from: 'branch-a', to: 'collect' });
    expect(renamed.ops?.join).toEqual({ join: {} });
  });

  it('shows both active branches, animates both selected inputs, and leaves join pending', () => {
    const nodes = toFlowNodes(region, 'parallel', state);
    expect(nodes.filter(node => node.data.state === 'running').map(node => node.id)).toEqual(['branch-a', 'branch-b']);
    expect(nodes.find(node => node.id === 'join')?.data).toMatchObject({ state: 'pending', control: 'join', kind: '展开自 fanout' });
    expect(toFlowEdges(region, state).filter(edge => edge.animated).map(edge => edge.target)).toEqual(['branch-a', 'branch-b']);
    expect(toFlowNodes(region, 'parallel', { ...state, active: {} }).filter(node => node.data.state === 'running')).toEqual([]);
  });

  it('preserves legacy cursor runs and de-duplicates an active cursor', () => {
    const cursor = { node: 'branch-a', pass: 2, dir: 'a' };
    expect(activeNodes({ ...state, cursor })).toHaveLength(2);
    expect(activeNodes({ ...state, active: undefined, cursor })).toEqual([cursor]);
    expect(activeNodes(null)).toEqual([]);
  });

  it('projects native Rust branch cursors while leaving completed branches and join inactive', () => {
    const native: OurRunState = { ...state, active: undefined, parallel: {
      fanout_node: 'fanout', join_node: 'join', branches: [
        { status: 'running', cursor: { node_id: 'branch-a', key: { invocation: 2 } } },
        { status: 'running', cursor: { node_id: 'branch-b', key: { invocation: 2 } } },
        { status: 'completed', cursor: null },
      ],
    } };
    expect(activeNodes(native).map(cursor => cursor.node)).toEqual(['branch-a', 'branch-b']);
    expect(toFlowNodes(region, 'parallel', native).filter(node => node.data.state === 'running').map(node => node.id))
      .toEqual(['branch-a', 'branch-b']);
    expect(toFlowEdges(region, native).filter(edge => edge.animated).map(edge => edge.target)).toEqual(['branch-a', 'branch-b']);
    expect(activeNodes({ ...native, parallel: null })).toEqual([]);
  });
});
