import { useEffect, useMemo, useRef, useState } from 'react';
import { api, reason } from './api';
import { createAgent, defaultModelReference } from './agentAuthoring';
import { validateCallTargets } from './calls';
import { addParallelRegion } from './parallel';
import type { GraphPluginActivation, GraphSummary, NodePluginRef, OurGraph, OurOp } from './model';

export type GraphPick = { kind: 'node' | 'edge' | 'agent'; id: string } | null;
export type WorkbenchNotice = { kind: 'ok' | 'bad'; text: string } | null;
const serialize = (doc: OurGraph) => JSON.stringify(doc, null, 2) + '\n';

export const graphSaveNotice = (activation?: GraphPluginActivation, checked = false) => activation?.mode === 'next_turn'
  ? `${checked ? '校验通过。' : ''}已保存，插件配置将在各会话的下一条消息生效。`
  : checked ? '校验通过，工作流已保存。' : '已保存。';

/** Owns the current editing snapshot and its history, independent of visible page. */
export function useGraphEditor(name: string, graphs: GraphSummary[], setNotice: (notice: WorkbenchNotice) => void) {
  const [loaded, setLoaded] = useState('');
  const [document, setDoc] = useState<OurGraph | null>(null);
  const [activation, setActivation] = useState<GraphPluginActivation>();
  const doc = loaded === name ? document : null;
  const docRef = useRef(doc); docRef.current = doc;
  const pluginActivation = loaded === name ? activation : undefined;
  const [savedDoc, setSavedDoc] = useState('');
  const [past, setPast] = useState<OurGraph[]>([]);
  const [future, setFuture] = useState<OurGraph[]>([]);
  const [pick, setPick] = useState<GraphPick>(null);
  const pendingPick = useRef<{ graph: string; node: string } | null>(null);
  const revision = useRef(0);
  const noticeRef = useRef(setNotice); noticeRef.current = setNotice;
  const dirty = doc !== null && serialize(doc) !== savedDoc;

  useEffect(() => {
    revision.current += 1;
    setDoc(null); setLoaded(''); setActivation(undefined); setSavedDoc(''); setPast([]); setFuture([]); setPick(null);
    if (!name) return;
    const controller = new AbortController();
    void api<{ definition: OurGraph; plugin_activation?: GraphPluginActivation }>(`/graphs/${encodeURIComponent(name)}`, 'GET', undefined, controller.signal)
      .then(value => {
        if (controller.signal.aborted) return;
        setLoaded(name); setDoc(value.definition); setActivation(value.plugin_activation); setSavedDoc(serialize(value.definition));
        const pending = pendingPick.current;
        setPick(pending?.graph === name && pending.node ? { kind: 'node', id: pending.node } : null);
        pendingPick.current = null; noticeRef.current(null);
      }).catch(() => { if (!controller.signal.aborted) setDoc(null); });
    return () => { revision.current += 1; controller.abort(); };
  }, [name]);
  useEffect(() => {
    if (!dirty) return;
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ''; };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty]);

  const patch = (next: OurGraph) => {
    if (!doc) return;
    docRef.current = next;
    setPast(history => [...history, doc]); setFuture([]); setDoc(next); setNotice(null);
  };
  const undo = () => {
    const [last, ...rest] = [...past].reverse();
    if (!last || !doc) return;
    setFuture([doc, ...future]); setPast(rest.reverse()); setDoc(last);
  };
  const redo = () => {
    const [first, ...rest] = future;
    if (!first || !doc) return;
    setPast([...past, doc]); setFuture(rest); setDoc(first);
  };
  const focusNode = (graph: string, node = '') => {
    pendingPick.current = graph === name ? null : { graph, node };
    setPick(node ? { kind: 'node', id: node } : null);
  };
  const save = async () => {
    if (!doc) return;
    const current = revision.current;
    validateCallTargets(doc, graphs.map(item => item.graph));
    const saved = await api<{ plugin_activation?: GraphPluginActivation }>(`/graphs/${encodeURIComponent(name)}`, 'PUT', { definition: doc });
    if (current === revision.current) { setSavedDoc(serialize(doc)); setActivation(saved.plugin_activation); }
    return saved.plugin_activation;
  };
  const selectedNode = pick?.kind === 'node' ? doc?.nodes.find(item => item.id === pick.id) : undefined;
  const selectedOp = selectedNode?.op ? doc?.ops?.[selectedNode.op] : undefined;
  const patchOperation = (op: OurOp) => {
    if (doc && selectedNode?.op) patch({ ...doc, ops: { ...doc.ops, [selectedNode.op]: op } });
  };
  const selectedAgent = pick?.kind === 'agent' && doc ? doc.agents?.[pick.id] : undefined;
  const selectedEdge = pick?.kind === 'edge' && doc ? doc.edges[Number(pick.id)] : undefined;

  // A node's `plugins` may be written as bare ids or as `{id, required, default}`;
  // the editor works on ids and preserves whatever declaration each keeps.
  const pluginRefs: NodePluginRef[] = selectedNode?.plugins ?? [];
  const selectedPlugins = pluginRefs.map(item => typeof item === 'string' ? item : item.id);
  const setPlugins = (ids: string[]) => patchNode({
    plugins: ids.map(id => {
      const prior = pluginRefs.find(item => (typeof item === 'string' ? item : item.id) === id);
      return prior ?? id;
    }),
  });
  const patchNode = (fields: Partial<{ id: string; agent: string; with: string; plugins: NodePluginRef[] }>) => {
    if (!doc || !selectedNode) return;
    patch({ ...doc, nodes: doc.nodes.map(item =>
      item.id === selectedNode.id ? { ...item, ...fields } : item) });
  };
  const patchAgent = (fields: Partial<{ model: string; network: boolean; instructions: string }>) => {
    if (!doc || pick?.kind !== 'agent') return;
    const current = doc.agents?.[pick.id] ?? { model: '' };
    patch({ ...doc, agents: { ...doc.agents, [pick.id]: { ...current, ...fields } } });
  };
  const patchEdge = (fields: Partial<{ from: string; to: string }>) => {
    if (!doc || pick?.kind !== 'edge') return;
    const at = Number(pick.id);
    patch({ ...doc, edges: doc.edges.map((item, index) =>
      index === at ? { ...item, ...fields } : item) });
  };

  const addNode = (agentName: string) => {
    if (!doc) return;
    let id = 'node';
    let suffix = 2;
    while (doc.nodes.some(item => item.id === id)) id = `node${suffix++}`;
    patch({ ...doc, nodes: [...doc.nodes, { id, agent: agentName }] });

    setPick({ kind: 'node', id });
  };

  const addOperation = (calling: boolean) => {
    if (!doc) return;
    let id = calling ? 'call' : 'command';
    let suffix = 2;
    while (doc.nodes.some(item => item.id === id) || doc.ops?.[id]) id = `${calling ? 'call' : 'command'}${suffix++}`;
    patch({ ...doc, entry: doc.entry || id, nodes: [...doc.nodes, { id, op: id }], ops: { ...doc.ops,
      [id]: calling ? { call: { graph: graphs.find(item => item.graph !== name)?.graph ?? '', mode: 'wait' } } : { run: 'true' } } });
    setPick({ kind: 'node', id });
  };

  const addParallel = async () => {
    if (!doc) return;
    const current = revision.current;
    try {
      const model = Object.values(doc.agents ?? {})[0]?.model ?? await defaultModelReference();
      if (current !== revision.current) return;
      if (docRef.current !== doc) { setNotice({ kind: 'bad', text: '工作流已更新，请重新添加并行区域。' }); return; }
      const added = addParallelRegion(doc, model);
      patch(added.graph); setPick({ kind: 'node', id: added.fanout });
    } catch (error) { if (current === revision.current) setNotice({ kind: 'bad', text: reason(error) }); }
  };

  const addAgent = async () => {
    if (!doc) return;
    const current = revision.current;
    try {
      const model = await defaultModelReference();
      if (current !== revision.current) return;
      if (docRef.current !== doc) { setNotice({ kind: 'bad', text: '工作流已更新，请重新添加角色。' }); return; }
      let id = 'agent';
      let suffix = 2;
      while (doc.agents?.[id]) id = `agent${suffix++}`;
      patch({ ...doc, agents: { ...doc.agents, [id]: createAgent(model) } });
      setPick({ kind: 'agent', id });
    } catch (error) { if (current === revision.current) setNotice({ kind: 'bad', text: reason(error) }); }
  };

  const duplicateNode = () => {
    if (!doc || !selectedNode) return;
    let id = `${selectedNode.id}-copy`;
    let suffix = 2;
    while (doc.nodes.some(item => item.id === id)) id = `${selectedNode.id}-copy${suffix++}`;
    const at = doc.layout?.positions?.[selectedNode.id];
    patch({ ...doc,
      nodes: [...doc.nodes, { ...selectedNode, id }],
      layout: at ? { ...doc.layout,
        positions: { ...doc.layout?.positions, [id]: { x: at.x + 45, y: at.y + 140 } } } : doc.layout });
    setPick({ kind: 'node', id });
  };

  const removePicked = () => {
    if (!doc || !pick) return;
    if (pick.kind === 'node') {
      patch({ ...doc, nodes: doc.nodes.filter(item => item.id !== pick.id),
              edges: doc.edges.filter(edge => edge.from !== pick.id && edge.to !== pick.id) });
    } else if (pick.kind === 'edge') {
      patch({ ...doc, edges: doc.edges.filter((_, index) => index !== Number(pick.id)) });
    } else {
      const used = doc.nodes.some(item => item.agent === pick.id);
      if (used) { setNotice({ kind: 'bad', text: '还有节点在用这个角色，先改掉它们。' }); return; }
      const agents = { ...doc.agents };
      delete agents[pick.id];
      patch({ ...doc, agents });
    }
    setPick(null);
  };

  const entry = doc?.entry ?? '';
  const routing = useMemo(() => {
    if (!doc) return new Set<string>();
    const counts: Record<string, number> = {};
    for (const edge of doc.edges) counts[edge.from] = (counts[edge.from] ?? 0) + 1;
    return new Set(Object.keys(counts).filter(id => counts[id] > 1));
  }, [doc]);

  return { doc, dirty, pluginActivation, pick, setPick, patch, undo, redo, canUndo: Boolean(past.length), canRedo: Boolean(future.length),
    focusNode, save, selectedNode, selectedOp, selectedAgent, selectedEdge, selectedPlugins, setPlugins,
    patchNode, patchAgent, patchEdge, patchOperation, addNode, addOperation, addParallel, addAgent, duplicateNode,
    removePicked, entry, routing };
}
export type GraphEditor = ReturnType<typeof useGraphEditor>;
