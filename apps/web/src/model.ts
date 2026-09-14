/** The shape our backend has, and the shape the canvas wants.
 *
 * The canvas came from the previous system and its data model is deliberately generic — a node with
 * a name, a state and an attempt number, and an edge that is selected or not. What differs is where
 * those come from: a run's `decided` map instead of a table of edge decisions, and each node's
 * submission instead of a node_run row. This file is that translation and nothing else.
 */

import type { Edge, Node, XYPosition } from '@xyflow/react';
import { layeredLayout, type Definition } from './graph';
import { label } from './execution';

export type OurGraph = {
  entry: string;
  objective?: string;
  max_rounds?: number;
  agents: Record<string, { model: string; instructions?: string; network?: boolean }>;
  nodes: { id: string; agent: string }[];
  edges: { from: string; to: string }[];
};

export type OurNodeResult = {
  node_id: string;
  pass_number: number;
  submission: string;
  files: string[];
  submitted: boolean;
  exit_status: string;
  route: string | null;
};

export type OurRunState = {
  objective: string;
  started: string;
  status: string;
  updated: string;
  cursor: { node: string; pass: number; dir: string } | null;
  passes: Record<string, number>;
  decided: Record<string, [boolean, number]>;
  nodes: Record<string, OurNodeResult>;
  executed: string[];
  skipped: string[];
  error: string;
};

export type OurRun = {
  run: string;
  graph: string;
  status: string;
  running: boolean;
  started: string;
  updated: string;
  executed: string[];
  objective: string;
};

export type TraceMessage = { role: string; text: string; tools?: string[]; exit_status?: string };

export type OurRunDetail = {
  graph: string;
  run: string;
  state: OurRunState;
  traces: Record<string, TraceMessage[]>;
  nodes: string[];
};

export type FlowNode = Node<{
  name: string; kind: string; state: string; detail: string;
  attempt?: number; statusLabel?: string;
}, 'execution'>;

/** Our graph.json in the shape `layeredLayout` expects. */
export function asDefinition(graph: OurGraph, name: string): Definition {
  return {
    graph_id: name,
    name,
    nodes: graph.nodes.map(node => ({ id: node.id, type: 'agent' as const, name: node.id })),
    edges: graph.edges.map(edge => ({ source: edge.from, target: edge.to })),
    entry_node_id: graph.entry,
  };
}

/** One node's state as of the most recent run, or as it was left by an earlier one. */
function nodeState(nodeId: string, graph: OurGraph, state: OurRunState | null): FlowNode {
  const definition = asDefinition(graph, nodeId);
  const node = definition.nodes.find(item => item.id === nodeId)!;
  const result = state?.nodes[nodeId];
  const passes = state?.passes[nodeId] ?? 0;
  const running = state?.cursor?.node === nodeId;
  const skipped = state?.skipped?.includes(nodeId) ?? false;

  const status = running ? 'running'
    : result?.submitted ? 'completed'
    : result ? 'failed'
    : skipped ? 'skipped'
    : 'pending';

  const statusLabel = result ? label(result.exit_status || status) : undefined;
  const detail = result
    ? (result.submission || '').split('\n').find(line => line.trim()) ?? ''
    : skipped ? '没有被选中' : '尚未执行';

  return {
    id: nodeId,
    type: 'execution',
    position: { x: 0, y: 0 },
    data: {
      name: node.name,
      kind: graph.nodes.find(item => item.id === nodeId)?.agent ?? 'agent',
      state: status,
      detail,
      // The canvas renders `attempt + 1` as "第 N 次执行", so zero means the first pass.
      attempt: passes ? passes - 1 : undefined,
      statusLabel,
    },
  };
}

export function toFlowNodes(graph: OurGraph, name: string,
                            state: OurRunState | null): FlowNode[] {
  const spec = asDefinition(graph, name);
  const positions = layeredLayout(spec);
  return spec.nodes.map(node => ({
    ...nodeState(node.id, graph, state),
    position: positions.get(node.id) ?? { x: 0, y: 0 },
  }));
}

/** An edge carries what the run decided about it, which is the whole point of drawing one. */
export function toFlowEdges(graph: OurGraph, state: OurRunState | null): Edge[] {
  return graph.edges.map((edge, index) => {
    const decision = state?.decided?.[`${edge.from}|${edge.to}`];
    const selected = decision?.[0];
    const decided = decision !== undefined;
    const style = !decided ? { stroke: '#c8d4d0', strokeDasharray: '4 4' }
      : selected ? { stroke: '#2f7d5f', strokeWidth: 2.5 }
      : { stroke: '#d6d6d6', strokeDasharray: '4 4' };
    return {
      id: `e${index}-${edge.from}-${edge.to}`,
      source: edge.from,
      target: edge.to,
      type: 'routed',
      style,
      label: decided ? (selected ? '选中' : '拒绝') : undefined,
      animated: Boolean(selected) && state?.cursor?.node === edge.to,
    } as Edge;
  });
}

export type { XYPosition };
