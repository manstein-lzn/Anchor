import type { XYPosition } from '@xyflow/react';
import dagre from '@dagrejs/dagre';

/** Only the topology and labels needed by the shared canvas layout. */
export interface Definition {
  graph_id: string;
  name: string;
  nodes: { id: string; type: 'agent' | 'op' | 'subgraph'; name: string }[];
  edges: { source: string; target: string }[];
  entry_node_id?: string;
}

export function isFeedbackEdge(source: XYPosition, target: XYPosition): boolean {
  return source.x > target.x + 20;
}

const NODE_WIDTH = 220;
const NODE_HEIGHT = 106;
const layoutCache = new Map<string, Map<string, XYPosition>>();

// Layered DAG layout (dagre) so a graph without saved drag positions never
// falls back to a naive grid with crossing, overlapping edges. Keyed by
// topology only, so polling and metadata edits cannot move the canvas.
export function layeredLayout(definition: Definition, width = NODE_WIDTH, height = NODE_HEIGHT): Map<string, XYPosition> {
  const key = JSON.stringify({
    id: definition.graph_id,
    size: [width, height],
    nodes: definition.nodes.map(node => node.id),
    edges: definition.edges.map(edge => `${edge.source}>${edge.target}`),
  });
  const cached = layoutCache.get(key);
  if (cached) return cached;
  const graph = new dagre.graphlib.Graph({ multigraph: true }).setDefaultEdgeLabel(() => ({}));
  graph.setGraph({ rankdir: 'LR', nodesep: 72, ranksep: 90, marginx: 48, marginy: 48 });
  for (const node of definition.nodes) {
    graph.setNode(node.id, { width, height });
  }
  for (const [index, edge] of definition.edges.entries()) {
    if (graph.hasNode(edge.source) && graph.hasNode(edge.target)) {
      // Dagre's multigraph keeps parallel and feedback constraints instead of collapsing them.
      graph.setEdge(edge.source, edge.target, {}, `${edge.source}>${edge.target}:${index}`);
    }
  }
  dagre.layout(graph);
  const positions = new Map<string, XYPosition>();
  for (const node of definition.nodes) {
    const laid = graph.node(node.id);
    if (laid) {
      positions.set(node.id, {
        x: Math.round(laid.x - width / 2),
        y: Math.round(laid.y - height / 2),
      });
    }
  }
  if (layoutCache.size > 50) layoutCache.clear();
  layoutCache.set(key, positions);
  return positions;
}
