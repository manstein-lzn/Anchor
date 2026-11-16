import type { GraphSummary } from './model';

/** Flatten valid task lineages without hiding an ambiguous or unavailable source. */
export function groupGraphTasks(graphs: GraphSummary[]) {
  const byId = new Map(graphs.map(item => [item.graph, item]));
  const roots: GraphSummary[] = [];
  const children = new Map<string, GraphSummary[]>();
  for (const item of graphs) {
    const visited = new Set([item.graph]);
    let source = item;
    while (source.task_source) {
      const parent = byId.get(source.task_source);
      if (!parent || visited.has(parent.graph)) { source = item; break; }
      visited.add(parent.graph);
      source = parent;
    }
    if (source === item) roots.push(item);
    else {
      const tasks = children.get(source.graph) ?? [];
      tasks.push(item);
      children.set(source.graph, tasks);
    }
  }
  return { roots, children };
}
