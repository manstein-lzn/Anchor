import { AvoidLib } from 'libavoid-js';
import { NODE_HEIGHT, NODE_WIDTH, routeLabel, type Definition, type Layout, type Route } from './graph';

// ELK Layered owns automatic placement. libavoid routes around fixed, user-positioned rectangles.
let loaded: Promise<void> | undefined;
// The package's declarations omit Embind's delete() on these value objects.
function dispose(value: unknown) { (value as { delete(): void }).delete(); }
export async function routeFixedLayout(definition: Definition, positions: Layout['positions'],
                                      ports: Layout['ports'], routes: Route[]): Promise<Route[]> {
  loaded ??= AvoidLib.load(typeof window === 'undefined' ? undefined
    : new URL('../node_modules/libavoid-js/dist/libavoid.wasm', import.meta.url).href)
    .catch(error => { loaded = undefined; throw error; });
  await loaded;
  const avoid = AvoidLib.getInstance();
  const router = new avoid.Router(2); // libavoid OrthogonalRouting flag
  try {
    router.setRoutingParameter(avoid.RoutingParameter.shapeBufferDistance, 10);
    router.setRoutingParameter(avoid.RoutingParameter.idealNudgingDistance, 18);
    router.setRoutingParameter(avoid.RoutingParameter.crossingPenalty, 100);
    router.setRoutingOption(avoid.RoutingOption.nudgeOrthogonalSegmentsConnectedToShapes, true);
    router.setRoutingOption(avoid.RoutingOption.nudgeSharedPathsWithCommonEndPoint, true);
    const shapes = new Map<string, InstanceType<typeof avoid.ShapeRef>>();
    for (const node of definition.nodes) {
      const { x, y } = positions[node.id];
      const top = new avoid.Point(x, y), bottom = new avoid.Point(x + NODE_WIDTH, y + NODE_HEIGHT);
      const rect = new avoid.Rectangle(top, bottom);
      const shape = new avoid.ShapeRef(router, rect);
      shapes.set(node.id, shape);
      ports[node.id].forEach((port, index) => {
        const pin = new avoid.ShapeConnectionPin(shape, index + 1, port.x / NODE_WIDTH, port.y / NODE_HEIGHT,
          true, 0, port.side === 'EAST' ? 8 : port.side === 'NORTH' ? 1 : 2);
        pin.setExclusive(true);
      });
      dispose(rect); dispose(top); dispose(bottom);
    }
    const connections = definition.edges.map((edge, index) => {
      const source = new avoid.ConnEnd(shapes.get(edge.source)!, ports[edge.source].findIndex(p => p.id === `e${index}-out`) + 1);
      const target = new avoid.ConnEnd(shapes.get(edge.target)!, ports[edge.target].findIndex(p => p.id === `e${index}-in`) + 1);
      const connection = new avoid.ConnRef(router, source, target);
      dispose(source); dispose(target);
      return connection;
    });
    router.processTransaction();
    return connections.map((connection, index) => {
      const line = connection.displayRoute();
      const points = Array.from({ length: line.size() }, (_, i) => {
        const point = line.at(i);
        return { x: point.x, y: point.y };
      });
      if (points.length < 2) throw new Error('无法为当前节点位置生成连线，请尝试自动整理。');
      return { ...routes[index], points, labelPosition: routeLabel(points) };
    });
  } finally { router.delete(); }
}
