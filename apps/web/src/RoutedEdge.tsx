import { BaseEdge, EdgeLabelRenderer, type EdgeProps } from '@xyflow/react';
import type { Route } from './graph';

// Geometry is owned by the whole-graph router; never re-route one edge in isolation here.
export function RoutedEdge({ id, data, markerEnd, style, selected }: EdgeProps) {
  const route = data as Route | undefined;
  if (!route?.points.length) return null;
  const path = route.points.map((p, i) => `${i ? 'L' : 'M'} ${p.x} ${p.y}`).join(' ');
  return <g data-testid={`edge-${id}`} data-feedback={route.feedback}>
    <BaseEdge id={id} path={path} markerEnd={markerEnd} interactionWidth={16}
      style={{ ...style, ...(selected ? { stroke: '#205bc1', strokeWidth: 3 } : {}), strokeLinejoin: 'round' }} />
    {route.label && route.labelPosition && <EdgeLabelRenderer>
      <span className="workflow-edge-label nodrag nopan" style={{
        transform: `translate(-50%, -50%) translate(${route.labelPosition.x}px, ${route.labelPosition.y}px)`,
      }}>{route.label}</span>
    </EdgeLabelRenderer>}
  </g>;
}
