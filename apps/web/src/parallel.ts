import type { OurGraph, OurRunState } from './model';

export function activeNodes(state: OurRunState | null | undefined) {
  const active = { ...state?.active };
  for (const branch of state?.parallel?.branches ?? []) {
    if (branch.status === 'running' && branch.cursor) {
      const { node_id: node, key } = branch.cursor;
      active[node] ??= { node, pass: key.invocation, dir: '' };
    }
  }
  if (state?.cursor && !active[state.cursor.node]) active[state.cursor.node] = state.cursor;
  return Object.values(active);
}

export function isNodeActive(state: OurRunState | null | undefined, node: string) {
  return state?.status === 'running' && activeNodes(state).some(item => item.node === node);
}

/** New keys encode identity explicitly; old serial Run keys remain readable. */
export function tracePassKeys(traces: Record<string, unknown> | undefined, node: string): string[] {
  const invocation = (key: string): number | undefined => {
    try {
      const value: unknown = JSON.parse(key);
      if (Array.isArray(value) && value.length === 2 && value[0] === node
          && Number.isInteger(value[1]) && value[1] > 0) return value[1];
    } catch { /* Legacy keys are plain node IDs. */ }
    if (key === node) return 1;
    if (key.startsWith(`${node}-`) && /^\d+$/.test(key.slice(node.length + 1))) return Number(key.slice(node.length + 1));
    return undefined;
  };
  return Object.keys(traces ?? {}).filter(key => invocation(key) !== undefined)
    .sort((a, b) => invocation(a)! - invocation(b)!);
}

export function parallelControl(graph: OurGraph, node: string): 'fanout' | 'join' | undefined {
  const op = graph.ops?.[graph.nodes.find(item => item.id === node)?.op ?? ''];
  return op?.fanout ? 'fanout' : op?.join ? 'join' : undefined;
}

export function pairedNodes(graph: OurGraph, node: string): string[] {
  const op = graph.ops?.[graph.nodes.find(item => item.id === node)?.op ?? ''];
  if (op?.fanout) return [op.fanout.join];
  if (op?.join) return graph.nodes.filter(item => graph.ops?.[item.op ?? '']?.fanout?.join === node).map(item => item.id);
  return [];
}

/** Insert a complete region before the existing entry, preserving its downstream topology. */
export function addParallelRegion(graph: OurGraph): { graph: OurGraph; fanout: string } {
  const used = new Set([...graph.nodes.map(node => node.id), ...Object.keys(graph.ops ?? {}), ...Object.keys(graph.agents ?? {})]);
  const unique = (base: string) => {
    let id = base, suffix = 2;
    while (used.has(id)) id = `${base}${suffix++}`;
    used.add(id);
    return id;
  };
  const fanout = unique('fanout'), join = unique('join');
  const left = unique('branch-a'), right = unique('branch-b');
  const role = Object.keys(graph.agents ?? {})[0] ?? unique('parallel-agent');
  const downstream = graph.nodes.some(node => node.id === graph.entry) ? graph.entry : unique('synthesize');
  return { fanout, graph: { ...graph, entry: fanout,
    agents: graph.agents?.[role] ? graph.agents : { ...graph.agents, [role]: { model: 'models.academic', network: false, instructions: '' } },
    ops: { ...graph.ops, [fanout]: { fanout: { join } }, [join]: { join: {} } },
    nodes: [...graph.nodes, { id: fanout, op: fanout }, { id: left, agent: role }, { id: right, agent: role }, { id: join, op: join },
      ...(!graph.nodes.some(node => node.id === downstream) ? [{ id: downstream, agent: role }] : [])],
    edges: [...graph.edges, { from: fanout, to: left }, { from: fanout, to: right },
      { from: left, to: join }, { from: right, to: join }, { from: join, to: downstream }],
  } };
}

export function renameGraphNode(graph: OurGraph, previous: string, next: string): OurGraph {
  return { ...graph, entry: graph.entry === previous ? next : graph.entry,
    nodes: graph.nodes.map(node => node.id === previous ? { ...node, id: next } : node),
    edges: graph.edges.map(edge => ({ from: edge.from === previous ? next : edge.from, to: edge.to === previous ? next : edge.to })),
    ops: graph.ops && Object.fromEntries(Object.entries(graph.ops).map(([id, op]) => [id,
      op.fanout && op.fanout.join === previous ? { ...op, fanout: { join: next } } : op])),
    layout: graph.layout && { ...graph.layout,
      positions: graph.layout.positions && Object.fromEntries(Object.entries(graph.layout.positions).map(([id, position]) => [id === previous ? next : id, position])),
      edgeLabels: graph.layout.edgeLabels && Object.fromEntries(Object.entries(graph.layout.edgeLabels).map(([key, value]) => [key.split('|').map(id => id === previous ? next : id).join('|'), value])),
    },
  };
}
