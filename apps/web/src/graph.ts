import type { Edge, Node, XYPosition } from '@xyflow/react';

export const kinds = {
  agent: 'Agent', tool: '工具', router: '路由', parallel: '并行', join: '汇合',
  verifier: '验证', approval: '审批', wait_for_event: '等待事件',
  human_task: '人工任务', artifact: '产物', loop: '循环', subgraph: '子图',
} as const;
export type Kind = keyof typeof kinds;
export interface NodeSpec {
  id: string; type: Kind; name: string;
  agent_ref?: string | null; tool_ref?: string | null; verifier_ref?: string | null;
  subgraph_version_id?: string | null;
  exit_condition?: string | null; progress_signal?: string | null;
  approval_required?: boolean;
  [key: string]: unknown;
}
export interface EdgeSpec {
  source: string; target: string; condition?: string | null;
  input_mapping?: Record<string, string>; [key: string]: unknown;
}
export interface Definition {
  graph_id: string; name: string; nodes: NodeSpec[]; edges: EdgeSpec[];
  entry_node_id?: string | null; [key: string]: unknown;
}
export interface Layout {
  positions?: Record<string, XYPosition>; [key: string]: unknown;
}
export interface Document { definition: Definition; layout: Layout }
export interface Draft extends Document { graph_id: string; revision: number; updated_at: string }
export interface Version {
  graph_version_id: string; graph_id: string; version: number;
  definition: Definition; content_hash: string; published_at: string;
}
export type CanvasNode = Node<{ spec: NodeSpec; entry: boolean }, 'anchor'>;

const object = (value: unknown): value is Record<string, unknown> =>
  !!value && typeof value === 'object' && !Array.isArray(value);

// Drafts may be semantically incomplete, but must be representable without loss.
export function parseDocument(value: unknown): Document {
  if (!object(value)) throw new Error('Graph JSON 必须为对象');
  const envelope = 'definition' in value;
  const definition = envelope ? value.definition : value;
  if (!object(definition) || typeof definition.graph_id !== 'string' ||
    typeof definition.name !== 'string' || !Array.isArray(definition.nodes) ||
    (definition.edges !== undefined && !Array.isArray(definition.edges))) {
    throw new Error('Graph 需要 graph_id、name、nodes 和 edges 字段');
  }
  const ids = new Set<string>();
  for (const node of definition.nodes) {
    if (!object(node) || typeof node.id !== 'string' || !/^[A-Za-z][A-Za-z0-9_-]{0,63}$/.test(node.id) ||
      typeof node.name !== 'string' || typeof node.type !== 'string' || !Object.hasOwn(kinds, node.type) || ids.has(node.id)) {
      throw new Error('节点需要唯一合法 ID、名称和支持的类型');
    }
    ids.add(node.id);
  }
  for (const edge of (definition.edges ?? []) as unknown[]) {
    if (!object(edge) || typeof edge.source !== 'string' || typeof edge.target !== 'string') {
      throw new Error('连线需要 source 和 target');
    }
  }
  const layout = envelope ? value.layout ?? {} : {};
  if (!object(layout)) throw new Error('layout 必须为对象');
  if (layout.positions !== undefined) {
    if (!object(layout.positions)) throw new Error('positions 必须为对象');
    for (const position of Object.values(layout.positions)) {
      if (!object(position) || typeof position.x !== 'number' || typeof position.y !== 'number' ||
        !Number.isFinite(position.x) || !Number.isFinite(position.y)) throw new Error('节点坐标必须为有限数值');
    }
  }
  return structuredClone({ definition: { ...definition, edges: definition.edges ?? [] }, layout }) as Document;
}

export function project(doc: Document, selected: string | null, readOnly: boolean) {
  const nodes: CanvasNode[] = doc.definition.nodes.map((spec, index) => ({
    id: spec.id, type: 'anchor', width: 220, height: 106,
    position: doc.layout.positions?.[spec.id] ?? { x: 80 + (index % 3) * 280, y: 90 + Math.floor(index / 3) * 170 },
    data: { spec, entry: doc.definition.entry_node_id === spec.id },
    selected: selected === `node:${spec.id}`, draggable: !readOnly,
  }));
  const edges: Edge[] = doc.definition.edges.map((edge, index) => ({
    id: `edge:${index}`, source: edge.source, target: edge.target,
    label: edge.condition ? '条件' : undefined,
    ariaLabel: `${edge.source} -> ${edge.target}${edge.condition ? `: ${edge.condition}` : ''}`,
    type: 'smoothstep', selected: selected === `edge:${index}`,
  }));
  return { nodes, edges };
}

export function removeSelection(doc: Document, selection: string): Document {
  if (selection.startsWith('edge:')) return { ...doc, definition: { ...doc.definition,
    edges: doc.definition.edges.filter((_, index) => `edge:${index}` !== selection) } };
  const id = selection.slice(5);
  const positions = { ...doc.layout.positions };
  delete positions[id];
  return { ...doc, layout: { ...doc.layout, positions }, definition: { ...doc.definition,
    nodes: doc.definition.nodes.filter(node => node.id !== id),
    edges: doc.definition.edges.filter(edge => edge.source !== id && edge.target !== id),
    entry_node_id: doc.definition.entry_node_id === id ? null : doc.definition.entry_node_id,
  } };
}

export function emptyDocument(): Document {
  return { definition: { graph_id: `graph-${crypto.randomUUID().slice(0, 8)}`, name: '未命名工作流', nodes: [], edges: [] }, layout: {} };
}

export function fingerprint(doc: Document): string { return JSON.stringify(doc); }

export function downloadDocument(doc: Document) {
  const url = URL.createObjectURL(new Blob([JSON.stringify(doc, null, 2)], { type: 'application/json' }));
  const link = document.createElement('a');
  link.href = url;
  link.download = `${doc.definition.graph_id}.json`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
