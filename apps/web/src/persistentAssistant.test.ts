import { describe, expect, it } from 'vitest';
import assistant from '../../../examples/graphs/wecom-persistent-assistant.json';
import { asDefinition, toFlowNodes, type OurGraph, type OurRunState } from './model';

const graph: OurGraph = assistant;
const state: OurRunState = {
  objective: graph.objective ?? '', started: '', updated: '', status: 'paused', cursor: null,
  passes: { assistant: 1 }, decided: {}, executed: ['assistant'], skipped: [], error: '',
  nodes: {
    assistant: {
      node_id: 'assistant', pass_number: 1, submission: 'superseded by a newer Turn',
      files: ['interruption.json'], submitted: false, exit_status: 'interrupted',
      interruption: 'superseded by a newer Turn', route: 'reply', commit: 'fs2-control',
    },
  },
};

describe('persistent assistant Graph projections', () => {
  it('uses ordinary Op nodes for host operations and preserves the loop', () => {
    const definition = asDefinition(graph, 'persistent-assistant');
    expect(definition.nodes.map(node => [node.id, node.type])).toEqual([
      ['wait_input', 'op'], ['assistant', 'agent'], ['reply', 'op'],
    ]);
    expect(definition.edges.map(edge => [edge.source, edge.target])).toEqual([
      ['wait_input', 'assistant'], ['assistant', 'reply'], ['reply', 'wait_input'],
    ]);
  });

  it('shows a yielded invocation as interrupted rather than successful or failed', () => {
    const node = toFlowNodes(graph, 'persistent-assistant', state).find(node => node.id === 'assistant');
    expect(node?.data.state).toBe('interrupted');
    expect(node?.data.statusLabel).toBe('已中断');
  });

  it('keeps ordinary successful invocation display unchanged', () => {
    const completed = {
      ...state, nodes: { assistant: { ...state.nodes.assistant, submitted: true, exit_status: '', interruption: null } },
    };
    expect(toFlowNodes(graph, 'persistent-assistant', completed).find(node => node.id === 'assistant')?.data.state).toBe('completed');
  });
});
