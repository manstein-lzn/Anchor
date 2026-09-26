import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Background, Controls, Handle, MarkerType, Panel, Position, ReactFlow, ReactFlowProvider,
  getViewportForBounds, useNodesState, useReactFlow, useUpdateNodeInternals,
  type Edge, type Node, type NodeProps,
} from '@xyflow/react';
import { Bot, Layers, Terminal } from 'lucide-react';
import { asDefinition, toFlowEdges, toFlowNodes, type OurGraph, type OurRunState } from './model';
import { layoutWorkflow, NODE_HEIGHT, NODE_WIDTH, type Layout, type Port } from './graph';
import { RoutedEdge } from './RoutedEdge';

type WorkflowNode = Node<{
  name: string; kind: string; nodeKind: 'agent' | 'op' | 'subgraph'; entry: boolean; terminal: boolean;
  state: string; statusLabel?: string; attempt?: number; ports: Port[]; editing: boolean;
  plugins?: string[];
}, 'workflow'>;

function WorkflowNodeView({ id, data, selected }: NodeProps<WorkflowNode>) {
  const update = useUpdateNodeInternals();
  useEffect(() => { update(id); }, [id, data.ports, update]);
  const Icon = data.nodeKind === 'op' ? Terminal : data.nodeKind === 'subgraph' ? Layers : Bot;
  return <div className={`workflow-node ${data.editing ? 'graph-node' : 'execution-node'} kind-${data.nodeKind} state-${data.state} ${selected ? 'selected' : ''}`}>
    <div className="workflow-node-heading"><Icon size={14} />
      <span>{data.nodeKind === 'op' ? '命令' : data.nodeKind === 'subgraph' ? '子图' : '智能体'}</span>
      {data.entry ? <span className="entry-tag">入口</span> : data.terminal && <span className="entry-tag">终点</span>}
      {!!data.plugins?.length && <span className="entry-tag" title={data.plugins.join(', ')}
        aria-label={`挂载 ${data.plugins.length} 个 Plugin`}>Plugin {data.plugins.length}</span>}
    </div>
    <strong title={data.name}>{data.name}</strong>
    <div className="workflow-node-footer" title={data.kind}>
      {data.editing ? data.kind : <><span>{data.statusLabel || ({ running: '执行中', completed: '已完成', pending: '尚未执行', failed: '失败', skipped: '未选中' }[data.state] ?? data.state)}</span>
        {data.attempt !== undefined && <span>第 {data.attempt + 1} 次</span>}</>}
    </div>
    {data.ports.map(port => <Handle key={port.id} id={port.id} type={port.type}
      position={port.side === 'SOUTH' ? Position.Bottom : port.side === 'NORTH' ? Position.Top : Position.Right}
      isConnectable={false} className="route-port"
      style={{ left: port.x, top: port.y, right: 'auto', bottom: 'auto', transform: 'translate(-50%, -50%)' }} />)}
    {data.editing && <>
      <Handle id="connect-in" type="target" position={Position.Top} className="connect-port" title="连接输入" />
      <Handle id="connect-out" type="source" position={Position.Bottom} className="connect-port" title="连接输出" />
    </>}
  </div>;
}

const nodeTypes = { workflow: WorkflowNodeView };
const edgeTypes = { routed: RoutedEdge };
type Pick = { kind: 'node' | 'edge'; id: string } | null;
type Props = {
  graph: OurGraph; name: string; mode: 'edit' | 'run'; state?: OurRunState | null; editable?: boolean;
  onPick: (pick: Pick) => void;
  onPositions?: (positions: Layout['positions']) => void;
  onConnect?: (source: string, target: string) => void;
};

export function WorkflowCanvas(props: Props) {
  return <ReactFlowProvider key={props.name}><Canvas {...props} /></ReactFlowProvider>;
}

function Canvas({ graph, name, mode, state = null, editable = false, onPick, onPositions, onConnect }: Props) {
  const editing = mode === 'edit';
  // Only topology, labels and saved positions invalidate geometry. Polling cannot relayout it.
  const definitionKey = JSON.stringify(asDefinition(graph, name));
  const positionsKey = JSON.stringify(graph.layout?.positions ?? {});
  const [geometry, setGeometry] = useState<{ key: string; layout: Layout } | null>(null);
  const [problem, setProblem] = useState('');
  const [arranging, setArranging] = useState(false);
  const [dragging, setDragging] = useState(false);
  const key = definitionKey + positionsKey;
  const currentKey = useRef(key); currentKey.current = key;
  const layout = geometry?.key === key ? geometry.layout : null;
  const container = useRef<HTMLDivElement>(null);
  const flow = useReactFlow<WorkflowNode>();
  useEffect(() => {
    let active = true;
    setProblem('');
    void layoutWorkflow(JSON.parse(definitionKey), JSON.parse(positionsKey)).then(result => {
      if (active) setGeometry({ key: definitionKey + positionsKey, layout: result });
    }).catch(error => { if (active) setProblem(String(error.message ?? error)); });
    return () => { active = false; };
  }, [definitionKey, positionsKey]);

  const nodes = useMemo<WorkflowNode[]>(() => !layout ? [] : toFlowNodes(graph, name, state).map(node => ({
    id: node.id, type: 'workflow', position: layout.positions[node.id], width: NODE_WIDTH, height: NODE_HEIGHT,
    data: { ...node.data, entry: node.id === graph.entry, terminal: !graph.edges.some(edge => edge.from === node.id),
      editing, ports: layout.ports[node.id] },
  })), [graph, name, state, editing, layout]);
  const [canvasNodes, setCanvasNodes, onNodesChange] = useNodesState(nodes);
  useEffect(() => { if (!dragging) setCanvasNodes(nodes); }, [nodes, dragging, setCanvasNodes]);
  const edges = useMemo<Edge[]>(() => !layout ? [] : toFlowEdges(graph, state).map((edge, index) => {
    const route = layout.routes[index];
    const style = !editing && state ? edge.style : { stroke: route.feedback ? '#98704a' : '#568571', strokeWidth: 1.8 };
    return { ...edge, sourceHandle: `e${index}-out`, targetHandle: `e${index}-in`, style,
      markerEnd: { type: MarkerType.ArrowClosed, color: style?.stroke, width: 16, height: 16 },
      data: { ...route, index }, hidden: dragging };
  }), [graph, state, editing, layout, dragging]);
  const fit = useCallback(() => {
    if (!layout || !container.current) return;
    const { width, height } = container.current.getBoundingClientRect();
    if (width && height) void flow.setViewport(getViewportForBounds(layout.bounds, width, height, 0.02, 1, 0.12));
  }, [layout, flow]);
  useEffect(() => {
    const element = container.current;
    if (!element) return;
    let frame = 0;
    const resized = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(fit); };
    const observer = new ResizeObserver(resized);
    observer.observe(element);
    resized();
    return () => { observer.disconnect(); cancelAnimationFrame(frame); };
  }, [fit]);
  const arrange = async () => {
    setArranging(true); setProblem('');
    try {
      const result = await layoutWorkflow(JSON.parse(definitionKey));
      if (currentKey.current === key) onPositions?.(result.positions);
    }
    catch (error) { setProblem(error instanceof Error ? error.message : String(error)); }
    finally { setArranging(false); }
  };
  return <div className={`workflow-canvas ${editing ? 'canvas' : 'execution-canvas'}`} ref={container}
    data-testid={editing ? 'graph-canvas' : 'execution-canvas'} aria-busy={!layout && !problem}>
    <ReactFlow<WorkflowNode> nodes={canvasNodes} edges={edges} nodeTypes={nodeTypes} edgeTypes={edgeTypes}
      onNodesChange={onNodesChange} nodesDraggable={editing && editable && !!layout}
      nodesConnectable={editing && editable} edgesReconnectable={false} deleteKeyCode={null}
      minZoom={0.02} maxZoom={2} proOptions={{ hideAttribution: true }}
      onConnect={connection => { if (editable && connection.source && connection.target) onConnect?.(connection.source, connection.target); }}
      onNodeDragStart={() => setDragging(true)}
      onNodeDragStop={(_, node) => {
        if (layout) onPositions?.({ ...layout.positions, [node.id]: node.position });
        setDragging(false);
      }}
      onNodeClick={(_, node) => onPick({ kind: 'node', id: node.id })}
      onEdgeClick={(_, edge) => { if (editing) onPick({ kind: 'edge', id: String(edge.data?.index) }); }}
      onPaneClick={() => onPick(null)}>
      <Background gap={24} color="#dce5df" />
      <Controls showInteractive={false} onFitView={fit} />
      <Panel position="top-left" className="workflow-legend">
        {editing ? <><span><i />前向连接</span><span><i className="feedback" />循环返回</span></>
          : <><span><i className="walked" />已走过</span><span><i className="unwalked" />未走过</span><span>侧边连接为循环返回</span></>}
      </Panel>
      {editing && <Panel position="top-right"><button className="arrange-button" disabled={!editable || arranging}
        onClick={() => void arrange()}>自动整理</button></Panel>}
    </ReactFlow>
    {!layout && !problem && <div className="layout-message" role="status">正在整理布局…</div>}
    {problem && <div className="layout-message" role="alert">布局失败：{problem}</div>}
  </div>;
}
