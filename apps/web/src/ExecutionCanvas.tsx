import { useEffect, useLayoutEffect, useRef, type RefObject } from 'react';
import {
  Background, Controls, Handle, ReactFlow, getViewportForBounds,
  Position, ReactFlowProvider, useNodesState, useNodesInitialized, useReactFlow, type Edge, type Node, type NodeProps,
} from '@xyflow/react';
import { Activity, AlertTriangle, Bot, CheckCircle2 } from 'lucide-react';
import { label } from './execution';
import { RoutedEdge } from './RoutedEdge';

export type ExecutionFlowNode = Node<{
  name: string;
  kind: string;
  state: string;
  detail: string;
  attempt?: number;
  statusLabel?: string;
}, 'execution'>;

function ExecutionNodeView({ data, selected }: NodeProps<ExecutionFlowNode>) {
  return <div className={`execution-node state-${data.state} ${selected ? 'selected' : ''}`}>
    <Handle type="target" position={Position.Left} />
    <div className="execution-node-kind"><Bot size={14} />{data.kind}<span>{data.attempt === undefined ? '' : `第 ${data.attempt + 1} 次执行`}</span></div>
    <strong title={data.name}>{data.name}</strong>
    <div className="execution-node-status">{data.state === 'completed' ? <CheckCircle2 size={14} /> : ['stalled', 'failed', 'revise', 'blocked'].includes(data.state) ? <AlertTriangle size={14} /> : <Activity size={14} />} {data.statusLabel || label(data.state)}</div>
    <small title={data.detail}>{data.detail || '尚无执行记录'}</small>
    <Handle type="source" position={Position.Right} />
  </div>;
}

const nodeTypes = { execution: ExecutionNodeView };
const edgeTypes = { routed: RoutedEdge };

function FitExecution({ container }: { container: RefObject<HTMLDivElement | null> }) {
  const flow = useReactFlow();
  const initialized = useNodesInitialized();
  useEffect(() => {
    const element = container.current;
    if (!element) return;
    let timer = 0;
    const fit = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        const area = element.getBoundingClientRect();
        const nodes = flow.getNodes();
        if (nodes.length && area.width > 0 && area.height > 0) void flow.setViewport(
          getViewportForBounds(flow.getNodesBounds(nodes), area.width, area.height, 0.1, 1, 0.15));
      }, 100);
    };
    const observer = new ResizeObserver(fit);
    observer.observe(element);
    fit();
    return () => { observer.disconnect(); window.clearTimeout(timer); };
  }, [container, flow, initialized]);
  return null;
}

type CanvasProps = {
  instanceKey: string;
  nodes: ExecutionFlowNode[];
  edges: Edge[];
  onSelectNode: (nodeId: string) => void;
};

export function ExecutionCanvas(props: CanvasProps) {
  return <ReactFlowProvider key={props.instanceKey}><MeasuredExecutionCanvas {...props} /></ReactFlowProvider>;
}

function MeasuredExecutionCanvas({ nodes, edges, onSelectNode }: CanvasProps) {
  const container = useRef<HTMLDivElement>(null);
  const [canvasNodes, setCanvasNodes, onNodesChange] = useNodesState(nodes);
  useLayoutEffect(() => {
    // Polling updates business data, not React Flow's measured geometry.
    // Dropping measured on each update also discards cached handle bounds.
    setCanvasNodes(previous => {
      const byId = new Map(previous.map(node => [node.id, node]));
      return nodes.map(node => ({ ...node, measured: byId.get(node.id)?.measured }));
    });
  }, [nodes, setCanvasNodes]);
  return <div ref={container} className="execution-canvas" data-testid="execution-canvas">
    <ReactFlow<ExecutionFlowNode>
      nodes={canvasNodes}
      onNodesChange={onNodesChange}
      edges={edges}
      nodeTypes={nodeTypes}
      edgeTypes={edgeTypes}
      nodesDraggable={false}
      nodesConnectable={false}
      deleteKeyCode={null}
      fitView
      fitViewOptions={{ padding: 0.15, maxZoom: 1 }}
      minZoom={0.1}
      onNodeClick={(_, value) => onSelectNode(value.id)}
    >
      <Background gap={22} color="#c8d4d0" />
      <Controls showInteractive={false} />
      <FitExecution container={container} />
    </ReactFlow>
  </div>;
}
