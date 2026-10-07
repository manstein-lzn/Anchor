/** The shape our backend has, and the shape the canvas wants.
 *
 * The canvas came from the previous system and its data model is deliberately generic — a node with
 * a name, a state and an attempt number, and an edge that is selected or not. What differs is where
 * those come from: a run's `decided` map instead of a table of edge decisions, and each node's
 * submission instead of a node_run row. This file is that translation and nothing else.
 */

import type { Edge, Node, XYPosition } from '@xyflow/react';
import type { Definition } from './graph';
import { label } from './execution';
import { isNodeActive, pairedNodes, parallelControl } from './parallel';

/** An agent: a model loop. `reads`/`writes` are the files it expects and the files it promises,
 *  which the loader checks against what the graph can actually hand it. */
export type OurAgent = {
  model: string;
  instructions?: string;
  network?: boolean;
  reads?: string[];
  writes?: string[];
};

/** An Op performs exactly one command, independent call, or same-Run parallel control. */
export type OurOp = {
  reads?: string[];
  writes?: string[];
  network?: boolean;
  wall_time_limit_seconds?: number;
} & (
  | { run: string; call?: never; fanout?: never; join?: never }
  | { run?: never; call: GraphCall; fanout?: never; join?: never }
  | { run?: never; call?: never; fanout: { join: string }; join?: never }
  | { run?: never; call?: never; fanout?: never; join: Record<string, never> }
);

export type GraphCall = {
  graph: string; mode: 'wait' | 'detach'; input?: Record<string, unknown>;
  input_map?: Record<string, string>; files?: { node: string; path: string; as: string }[];
  result?: { node: string; files?: string[] }; session?: string;
};
export type RunTrigger = { source: string; schedule?: string; scheduled_at?: string;
  graph?: string; run?: string; node?: string; invocation?: number; mode?: 'wait' | 'detach'; root_run?: string };
export type CallRecord = { node: string; invocation: number; graph: string; run: string;
  mode: 'wait' | 'detach'; status: string; active?: boolean; summary?: string; input?: Record<string, unknown>; result?: unknown };
export type GraphRelationsData = { graphs: { graph: string; schedules: number }[];
  calls: { graph: string; node: string; op: string; target: string; mode: 'wait' | 'detach' }[] };
export const callModeLabel = (mode: 'wait' | 'detach') => mode === 'wait' ? '等待完成' : '启动后继续';

export type OurGraph = {
  entry: string;
  objective?: string;
  input?: Record<string, unknown>;
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
  layout?: { positions?: Record<string, { x: number; y: number }>; edgeLabels?: Record<string, string> };
};

/** A node runs an agent or a graph, never both and never neither.
 *
 * `with` is what this use adds to what the role already says, which is what makes declaring a role
 * separately from its nodes worth doing: two nodes can share one and still be asked for different
 * things. It belongs to agents: an op has no instructions to add to, and the loader refuses it there.
 */
export type OurNode = { id: string; agent?: string; op?: string; graph?: string; with?: string; plugins?: string[] };

export type Plugin = {
  id: string; name: string; description: string; digest?: string;
  skills?: string[];
  unsupported?: string[];
  mcpServers?: Record<string, { transport: string; auth?: string }>;
  available?: boolean; error?: string; instructions?: string;
};

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
  trigger?: RunTrigger;
  input?: Record<string, unknown>;
  objective: string;
  started: string;
  status: string;
  updated: string;
  cursor: { node: string; pass: number; dir: string } | null;
  active?: Record<string, { node: string; pass: number; run?: number; dir: string }>;
  parallel?: { fanout: string; join: string; invocation: number; [key: string]: unknown } | null;
  passes: Record<string, number>;
  decided: Record<string, [boolean, number]>;
  nodes: Record<string, OurNodeResult>;
  executed: string[];
  skipped: string[];
  error: string;
  /** Why a run that stopped did not simply finish. `asked` means somebody pressed the button. */
  reason?: string;
  /** Exact io-harness tool attempts whose result was not recorded yet. */
  recovery?: RecoveryAttempt[];
};

export type RecoveryAttempt = {
  key: { run_id: string; graph_digest: string; node_id: string; invocation: number };
  attempt: { attempt_id: number; step: number; tool: string; started_at: string };
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
  trigger?: RunTrigger;
};

export type TimelineItem = { schedule: string; graph: string; scheduled_at: string; run?: string; status: string };
export type TimelineData = { runs: OurRun[]; scheduled: TimelineItem[]; schedules: Schedule[];
  capabilities?: { scheduling?: boolean } };
export type Schedule = { id: string; graph: string; rule: Record<string, unknown>; next_at: string; enabled: boolean; input?: Record<string, unknown> };

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
  tool_call_id?: string;
  status?: string;
  thinking?: boolean;
  contents?: unknown[];
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
  control_requested?: 'pause' | 'stop' | null;
  /** Whether this host currently owns a live execution task for the durable Run. */
  active?: boolean;
  calls?: CallRecord[];
  plugins?: Record<string, Plugin[]>;
  graph: string;
  run: string;
  state: OurRunState;
  traces: Record<string, TraceMessage[]>;
  nodes: string[];
};

export type FlowNode = Node<{
  name: string; kind: string; state: string; detail: string;
  plugins?: string[];
  call?: GraphCall;
  control?: 'fanout' | 'join';
  /** Which of the two it is, so a component can choose an icon without parsing the label. */
  nodeKind: 'agent' | 'op' | 'subgraph';
  attempt?: number; statusLabel?: string;
}, 'execution'>;

/** Only display metadata and topology go to the layout engine. */
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
    edges: graph.edges.map(edge => ({ source: edge.from, target: edge.to,
      label: graph.layout?.edgeLabels?.[`${edge.from}|${edge.to}`] })),
    entry_node_id: graph.entry,
  };
}

/** One node's state as of the most recent run, or as it was left by an earlier one. */
function nodeState(nodeId: string, graph: OurGraph, state: OurRunState | null): FlowNode {
  const definition = asDefinition(graph, nodeId);
  const node = definition.nodes.find(item => item.id === nodeId)!;
  const result = state?.nodes[nodeId];
  const passes = state?.passes[nodeId] ?? 0;
  const running = isNodeActive(state, nodeId);
  const skipped = state?.skipped?.includes(nodeId) ?? false;

  const status = running ? 'running'
    : result?.submitted ? 'completed'
    : result ? 'failed'
    : skipped ? 'skipped'
    : 'pending';

  const statusLabel = running ? label('running') : result ? label(result.exit_status || status) : undefined;
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
      nodeKind: kindOf(graph.nodes.find(item => item.id === nodeId)!),
      plugins: graph.nodes.find(item => item.id === nodeId)?.plugins,
      control: parallelControl(graph, nodeId),
      call: graph.ops?.[graph.nodes.find(item => item.id === nodeId)?.op ?? '']?.call,
      // The canvas renders `attempt + 1` as "第 N 次执行", so zero means the first pass.
      attempt: passes ? passes - 1 : undefined,
      statusLabel,
    },
  };
}

function nodeKindLabel(graph: OurGraph, nodeId: string): string {
  const node = graph.nodes.find(item => item.id === nodeId);
  if (!node) return '';
  if (node.graph) return '本次运行内执行';
  const control = parallelControl(graph, nodeId);
  if (control) return `${control === 'fanout' ? '收束于' : '展开自'} ${pairedNodes(graph, nodeId).join('、') || '未配对'}`;
  const call = graph.ops?.[node.op ?? '']?.call;
  if (call) return `${call.graph || '未选择目标'} · ${callModeLabel(call.mode)}`;
  if (node.op) return `${node.op} · op`;
  return `${node.agent} · agent`;
}

export function toFlowNodes(graph: OurGraph, name: string,
                            state: OurRunState | null): FlowNode[] {
  const spec = asDefinition(graph, name);
  return spec.nodes.map(node => nodeState(node.id, graph, state));
}

/** An edge carries what the run decided about it, which is the whole point of drawing one. */
export function toFlowEdges(graph: OurGraph, state: OurRunState | null): Edge[] {
  // `decided` stores the latest settlement for an edge. When a routed node has several exits,
  // the runner deliberately settles the exits it did not choose to false; that must not erase a
  // true traversal from an earlier feedback cycle. The execution sequence is the durable history
  // of which adjacent node transition actually happened.
  const traversed = new Set<string>();
  for (let index = 1; index < (state?.executed.length ?? 0); index += 1) {
    traversed.add(`${state!.executed[index - 1]}|${state!.executed[index]}`);
  }
  return graph.edges.map((edge, index) => {
    const decision = state?.decided?.[`${edge.from}|${edge.to}`];
    const selected = decision?.[0];
    const decided = decision !== undefined;
    const walked = traversed.has(`${edge.from}|${edge.to}`)
      // Ops can be scheduled between a source and its routed target, so the persisted decision
      // is the fallback evidence for a selected edge when adjacency is not visible in the list.
      || selected === true && decision?.[1] !== undefined;
    const style = walked ? { stroke: '#167565', strokeWidth: 3.5 }
      : !decided ? { stroke: '#b8cbc0', strokeWidth: 1.8, strokeDasharray: '6 5' }
      : selected ? { stroke: '#167565', strokeWidth: 3.5 }
      : { stroke: '#d1d8d4', strokeWidth: 1.5, strokeDasharray: '5 6' };
    return {
      id: `e${index}-${edge.from}-${edge.to}`,
      source: edge.from,
      target: edge.to,
      type: 'routed',
      style,
      animated: Boolean(selected) && isNodeActive(state, edge.to),
    } as Edge;
  });
}

export type { XYPosition };
