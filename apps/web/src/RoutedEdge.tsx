import {
  BaseEdge, getBezierPath, getSmoothStepPath, Position, type EdgeProps,
} from '@xyflow/react';

// A backwards edge (target left of its source) is routed below the graph in two
// orthogonal segments so it never sweeps across unrelated nodes. Forward edges
// stay as bezier curves whose curvature is varied per fan-out by `project()`.
const HANDLE_OFFSET = 20;
const BACK_EDGE_DROP = 80;
const BACK_EDGE_OFFSET = 30;
const BORDER_RADIUS = 16;

export function RoutedEdge({
  id, sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition,
  markerEnd, style, pathOptions,
}: EdgeProps) {
  const backward = sourceX - HANDLE_OFFSET > targetX;
  const interaction = 26;

  if (backward) {
    const bendX = (sourceX + targetX) / 2;
    const bendY = Math.max(sourceY, targetY) + BACK_EDGE_DROP;
    const [first] = getSmoothStepPath({
      sourceX, sourceY, sourcePosition, targetX: bendX, targetY: bendY,
      targetPosition: Position.Right, borderRadius: BORDER_RADIUS, offset: BACK_EDGE_OFFSET,
    });
    const [second] = getSmoothStepPath({
      sourceX: bendX, sourceY: bendY, sourcePosition: Position.Left,
      targetX, targetY, targetPosition, borderRadius: BORDER_RADIUS, offset: BACK_EDGE_OFFSET,
    });
    return <g data-testid={`edge-${id}`}>
      <BaseEdge id={id} path={first} style={style} interactionWidth={interaction} />
      <BaseEdge id={`${id}-return`} path={second} style={style} markerEnd={markerEnd}
        interactionWidth={interaction} />
    </g>;
  }

  const curvature = (pathOptions as { curvature?: number } | undefined)?.curvature ?? 0.3;
  const [path] = getBezierPath({
    sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition, curvature,
  });
  return <g data-testid={`edge-${id}`}>
    <BaseEdge id={id} path={path} style={style} markerEnd={markerEnd} interactionWidth={interaction} />
  </g>;
}
