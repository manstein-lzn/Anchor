/** The editing canvas.
 *
 * Separate from `ExecutionCanvas`, which shows a run and deliberately cannot be touched. This one is
 * the document: nodes are draggable and their positions are kept, dragging from a node's right
 * handle to another node's left handle makes an edge, and clicking selects. `ExecutionCanvas` sets
 * `nodesDraggable={false} nodesConnectable={false}`, so using it for editing gave a canvas that
 * showed the graph and would not let anyone change it.
 *
 * Positions live in the graph itself, under `layout`, beside the definition rather than inside it —
 * where a node sits is not part of what the graph means, and a run must not be affected by it.
 */

import {
  Background, Controls, Handle, Position, ReactFlow, ReactFlowProvider,
  useNodesState, useReactFlow, type Connection, type Edge, type Node, type NodeProps,
} from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { useEffect, useMemo, useRef } from 'react';
import { Bot, Terminal } from 'lucide-react';
import { RoutedEdge } from './RoutedEdge';
import { layeredLayout } from './graph';
import { kindOf, type OurGraph } from './model';

export type AnchorNode = Node<{ label: string; agent: string; entry: boolean;
                                  kind: 'agent' | 'op' | 'subgraph' }, 'anchor'>;

function AnchorNodeView({ data, selected }: NodeProps<AnchorNode>) {
  return <div className={`graph-node ${selected ? 'selected' : ''}`}>
    <Handle type="target" position={Position.Left} />
    <div className="node-kind">
      {/* Three shapes, because there are three as written. An op is a command and no model, so a
          picture that said only "node" would be saying the one thing that is not true of it. */}
      {data.kind === 'op' ? <Terminal size={15} /> : <Bot size={15} />}
      <span>{data.kind === 'subgraph' ? '模块' : data.kind === 'op' ? 'Op' : '节点'}</span>
      {data.entry && <span className="entry-tag">入口</span>}
    </div>
    <strong>{data.label}</strong>
    <span className="node-reference">{data.agent}</span>
    <Handle type="source" position={Position.Right} />
  </div>;
}

const nodeTypes = { anchor: AnchorNodeView };
// Stable identity: React Flow re-registers edge types when this object changes, which drops edges
// during a drag's high-frequency re-renders.
const edgeTypes = { routed: RoutedEdge };

function Inner({ graph, name, editable, onMove, onConnect, onPick }:
  { graph: OurGraph; name: string; editable: boolean;
    onMove: (id: string, position: { x: number; y: number }) => void;
    onConnect: (from: string, to: string) => void;
    onPick: (pick: { kind: 'node' | 'edge'; id: string } | null) => void }) {

  const laid = useMemo(() => layeredLayout({
    graph_id: name, name,
    nodes: graph.nodes.map(item => ({
      id: item.id,
      type: kindOf(item),
      name: item.graph ? `${item.id} · ${item.graph}` : item.id,
    })),
    edges: graph.edges.map(item => ({ source: item.from, target: item.to })),
  }), [graph, name]);

  const nodes: AnchorNode[] = useMemo(() => graph.nodes.map(item => ({
    id: item.id,
    type: 'anchor',
    // A declared position wins, so a node stays where it was put. Without this the drag would move
    // it and the next render would put it straight back.
    position: graph.layout?.positions?.[item.id] ?? laid.get(item.id) ?? { x: 0, y: 0 },
    data: { label: item.graph ? `${item.id} · ${item.graph}` : item.id,
            agent: item.graph ?? item.op ?? item.agent ?? '',
            entry: item.id === graph.entry,
            kind: kindOf(item) },
  })), [graph, laid]);

  const edges: Edge[] = useMemo(() => graph.edges.map((item, index) => ({
    id: `e${index}`, source: item.from, target: item.to, type: 'routed',
  })), [graph]);

  const [flowNodes, setFlowNodes, onNodesChange] = useNodesState(nodes);
  useEffect(() => {
    setFlowNodes(previous => {
      const measured = new Map(previous.map(node => [node.id, node.measured]));
      return nodes.map(node => ({ ...node, measured: measured.get(node.id) }));
    });
  }, [nodes, setFlowNodes]);

  const flow = useReactFlow();
  const fitted = useRef('');
  useEffect(() => {
    const key = `${name}:${graph.nodes.length}`;
    if (fitted.current === key) return;
    fitted.current = key;
    window.setTimeout(() => void flow.fitView({ padding: 0.25, maxZoom: 1 }), 60);
  }, [name, graph.nodes.length, flow]);

  const handleConnect = (connection: Connection) => {
    if (!editable || !connection.source || !connection.target) return;
    onConnect(connection.source, connection.target);
  };

  return <div className="canvas" data-testid="graph-canvas">
    <ReactFlow<AnchorNode>
      nodes={flowNodes}
      edges={edges}
      onNodesChange={onNodesChange}
      nodeTypes={nodeTypes}
      edgeTypes={edgeTypes}
      nodesDraggable={editable}
      nodesConnectable={editable}
      edgesReconnectable={false}
      deleteKeyCode={null}
      multiSelectionKeyCode={null}
      selectionKeyCode={null}
      proOptions={{ hideAttribution: true }}
      onConnect={handleConnect}
      onNodeDragStop={(_, node) => onMove(node.id, node.position)}
      onNodeClick={(_, node) => onPick({ kind: 'node', id: node.id })}
      onEdgeClick={(_, edge) => onPick({ kind: 'edge', id: edge.id.replace(/^e/, '') })}
      onPaneClick={() => onPick(null)}
      minZoom={0.15}
    >
      <Background gap={20} color="#dfe6e3" />
      <Controls showInteractive={false} />
    </ReactFlow>
  </div>;
}

export function GraphCanvas(props: Parameters<typeof Inner>[0]) {
  return <ReactFlowProvider><Inner {...props} /></ReactFlowProvider>;
}
