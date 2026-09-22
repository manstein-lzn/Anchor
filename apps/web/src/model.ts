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

/** An agent: a model loop. `reads`/`writes` are the files it expects and the files it promises,
 *  which the loader checks against what the graph can actually hand it. */
export type OurAgent = {
  model: string;
  instructions?: string;
  network?: boolean;
  reads?: string[];
  writes?: string[];
};

/** An op: one command and no model at all, whose **exit code is the verdict**. Everything else about
 *  it is what it is for an agent — the same sandbox, the same workspace, a commit per pass, the same
 *  `reads`/`writes` — so the canvas draws it as a node like any other and only says which kind it is. */
export type OurOp = {
  run: string;
  reads?: string[];
  writes?: string[];
  network?: boolean;
  wall_time_limit_seconds?: number;
};

export type OurGraph = {
  entry: string;
  objective?: string;
  max_rounds?: number;
  /** A file declares agents, ops, or both. A graph of nothing but ops has no `agents` key at all,
   *  and one of nothing but agents has no `ops` — so neither is required. */
  agents?: Record<string, OurAgent>;
  ops?: Record<string, OurOp>;
  /** Graphs this file declares, which a node may run instead of an agent. The canvas shows one as a
   *  module; a run never sees this shape — it reads the expansion, where every module is inlined. */
  graphs?: Record<string, unknown>;
  nodes: OurNode[];
  edges: { from: string; to: string }[];
  /** Where nodes were dragged to. Beside the definition, not part of it: a node's position is not
   *  something the graph means, and a run must not be affected by it. */
  layout?: { positions?: Record<string, { x: number; y: number }> };
};

/** A node runs an agent or a graph, never both and never neither.
 *
 * `with` is what this use adds to what the role already says, which is what makes declaring a role
 * separately from its nodes worth doing: two nodes can share one and still be asked for different
 * things. It belongs to agents: an op has no instructions to add to, and the loader refuses it there.
 */
export type OurNode = { id: string; agent?: string; op?: string; graph?: string; with?: string };

/** Which of the three a node is as written. A module is the third: it disappears when the graph is
 *  expanded, so a run only ever sees agents and ops. */
export function kindOf(node: OurNode): 'agent' | 'op' | 'subgraph' {
  return node.graph ? 'subgraph' : node.op ? 'op' : 'agent';
}

export type OurNodeResult = {
  node_id: string;
  pass_number: number;
  submission: string;
  files: string[];
  submitted: boolean;
  exit_status: string;
  route: string | null;
  /** This pass, frozen. The workspace is reused across passes, so the commit is what makes one pass
   *  readable after a later one has written over it — and what a downstream node reads the history
   *  through. */
  commit?: string;
  /** What this pass was handed, as `[node, commit]`. The pointers are pinned, so this is what makes
   *  "what did this node read" answerable rather than reconstructable. */
  inputs?: [string, string][];
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
  /** Why a run that stopped did not simply finish. `asked` means somebody pressed the button. */
  reason?: string;
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

export type TraceMessage = {
  role: string;
  text: string;
  /** What the node ran. The projection carries these whole, not only the tool's name: a command is
   *  the most informative thing in a trace and the view has nothing to show without it. */
  commands?: string[];
  /** Whether the text was cut before it got here, so a reader is never shown part of a result
   *  believing it is all of it. */
  truncated?: boolean;
  exit_status?: string;
};

/** One file in a node's workspace, as the listing reports it. */
export type OurFile = { path: string; size: number };
export type OurFileList = { node: string; files: OurFile[]; truncated: boolean };

/** One file's contents. `binary` is the difference between "this is the text" and "download it": a
 *  mangled decode would be worse than saying so. */
export type OurFileBody = {
  path: string;
  size: number;
  binary: boolean;
  text: string;
  truncated: boolean;
};

export type OurRunDetail = {
  graph: string;
  run: string;
  state: OurRunState;
  traces: Record<string, TraceMessage[]>;
  nodes: string[];
};

export type FlowNode = Node<{
  name: string; kind: string; state: string; detail: string;
  /** Which of the two it is, so a component can choose an icon without parsing the label. */
  nodeKind: 'agent' | 'op';
  attempt?: number; statusLabel?: string;
}, 'execution'>;

/** Our graph.json in the shape `layeredLayout` expects. */
export function asDefinition(graph: OurGraph, name: string): Definition {
  return {
    graph_id: name,
    name,
    nodes: graph.nodes.map(node => ({
      id: node.id,
      type: kindOf(node),
      // A module is named for the graph it runs, so the canvas says which module rather than
      // printing the node's own id twice.
      name: node.graph ? `${node.id} · ${node.graph}` : node.id,
    })),
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
      // Which definition it points at, and which kind that is. An op and an agent are the same thing
      // to everything the canvas draws — a node with a workspace and a state — so the difference is
      // said in words rather than drawn as a different shape.
      kind: nodeKindLabel(graph, nodeId),
      state: status,
      detail,
      nodeKind: graph.nodes.find(item => item.id === nodeId)?.op ? 'op' : 'agent',
      // The canvas renders `attempt + 1` as "第 N 次执行", so zero means the first pass.
      attempt: passes ? passes - 1 : undefined,
      statusLabel,
    },
  };
}

function nodeKindLabel(graph: OurGraph, nodeId: string): string {
  const node = graph.nodes.find(item => item.id === nodeId);
  if (!node) return '';
  if (node.graph) return `${node.graph} · 模块`;
  if (node.op) return `${node.op} · op`;
  return `${node.agent} · agent`;
}

export function toFlowNodes(graph: OurGraph, name: string,
                            state: OurRunState | null): FlowNode[] {
  const spec = asDefinition(graph, name);
  // Stored positions win, so a run's picture is laid out the way its author arranged it rather than
  // the way dagre would. Falling back to dagre covers a graph nobody has dragged yet.
  const auto = layeredLayout(spec);
  return spec.nodes.map(node => ({
    ...nodeState(node.id, graph, state),
    position: graph.layout?.positions?.[node.id] ?? auto.get(node.id) ?? { x: 0, y: 0 },
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
      animated: Boolean(selected) && state?.cursor?.node === edge.to,
    } as Edge;
  });
}

export type { XYPosition };
