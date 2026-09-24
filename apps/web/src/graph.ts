import ELK from 'elkjs/lib/elk.bundled.js';
import type { XYPosition, Rect } from '@xyflow/react';

export interface Definition {
  graph_id: string;
  name: string;
  nodes: { id: string; type: 'agent' | 'op' | 'subgraph'; name: string }[];
  edges: { source: string; target: string; label?: string }[];
  entry_node_id?: string;
}

export const NODE_WIDTH = 200;
export const NODE_HEIGHT = 108;
export type Port = XYPosition & { id: string; type: 'source' | 'target'; side: 'EAST' | 'WEST' | 'SOUTH' };
export type Route = { points: XYPosition[]; feedback: boolean; label?: string; labelPosition?: XYPosition };
export type Layout = {
  positions: Record<string, XYPosition>;
  ports: Record<string, Port[]>;
  routes: Route[];
  bounds: Rect;
};

// Match the scheduler's entry-first DFS cycle classification, independent of screen coordinates.
export function feedbackEdges(definition: Definition): Set<number> {
  const outgoing = new Map(definition.nodes.map(node => [node.id, [] as number[]]));
  definition.edges.forEach((edge, index) => outgoing.get(edge.source)?.push(index));
  const colours = new Map<string, number>();
  const back = new Set<number>();
  for (const start of [definition.entry_node_id, ...outgoing.keys()]) {
    if (!start || colours.has(start)) continue;
    colours.set(start, 1);
    const stack = [{ id: start, next: 0 }];
    while (stack.length) {
      const head = stack[stack.length - 1];
      const index = outgoing.get(head.id)?.[head.next++];
      if (index === undefined) { colours.set(head.id, 2); stack.pop(); continue; }
      const target = definition.edges[index].target;
      if (colours.get(target) === 1) back.add(index);
      else if (!colours.has(target)) { colours.set(target, 1); stack.push({ id: target, next: 0 }); }
    }
  }
  return back;
}

export function routeLabel(points: XYPosition[]): XYPosition {
  let longest = -1;
  let centre = points[0];
  for (let index = 1; index < points.length; index++) {
    const a = points[index - 1], b = points[index];
    const length = Math.abs(a.x - b.x) + Math.abs(a.y - b.y);
    if (length > longest) { longest = length; centre = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }; }
  }
  return centre;
}

export function layoutBounds(positions: Layout['positions'], routes: Route[]): Rect {
  const points = Object.values(positions).flatMap(p => [p, { x: p.x + NODE_WIDTH, y: p.y + NODE_HEIGHT }]);
  for (const route of routes) {
    points.push(...route.points);
    if (route.label && route.labelPosition) {
      const { x, y } = route.labelPosition, half = labelWidth(route.label) / 2;
      points.push({ x: x - half, y: y - 12 }, { x: x + half, y: y + 12 });
    }
  }
  if (!points.length) return { x: 0, y: 0, width: 1, height: 1 };
  const x = Math.min(...points.map(p => p.x)) - 24, y = Math.min(...points.map(p => p.y)) - 24;
  return { x, y, width: Math.max(...points.map(p => p.x)) - x + 24,
    height: Math.max(...points.map(p => p.y)) - y + 24 };
}

function labelWidth(text: string) { return Array.from(text).reduce((n, c) => n + (c.charCodeAt(0) > 255 ? 12 : 7), 16); }
const elk = new ELK();
const cache = new Map<string, Promise<Layout>>();

export function layoutWorkflow(definition: Definition, saved: Layout['positions'] = {}): Promise<Layout> {
  const key = JSON.stringify([definition, saved]);
  const previous = cache.get(key);
  if (previous) return previous;
  const pending = calculateLayout(definition, saved).catch(error => { cache.delete(key); throw error; });
  if (cache.size > 30) cache.clear();
  cache.set(key, pending);
  return pending;
}

async function calculateLayout(definition: Definition, saved: Layout['positions']): Promise<Layout> {
  const back = feedbackEdges(definition);
  const ports: Layout['ports'] = Object.fromEntries(definition.nodes.map(n => [n.id, []]));
  definition.edges.forEach((edge, index) => {
    if (!ports[edge.source] || !ports[edge.target]) throw new Error('连线引用了不存在的节点，请检查连线起点和终点。');
    ports[edge.source].push({ id: `e${index}-out`, type: 'source', side: back.has(index) ? 'SOUTH' : 'EAST', x: 0, y: 0 });
    ports[edge.target].push({ id: `e${index}-in`, type: 'target', side: back.has(index) ? 'SOUTH' : 'WEST', x: 0, y: 0 });
  });
  const result = await elk.layout({
    id: 'workflow',
    layoutOptions: {
      'elk.algorithm': 'layered', 'elk.direction': 'RIGHT', 'elk.edgeRouting': 'ORTHOGONAL',
      'elk.layered.mergeEdges': 'false', 'elk.layered.feedbackEdges': 'true',
      'elk.spacing.nodeNode': '48', 'elk.spacing.edgeNode': '24', 'elk.spacing.edgeEdge': '18',
      'elk.layered.spacing.nodeNodeBetweenLayers': '64', 'elk.layered.spacing.edgeEdgeBetweenLayers': '18',
      'elk.layered.spacing.edgeNodeBetweenLayers': '24',
    },
    children: definition.nodes.map(node => ({
      id: node.id, width: NODE_WIDTH, height: NODE_HEIGHT,
      layoutOptions: { 'elk.portConstraints': 'FIXED_SIDE' },
      ports: ports[node.id].map(port => ({ id: port.id, width: 0, height: 0,
        layoutOptions: { 'elk.port.side': port.side } })),
    })),
    edges: definition.edges.map((edge, index) => ({
      id: `e${index}`, sources: [`e${index}-out`], targets: [`e${index}-in`],
      layoutOptions: { 'elk.layered.priority.direction': back.has(index) ? '0' : '100' },
      labels: edge.label ? [{ id: `label${index}`, text: edge.label, width: labelWidth(edge.label), height: 24 }] : [],
    })),
  });
  const positions: Layout['positions'] = {};
  for (const node of result.children ?? []) {
    positions[node.id] = { x: node.x!, y: node.y! };
    for (const port of node.ports ?? []) Object.assign(ports[node.id].find(p => p.id === port.id)!, { x: port.x!, y: port.y! });
  }
  let routes: Route[] = (result.edges ?? []).map((edge, index) => {
    const section = edge.sections?.[0];
    if (!section) throw new Error('布局未返回完整连线路径。');
    const points = [section.startPoint, ...section.bendPoints ?? [], section.endPoint];
    const label = edge.labels?.[0];
    return { points, feedback: back.has(index), label: definition.edges[index].label,
      labelPosition: label ? { x: label.x! + label.width! / 2, y: label.y! + label.height! / 2 } : undefined };
  });
  let moved = false;
  for (const [id, point] of Object.entries(saved)) {
    if (!positions[id]) continue;
    if (!Number.isFinite(point.x) || !Number.isFinite(point.y)) throw new Error('节点坐标必须为有限数值。');
    moved ||= positions[id].x !== point.x || positions[id].y !== point.y;
    positions[id] = point;
  }
  if (moved) {
    const { routeFixedLayout } = await import('./manualRouting');
    routes = await routeFixedLayout(definition, positions, ports, routes);
  }
  return { positions, ports, routes, bounds: layoutBounds(positions, routes) };
}
