import type { GraphCall, OurGraph } from './model';

/** Input mappings are JSON pointers, never expressions. The server remains the authority. */
export function previewCallInput(call: GraphCall, input: Record<string, unknown> = {}) {
  const result = { ...call.input };
  for (const [key, pointer] of Object.entries(call.input_map ?? {})) {
    if (!key || (pointer !== '' && !pointer.startsWith('/')) || /~(?:[^01]|$)/.test(pointer)) throw new Error('请填写目标参数和有效的 JSON Pointer，例如 /request/code。');
    let value: unknown = input;
    for (const raw of pointer === '' ? [] : pointer.slice(1).split('/')) {
      const part = raw.replace(/~1/g, '/').replace(/~0/g, '~');
      if (!value || typeof value !== 'object' || !Object.prototype.hasOwnProperty.call(value, part)) throw new Error(`默认输入未提供 ${pointer}；运行时必须提供此值。`);
      value = (value as Record<string, unknown>)[part];
    }
    Object.defineProperty(result, key, { value, enumerable: true, writable: true, configurable: true });
  }
  return result;
}

export function validateCallTargets(graph: OurGraph, targets: string[]) {
  for (const node of graph.nodes) {
    const call = graph.ops?.[node.op ?? '']?.call;
    if (call && !targets.includes(call.graph)) throw new Error(`调用节点 ${node.id} 的目标工作流不存在：${call.graph || '未选择'}`);
  }
}
